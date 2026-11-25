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

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{
    AnyView, App, AppContext as _, Bounds, Pixels, TitlebarOptions, WindowAppearance,
    WindowOptions, component::Root, px, size,
};

use crate::{
    design::{AppAssets, Appearance},
    keymap,
    model::{AppModel, CoreBackend, FixtureBackend, fixture},
    palette, sidebar,
    theme::{self, DesignTheme as _, Themes},
    thread::recording::Recording,
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
    /// `--fixture`: fake projects and threads, no core, no lock (tests, screenshots, a look).
    pub fixture: bool,
    /// `--dev` (DEV ONLY): enables `--dev-send` and, with `--fixture`, makes a prompt sent from
    /// the composer replay the recorded turn (`crates/native/fixtures/thread-session.json`).
    /// (Before Phase 5's composer it added a prompt strip under the thread.)
    pub dev: bool,
    /// `--dev-provider <url>`: the prompts go to this OpenAI-compatible endpoint (a
    /// mock provider such as `scripts/perf/mock_llm.py`'s `http://127.0.0.1:47900/v1`) instead of
    /// the thread's own provider.
    pub dev_provider: Option<String>,
    /// `--dev-send <prompt>` (with `--dev`): open the first thread, send the prompt, print the
    /// streamed and the stored reply, quit ([`crate::dev_send`]: the Phase 4 live check).
    pub dev_send: Option<String>,
    /// `--dev-answer <once|session|always|deny>` (with `--dev-send`): answer the turn's approvals
    /// with this choice and its questions with their first choice, from the composer's model, and
    /// write the `--dump` (`<file>.approval`, `<file>.question`) while each panel is on screen
    /// ([`crate::dev_send`]: the Phase 5 live check). No notification is made in this mode.
    pub dev_answer: Option<String>,
}

fn appearance_of(window: WindowAppearance) -> Appearance {
    match window {
        WindowAppearance::Dark => Appearance::Dark,
        _ => Appearance::Light,
    }
}

/// Says why the app does not start, in a message box, and exits (nothing was opened yet).
fn stop(level: rfd::MessageLevel, message: &str) -> ! {
    tracing::warn!("not starting: {message}");
    rfd::MessageDialog::new()
        .set_level(level)
        .set_title("Hermes")
        .set_description(message)
        .show();
    std::process::exit(1)
}

