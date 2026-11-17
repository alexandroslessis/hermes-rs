//! Developer tools for checking the UI without looking at it (`docs/native-ui/README.md`, *The
//! dev loop*): `.probe()` to name elements, and the element dump, written by `--dump <file>` or
//! F12 from a binary built with `--features dev`, and read by the layout checks in tests
//! (`crate::testing`).

mod probe;

#[cfg(any(test, feature = "dev"))]
pub use probe::{Probe, ProbeFacts};
pub use probe::{ProbeExt, Probed};

#[cfg(any(test, feature = "dev"))]
mod dump;
#[cfg(any(test, feature = "dev"))]
pub use dump::{Color, Dump, Entry};

#[cfg(feature = "dev")]
mod live {
    use std::{path::PathBuf, time::Duration};

    use gpui_kit::{AnyWindowHandle, App, KeyBinding, Window, actions};

    use super::Dump;

    actions!(hermes_dev, [DumpLayout]);

    /// Where F12 writes the dump: `layout-dump.txt` in `HERMES_RS_HOME` (our test instances set
    /// it to `target/native-test`), else in the working folder. Never the user's data folder.
    pub fn default_dump_path() -> PathBuf {
        std::env::var_os("HERMES_RS_HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("layout-dump.txt")
    }

    /// Binds F12 to writing the active window's dump to [`default_dump_path`].
    pub fn install(cx: &mut App) {
        cx.bind_keys([KeyBinding::new("f12", DumpLayout, None)]);
        cx.on_action(|_: &DumpLayout, cx| {
            if let Some(window) = cx.active_window() {
                write_dump(window, default_dump_path(), cx);
            }
        });
    }

    /// Writes `window`'s dump to `path` once it has drawn and settled for `delay` (for
    /// `--dump <file>`).
    pub fn dump_after(window: AnyWindowHandle, delay: Duration, path: PathBuf, cx: &mut App) {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(delay).await;
            cx.update(|cx| write_dump(window, path, cx));
        })
        .detach();
    }

    fn write_dump(window: AnyWindowHandle, path: PathBuf, cx: &mut App) {
        let text = window.update(cx, |_, window: &mut Window, _| {
            Dump::capture(window).to_text()
        });
        match text.map(|text| std::fs::write(&path, text)) {
            Ok(Ok(())) => eprintln!("layout dump: {}", path.display()),
            Ok(Err(error)) => eprintln!("layout dump: {}: {error}", path.display()),
            Err(error) => eprintln!("layout dump: {error:#}"),
        }
    }
}

#[cfg(feature = "dev")]
pub use live::{default_dump_path, dump_after, install};

/// `--dump <file>` from the command line.
pub fn dump_flag(args: &[String]) -> Option<std::path::PathBuf> {
    args.iter()
        .position(|a| a == "--dump")
        .and_then(|ix| args.get(ix + 1))
        .map(std::path::PathBuf::from)
}

/// Acts on `--dump <file>`: writes `window`'s dump there half a second after start (it has drawn
/// by then), or says the build has no dump.
pub fn start_dump(
    path: Option<std::path::PathBuf>,
    window: gpui_kit::AnyWindowHandle,
    cx: &mut gpui_kit::App,
) {
    let Some(path) = path else {
        return;
    };
    #[cfg(feature = "dev")]
    dump_after(window, std::time::Duration::from_millis(500), path, cx);
    #[cfg(not(feature = "dev"))]
    {
        let _ = (window, cx);
        eprintln!(
            "--dump {}: this build has no element dump (build with --features dev)",
            path.display()
        );
    }
}
