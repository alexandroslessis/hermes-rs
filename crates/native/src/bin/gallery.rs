//! `hermes-native-gallery`: every component and screen of the native UI as a story, with fake
//! data, in both themes (`crates/native/src/gallery/`). Polishing happens here, not in the app.
//!
//! Flags:
//! - `--theme <dark|light|one-dark|one-light|t3-dark|t3-light>`: the theme to start with (default `light`).
//! - `--quit-after <seconds>`: quit on its own after that long.
//! - `--dump <file>` (with `--features dev`): write the element dump once the window has drawn.
//!   With `--features dev`, F12 writes it to `layout-dump.txt` in `HERMES_RS_HOME`.

use std::time::Duration;

use gpui_kit::*;
use hermes_native::{
    gallery::GalleryView,
    theme::{ThemeChoice, apply_theme},
};

fn main() {
    hermes_native::log_to_stderr();
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|ix| args.get(ix + 1))
            .cloned()
    };
    let theme = match flag("--theme").as_deref() {
        Some("dark") => ThemeChoice::DefaultDark,
        Some("one-dark") => ThemeChoice::OneDark,
        Some("t3-dark") => ThemeChoice::T3Dark,
        Some("t3-light") => ThemeChoice::T3Light,
        Some("one-light") => ThemeChoice::OneLight,
        _ => ThemeChoice::DefaultLight,
    };
    // `from_secs_f64` panics on a negative, NaN or huge value; such a flag is ignored.
    let quit_after = flag("--quit-after")
        .and_then(|secs| secs.parse::<f64>().ok())
        .and_then(|secs| Duration::try_from_secs_f64(secs).ok());
    let dump = hermes_native::dev::dump_flag(&args);

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        #[cfg(feature = "dev")]
        hermes_native::dev::install(cx);
        apply_theme(theme, cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
        let opened = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Hermes native gallery".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            cx,
            |_, cx| cx.new(|_| GalleryView::new(theme)),
        );
        let window = match opened {
            Ok((window, _)) => window,
            Err(error) => {
                eprintln!("could not open the window: {error:#}");
                cx.quit();
                return;
            }
        };
        hermes_native::dev::start_dump(dump, window, cx);

        if let Some(after) = quit_after {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(after).await;
                cx.update(|cx| cx.quit());
            })
            .detach();
        }
    });
}
