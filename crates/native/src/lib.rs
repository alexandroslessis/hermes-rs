//! `hermes-native`: the native Windows UI for Hermes on GPUI Kit (`docs/native-ui/`). The library
//! holds what the two binaries share: `hermes-native` (CP0.1's spike window for now, the real app
//! from Phase 2 on) and `hermes-native-gallery` (every component and screen as a story).
//!
//! The checking tools (CP0.2, `docs/TESTING.md` *Native UI*): [`dev`] (`.probe()` and the
//! element dump), `testing` (layout checks, in tests and with `--features dev`), and the gallery's
//! snapshot tests (`cargo test -p hermes-native --features snapshots`).

pub mod dev;
pub mod fake;
pub mod gallery;
pub mod spike;
#[cfg(any(test, feature = "dev"))]
pub mod testing;
pub mod themes;
pub mod zed_theme;
