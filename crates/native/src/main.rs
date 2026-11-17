//! `hermes-native`: the native Windows UI for Hermes on GPUI Kit (`docs/native-ui/`). For now it
//! is CP0.1's throwaway spike window on fake data ([`spike`]); the real app replaces it from
//! Phase 2 on.
//!
//! Flags (for measuring, CP0.1 step 4):
//! - `--no-stream`: don't start the timer that streams text into the last message (idle memory).
//! - `--quit-after <seconds>`: quit on its own after that long.
//! - `--theme <dark|light|one-dark|one-light>`: the theme to start with (default `dark`).
//!
//! It prints `first-frame-ms=<n>` on stderr once the first frame has been drawn, counted from
//! the start of `main` (the process's own start-up before `main` is not included).

mod fake;
mod spike;
mod zed_theme;

use std::time::{Duration, Instant};

use gpui_kit::*;

fn main() {
    let started = Instant::now();
    let args: Vec<String> = std::env::args().collect();
    let stream = !args.iter().any(|a| a == "--no-stream");
    let quit_after = args
        .iter()
        .position(|a| a == "--quit-after")
        .and_then(|ix| args.get(ix + 1))
        .and_then(|secs| secs.parse::<f64>().ok())
        // `from_secs_f64` panics on a negative, NaN or huge value; such a flag is ignored.
        .and_then(|secs| Duration::try_from_secs_f64(secs).ok());
    let theme = match args
        .iter()
        .position(|a| a == "--theme")
        .and_then(|ix| args.get(ix + 1))
        .map(String::as_str)
    {
        Some("light") => spike::ThemeChoice::DefaultLight,
        Some("one-dark") => spike::ThemeChoice::OneDark,
        Some("one-light") => spike::ThemeChoice::OneLight,
        _ => spike::ThemeChoice::DefaultDark,
    };

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        spike::apply_theme(theme, cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        let opened = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Hermes native (spike)".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            cx,
            |window, cx| {
                window.on_next_frame(move |_, _| {
                    eprintln!("first-frame-ms={}", started.elapsed().as_millis());
                });
                cx.new(|cx| spike::Spike::new(stream, window, cx))
            },
        );
        if let Err(error) = opened {
            eprintln!("could not open the window: {error:#}");
            cx.quit();
            return;
        }

        if let Some(after) = quit_after {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(after).await;
                cx.update(|cx| cx.quit());
            })
            .detach();
        }
    });
}
