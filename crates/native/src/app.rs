//! Starting the app: the themes (t3code's two, the bundled Zed families, the user's folder,
//! watched while the app runs), the remembered layout, the key bindings, and the one window with
//! its [`Workspace`]. `hermes-native.exe` calls [`run`]; the spike (CP0.1) is still reachable with
//! `--spike`.
//!
//! The window has no system frame: `appears_transparent` makes GPUI strip it and the frame draws
//! its own title bar ([`crate::titlebar`]); `window_min_size` is [`crate::ui_state::MIN_WINDOW`].
//! Position and size come from `native-ui.json` ([`crate::workspace::restore_window`], which puts
//! the window on a screen that exists) and are written back as the window moves and resizes.
//! DPI changes need no code of ours: GPUI handles `WM_DPICHANGED` (it resizes the window to the
//! rectangle Windows suggests and re-renders at the new scale factor) and everything is drawn in
//! logical pixels. The theme follows the system's light or dark until the user picks one
//! (`Ctrl+Alt+Shift+A`, the sidebar's palette button, the palette).

use std::time::Duration;

use gpui_kit::{
    AnyView, App, AppContext as _, Bounds, Pixels, TitlebarOptions, WindowAppearance,
    WindowOptions, component::Root, px, size,
};

use crate::{
    design::{AppAssets, Appearance},
    keymap, palette,
    theme::{self, DesignTheme as _, Themes},
    ui_state::{MIN_WINDOW, UiState},
    workspace::{Workspace, restore_window},
};

/// What the command line asked for.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Quit by itself after this long (measurements and tests of the real window).
    pub quit_after: Option<Duration>,
    /// A theme name to wear instead of the remembered one (`--theme "One Dark"`).
    pub theme: Option<String>,
    /// Where `--dump` writes the element dump (needs `--features dev`).
    pub dump: Option<std::path::PathBuf>,
}

fn appearance_of(window: WindowAppearance) -> Appearance {
    match window {
        WindowAppearance::Dark => Appearance::Dark,
        _ => Appearance::Light,
    }
}

/// Opens the app and runs until the window closes.
pub fn run(options: Options) {
    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            #[cfg(feature = "dev")]
            crate::dev::install(cx);
            keymap::bind(cx);
            palette::bind(cx);

            let data_dir = theme::folder::data_dir();
            let state = data_dir.as_deref().map(UiState::load).unwrap_or_default();

            // The user's themes folder: read now, watched while we run.
            match &data_dir {
                Some(dir) => {
                    let (themes, scan) = Themes::load(dir);
                    for problem in &scan.problems {
                        tracing::warn!("theme file {} ignored: {}", problem.file, problem.reason);
                    }
                    themes.install(cx);
                    theme::watch_user_themes(theme::folder::themes_dir(dir), scan.fingerprint, cx);
                }
                None => Themes::new(Vec::new()).install(cx),
            }

            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let displays: Vec<Bounds<Pixels>> = cx.displays().iter().map(|d| d.bounds()).collect();
            let bounds = restore_window(&state.window, &displays);
            let wanted_theme = options.theme.clone().or_else(|| state.theme.clone());
            let opened = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(bounds),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Hermes".into()),
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    window_min_size: Some(size(px(MIN_WINDOW.0), px(MIN_WINDOW.1))),
                    ..Default::default()
                },
                cx,
                move |window, cx| {
                    // The remembered theme, else the system's light or dark.
                    let themes = Themes::global(cx);
                    let def = wanted_theme
                        .as_deref()
                        .and_then(|name| themes.find(name))
                        .unwrap_or_else(|| themes.default_for(appearance_of(window.appearance())))
                        .clone();
                    theme::apply(&def, cx);

                    let workspace = cx.new(|cx| Workspace::new(state, data_dir, window, cx));
                    workspace.update(cx, |w, cx| w.focus(window, cx));
                    // Follow the system's light and dark while no theme was picked.
                    let follow = workspace.clone();
                    window
                        .observe_window_appearance(move |window, cx| {
                            if follow.read(cx).state().theme.is_none() {
                                let themes = Themes::global(cx);
                                let def = themes
                                    .default_for(appearance_of(window.appearance()))
                                    .clone();
                                if def.name != cx.design_theme().name {
                                    theme::apply(&def, cx);
                                    window.refresh();
                                }
                            }
                        })
                        .detach();
                    let saving = workspace.clone();
                    window.on_window_should_close(cx, move |_, cx| {
                        saving.read(cx).save_now();
                        true
                    });
                    let view: AnyView = workspace.into();
                    cx.new(|cx| Root::new(view, window, cx))
                },
            );
            match opened {
                Ok((window, _)) => crate::dev::start_dump(options.dump.clone(), window, cx),
                Err(error) => {
                    eprintln!("could not open the window: {error:#}");
                    cx.quit();
                    return;
                }
            }
            if let Some(after) = options.quit_after {
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(after).await;
                    cx.update(|cx: &mut App| cx.quit());
                })
                .detach();
            }
        });
}
