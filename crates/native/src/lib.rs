//! `hermes-native`: the native Windows UI for Hermes on GPUI Kit (`docs/native-ui/`). The library
//! holds what the two binaries share: `hermes-native` (CP0.1's spike window for now, the real app
//! from Phase 2 on) and `hermes-native-gallery` (every component and screen as a story).
//!
//! The checking tools (CP0.2, `docs/TESTING.md` *Native UI*): [`dev`] (`.probe()` and the
//! element dump), `testing` (layout checks, in tests and with `--features dev`), and the gallery's
//! snapshot tests (`cargo test -p hermes-native --features snapshots`).

pub mod app;
pub mod design;
pub mod dev;
pub mod fake;
pub mod gallery;
pub mod keymap;
pub mod model;
pub mod palette;
pub mod sidebar;
pub mod spike;
#[cfg(any(test, feature = "dev"))]
pub mod testing;
pub mod theme;
pub mod titlebar;
pub mod ui_state;
pub mod workspace;

/// Sends `tracing` warnings and errors (a theme file skipped, fonts that failed to load) to
/// stderr. Both binaries call it first; without a subscriber those messages are lost.
pub fn log_to_stderr() {
    // Fails only when a subscriber is already set, which is fine.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .try_init();
}