/// Opens the app and runs until the window closes.
///
/// With the real core (the default): takes `<data>/app.lock` first (a second app on the folder
/// says "Hermes is already open" and exits, as the desktop app does; the folder is the same
/// `HERMES_RS_HOME` / `%APPDATA%\hermes-rs`), starts a tokio runtime for the core, opens the
/// window at once and builds the core on a background thread (`Core::new` seeds the bundled
/// skills on a first run), then `Core::start(true)`: Settings on the runtime, the auto-archive
/// sweep and the scheduled curator. `background = true` because this app is the one app on the
/// folder (the lock), exactly as the desktop app owns the curator's schedule; only the ACP agent,
/// which runs beside another app, passes `false`.
pub fn run(options: Options) {
    let data_dir = theme::folder::data_dir();
    let _lock = if options.fixture {
        None
    } else {
        let Some(dir) = &data_dir else {
            stop(
                rfd::MessageLevel::Error,
                "Hermes could not find its data folder.",
            );
        };
        match hermes_core::AppLock::acquire(dir) {
            Ok(lock) => Some(lock),
            Err(error) => stop(rfd::MessageLevel::Info, &error.to_string()),
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("hermes-runtime")
        .enable_all()
        .build()
        .expect("a tokio runtime");
    let handle = runtime.handle().clone();
    // The terminals' shells: the core backend starts them; whatever is still running when the
    // window closes is ended below, with everything it started (no orphans).
    let terminals = Arc::new(hermes_core::terminal::Terminals::new(env!(
        "CARGO_PKG_VERSION"
    )));
    let shells = terminals.clone();
    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            #[cfg(feature = "dev")]
            crate::dev::install(cx);
            keymap::bind(cx);
            palette::bind(cx);
            crate::panel::files::bind(cx);
            crate::panel::finder::bind(cx);
            sidebar::keys::bind(cx);
            crate::thread::header::bind(cx);
            crate::thread::composer::view::bind(cx);
            // After the global bindings: the terminal takes some of them back for the shell.
            crate::terminal::bind(cx);

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
            let (dev_send, send_dump) = (options.dev_send.clone(), options.dump.clone());
            let driven = options.dev && options.dev_send.is_some();
            let answer_with = options.dev_answer.clone();
            let quitting = shells.clone();
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

                    let model = if options.fixture {
                        let snapshot = fixture::snapshot();
                        let backend = FixtureBackend::new(snapshot.clone());
                        if options.dev {
                            backend.replay_on_submit(Recording::fixture());
                        }
                        cx.new(|cx| {
                            AppModel::with_snapshot(backend, snapshot, cx)
                                .with_clock(|| fixture::NOW_MS)
                        })
                    } else {
                        let model = cx.new(|_| AppModel::starting());
                        start_core(
                            model.clone(),
                            handle.clone(),
                            options.dev_provider.clone(),
                            shells.clone(),
                            cx,
                        );
                        model
                    };
                    let dev = options.dev;
                    if let Some(prompt) = dev_send.clone().filter(|_| dev) {
                        crate::dev_send::start(
                            model.clone(),
                            window.window_handle(),
                            prompt,
                            send_dump.clone(),
                            answer_with.clone(),
                            cx,
                        );
                    }
                    let workspace =
                        cx.new(|cx| Workspace::with_model(state, data_dir, model, window, cx));
                    workspace.update(cx, |w, cx| w.focus(window, cx));
                    // A waiting thread flashes the taskbar and shows a notification (not in a
                    // fixture, and not in the live check that answers by itself).
                    #[cfg(windows)]
                    if !options.fixture && options.dev_answer.is_none() {
                        let (clicked, clicks) = std::sync::mpsc::channel();
                        let notifier = std::rc::Rc::new(crate::notify::OsNotifier::new(clicked));
                        workspace.update(cx, |w, cx| {
                            w.set_notifier(notifier, Some(clicks), window, cx)
                        });
                    }
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
                // With `--dev-send` the dump is written when its turn has ended.
                Ok((window, _)) if !driven => {
                    crate::dev::start_dump(options.dump.clone(), window, cx)
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("could not open the window: {error:#}");
                    cx.quit();
                    return;
                }
            }
            cx.on_app_quit(move |_| {
                quitting.close_all();
                async {}
            })
            .detach();
            if let Some(after) = options.quit_after {
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(after).await;
                    cx.update(|cx: &mut App| cx.quit());
                })
                .detach();
            }
        });
    // When the event loop returns (the window closed): dropping a tokio runtime waits without end
    // for its blocking tasks (a curator or review call in flight), which would leave a windowless
    // process holding the app lock. Give them a moment, then go; the lock is released after.
    runtime.shutdown_timeout(Duration::from_secs(2));
    terminals.close_all();
}

/// Builds the core off the main thread and gives it to the model.
fn start_core(
    model: gpui_kit::Entity<AppModel>,
    handle: tokio::runtime::Handle,
    dev_provider: Option<String>,
    terminals: Arc<hermes_core::terminal::Terminals>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        let built = cx
            .background_spawn(async move {
                let config = hermes_runtime::RuntimeConfig::for_current_user()?;
                let core = hermes_core::Core::new(config, handle)?;
                core.start(true);
                Ok::<_, anyhow::Error>(core)
            })
            .await;
        let _ = model.update(cx, |model, cx| match built {
            Ok(core) => model.connect(
                Arc::new(
                    CoreBackend::new(core)
                        .with_dev_provider(dev_provider)
                        .with_terminals(terminals),
                ),
                cx,
            ),
            Err(error) => model.fail(format!("{error:#}"), cx),
        });
    })
    .detach();
}
