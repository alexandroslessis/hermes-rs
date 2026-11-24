//! `hermes-native`: the native Windows UI for Hermes on GPUI Kit (`docs/native-ui/`). From CP2.2 it
//! opens the app frame ([`hermes_native::app`]): title bar, sidebar, main area, a right panel and a
//! drawer that open on demand, the command palette (`Ctrl+K`), toasts and the themes. The
//! throwaway window of CP0.1 is still there with `--spike`.
//!
//! Flags of the app:
//! - `--theme <name>`: wear that theme (`"Default Dark"`, `"One Light"`, a user file's) instead of
//!   the remembered one.
//! - `--fixture`: fake projects and threads, no core and no app lock (for looking and testing).
//! - `--dev` (DEV ONLY, until Phase 5's composer): a strip under the open thread sends a prompt;
//!   with `--fixture` it replays the recorded turn. `--dev-provider <url>` sends the prompts to a
//!   mock OpenAI-compatible endpoint (`scripts/perf/mock_llm.py`: `http://127.0.0.1:47900/v1`).
//! - `--dev-send <prompt>` (with `--dev`): open the first listed thread, send the prompt, print
//!   the streamed and the stored reply on stderr, quit (the Phase 4 live check,
//!   `scripts/perf/native-thread-check.ps1`; `--dump` is then written when the turn has ended).
//! - `--quit-after <seconds>`, `--dump <file>` (with `--features dev`): as below.
//!
//! Flags of `--spike` (for measuring, CP0.1 step 4):
//! - `--no-stream`: don't start the timer that streams text into the last message (idle memory).
//! - `--quit-after <seconds>`: quit on its own after that long.
//! - `--theme <dark|light|one-dark|one-light|t3-dark|t3-light>`: the theme to start with (default `dark`).
//! - `--dump <file>` (with `--features dev`): write the element dump once the window has drawn
//!   (`docs/TESTING.md`, *Native UI*). With `--features dev`, F12 writes it to `layout-dump.txt`
//!   in `HERMES_RS_HOME`.
//!
//! It prints `first-frame-ms=<n>` on stderr once the first frame has been drawn, counted from
//! the start of `main` (the process's own start-up before `main` is not included).

use std::time::{Duration, Instant};

use gpui_kit::*;
use hermes_native::{
    spike,
    theme::{ThemeChoice, apply_theme},
};

fn main() {
    let started = Instant::now();
    hermes_native::log_to_stderr();
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
        Some("light") => ThemeChoice::DefaultLight,
        Some("one-dark") => ThemeChoice::OneDark,
        Some("t3-dark") => ThemeChoice::T3Dark,
        Some("t3-light") => ThemeChoice::T3Light,
        Some("one-light") => ThemeChoice::OneLight,
        _ => ThemeChoice::DefaultDark,
    };
    let dump = hermes_native::dev::dump_flag(&args);

    if !args.iter().any(|a| a == "--spike") {
        let theme = args
            .iter()
            .position(|a| a == "--theme")
            .and_then(|ix| args.get(ix + 1))
            .cloned();
        hermes_native::app::run(hermes_native::app::Options {
            quit_after,
            theme,
            dump,
            fixture: args.iter().any(|a| a == "--fixture"),
            dev: args.iter().any(|a| a == "--dev"),
            dev_provider: args
                .iter()
                .position(|a| a == "--dev-provider")
                .and_then(|ix| args.get(ix + 1))
                .cloned(),
            dev_send: args
                .iter()
                .position(|a| a == "--dev-send")
                .and_then(|ix| args.get(ix + 1))
                .cloned(),
        });
        return;
    }

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
        match opened {
            Ok((window, _)) => hermes_native::dev::start_dump(dump, window, cx),
            Err(error) => {
                eprintln!("could not open the window: {error:#}");
                cx.quit();
                return;
            }
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
