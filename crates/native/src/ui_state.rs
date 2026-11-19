//! `native-ui.json`: the native app's remembered layout, in the data folder beside `sessions.db`
//! (AGENTS.md ground rule 6: a side file, never the store's schema). Window position and size,
//! whether it was maximised, the sidebar, right panel and bottom drawer (open or not, and their
//! sizes) and the theme. Later phases add per-thread drafts and collapsed projects here.
//!
//! Reading never fails: a missing file, a file that is not JSON, or a value out of range gives the
//! defaults for what is wrong (a bad width does not reset the window position). Writing is atomic
//! (a temporary file renamed over the old one) so a crash cannot leave half a file. The app
//! writes it debounced ([`SAVE_AFTER`]) when something changed.
//!
//! t3code keeps the same things in the browser's local storage (`chat_thread_sidebar_width` in
//! `threadSidebarWidth.ts`, `rightPanelStore.ts`, `uiStateStore.ts`); the defaults and limits here
//! are its numbers (sidebar 256 px, 208 to 640 here; right panel 42vw capped 360 to 560).

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How long after the last change the file is written.
pub const SAVE_AFTER: Duration = Duration::from_millis(500);

/// The file's name in the data folder.
pub const FILE: &str = "native-ui.json";

/// Sidebar width limits and default, in logical pixels.
pub mod sidebar {
    /// Default (`THREAD_SIDEBAR_DEFAULT_WIDTH`, 16 rem).
    pub const DEFAULT: f32 = 256.;
    /// Narrowest (13 rem).
    pub const MIN: f32 = 208.;
    /// Widest.
    pub const MAX: f32 = 640.;
}

/// Right panel width limits and default.
pub mod right_panel {
    /// Default.
    pub const DEFAULT: f32 = 420.;
    /// Narrowest.
    pub const MIN: f32 = 280.;
    /// Widest.
    pub const MAX: f32 = 720.;
}

/// Bottom drawer height limits and default.
pub mod drawer {
    /// Default.
    pub const DEFAULT: f32 = 280.;
    /// Lowest.
    pub const MIN: f32 = 120.;
    /// Tallest.
    pub const MAX: f32 = 640.;
}

/// The smallest window (logical pixels): the sidebar's minimum plus the middle's 320.
pub const MIN_WINDOW: (f32, f32) = (640., 480.);

/// The window as it was left.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    /// Left edge, logical pixels, screen coordinates.
    pub x: Option<f32>,
    /// Top edge.
    pub y: Option<f32>,
    /// Width of the restored (not maximised) window.
    pub width: f32,
    /// Height of the restored window.
    pub height: f32,
    /// Whether it was maximised.
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            x: None,
            y: None,
            width: 1280.,
            height: 820.,
            maximized: false,
        }
    }
}

impl WindowState {
    /// Whether a position was saved (otherwise the window is centred).
    pub fn has_position(&self) -> bool {
        self.x.is_some() && self.y.is_some()
    }
}

/// A side region: whether it is open and its size.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PaneState {
    /// Open or collapsed.
    pub open: bool,
    /// Width (sidebar, right panel) or height (drawer) when open.
    pub size: f32,
}

/// Everything remembered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// The window.
    pub window: WindowState,
    /// The left sidebar (open by default).
    pub sidebar: PaneState,
    /// The right panel (closed by default).
    pub right_panel: PaneState,
    /// The bottom drawer (closed by default).
    pub drawer: PaneState,
    /// The theme's name (`None`: follow the system's light or dark).
    pub theme: Option<String>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            window: WindowState::default(),
            sidebar: PaneState {
                open: true,
                size: sidebar::DEFAULT,
            },
            right_panel: PaneState {
                open: false,
                size: right_panel::DEFAULT,
            },
            drawer: PaneState {
                open: false,
                size: drawer::DEFAULT,
            },
            theme: None,
        }
    }
}

// A pane missing a field takes the field's default from the pane's own kind, so each pane has its
// own default through `UiState`; `PaneState::default` is only for `#[serde(default)]` itself.
impl Default for PaneState {
    fn default() -> Self {
        Self {
            open: false,
            size: 0.,
        }
    }
}

fn clamp_size(size: f32, min: f32, max: f32, default: f32) -> f32 {
    if size.is_finite() {
        size.clamp(min, max)
    } else {
        default
    }
}

impl UiState {
    /// Where the file is in `data_dir`.
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(FILE)
    }

    /// Puts every value in its range (a hand-edited or old file can hold anything).
    pub fn sanitized(mut self) -> Self {
        let w = &mut self.window;
        w.width = clamp_size(w.width, MIN_WINDOW.0, 16_000., 1280.);
        w.height = clamp_size(w.height, MIN_WINDOW.1, 16_000., 820.);
        let sane = |v: Option<f32>| v.filter(|v| v.is_finite() && v.abs() < 64_000.);
        match (sane(w.x), sane(w.y)) {
            (Some(x), Some(y)) => (w.x, w.y) = (Some(x), Some(y)),
            _ => (w.x, w.y) = (None, None),
        }
        // A pane that is "missing" in the file has size 0: not a real size, so the default.
        let fix = |pane: &mut PaneState, min, max, default| {
            pane.size = if pane.size == 0. {
                default
            } else {
                clamp_size(pane.size, min, max, default)
            };
        };
        fix(
            &mut self.sidebar,
            sidebar::MIN,
            sidebar::MAX,
            sidebar::DEFAULT,
        );
        fix(
            &mut self.right_panel,
            right_panel::MIN,
            right_panel::MAX,
            right_panel::DEFAULT,
        );
        fix(&mut self.drawer, drawer::MIN, drawer::MAX, drawer::DEFAULT);
        self
    }

    /// Reads `<data_dir>/native-ui.json`. Missing or unreadable gives the defaults; a file whose
    /// top level is not an object too; fields of the wrong type fall back one by one (each part of
    /// the file is read on its own, so a bad `drawer` does not reset the `window`).
    pub fn load(data_dir: &Path) -> Self {
        let defaults = Self::default();
        let Ok(bytes) = std::fs::read(Self::path(data_dir)) else {
            return defaults;
        };
        let text = String::from_utf8_lossy(&bytes);
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!("{FILE} is not valid JSON, using defaults: {error}");
                return defaults;
            }
        };
        let Some(object) = value.as_object() else {
            tracing::warn!("{FILE} is not an object, using defaults");
            return defaults;
        };
        let part = |name: &str| object.get(name).cloned();
        let read = |name: &str, default: &PaneState| -> PaneState {
            let mut pane = *default;
            if let Some(Value::Object(fields)) = part(name) {
                if let Some(open) = fields.get("open").and_then(Value::as_bool) {
                    pane.open = open;
                }
                if let Some(size) = fields.get("size").and_then(Value::as_f64) {
                    pane.size = size as f32;
                }
            }
            pane
        };
        let window = part("window")
            .and_then(|v| serde_json::from_value::<WindowState>(v).ok())
            .unwrap_or(defaults.window);
        Self {
            window,
            sidebar: read("sidebar", &defaults.sidebar),
            right_panel: read("right_panel", &defaults.right_panel),
            drawer: read("drawer", &defaults.drawer),
            theme: part("theme").and_then(|v| v.as_str().map(str::to_string)),
        }
        .sanitized()
    }

    /// Writes the file (atomically).
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let path = Self::path(data_dir);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }
}

#[cfg(test)]
mod tests {
    use hermes_testdir::tempdir;

    use super::*;

    #[test]
    fn a_missing_file_gives_the_defaults() {
        let dir = tempdir().unwrap();
        let state = UiState::load(dir.path());
        assert_eq!(state, UiState::default().sanitized());
        assert!(state.sidebar.open && !state.right_panel.open && !state.drawer.open);
        assert!(!state.window.has_position());
    }

    #[test]
    fn what_is_saved_is_what_is_loaded() {
        let dir = tempdir().unwrap();
        let mut state = UiState::default();
        state.window = WindowState {
            x: Some(120.),
            y: Some(80.),
            width: 1500.,
            height: 900.,
            maximized: true,
        };
        state.sidebar = PaneState {
            open: false,
            size: 300.,
        };
        state.right_panel = PaneState {
            open: true,
            size: 500.,
        };
        state.drawer = PaneState {
            open: true,
            size: 200.,
        };
        state.theme = Some("One Dark".into());
        state.save(dir.path()).unwrap();
        assert_eq!(UiState::load(dir.path()), state);
        // No temporary file is left behind.
        assert!(!dir.path().join("native-ui.json.tmp").exists());
    }

    #[test]
    fn a_window_without_a_position_round_trips() {
        let dir = tempdir().unwrap();
        UiState::default().save(dir.path()).unwrap();
        assert!(!UiState::load(dir.path()).window.has_position());
    }

    #[test]
    fn a_malformed_file_gives_the_defaults() {
        let dir = tempdir().unwrap();
        for bad in ["", "not json", "[1,2]", "{\"window\": ", "\u{0}\u{1}"] {
            std::fs::write(UiState::path(dir.path()), bad).unwrap();
            assert_eq!(
                UiState::load(dir.path()),
                UiState::default().sanitized(),
                "{bad:?}"
            );
        }
        std::fs::write(UiState::path(dir.path()), [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(UiState::load(dir.path()), UiState::default().sanitized());
    }

    #[test]
    fn one_bad_part_does_not_reset_the_rest() {
        let dir = tempdir().unwrap();
        std::fs::write(
            UiState::path(dir.path()),
            r#"{"window":{"x":10,"y":20,"width":1000,"height":700,"maximized":false},
                "sidebar":{"open":false,"size":"wide"},
                "right_panel":7,
                "drawer":{"open":true,"size":300},
                "theme":42}"#,
        )
        .unwrap();
        let state = UiState::load(dir.path());
        assert_eq!(state.window.width, 1000.);
        assert!(state.window.has_position());
        // The sidebar's size was the wrong type: its default, but `open` was read.
        assert_eq!(
            state.sidebar,
            PaneState {
                open: false,
                size: sidebar::DEFAULT
            }
        );
        assert_eq!(state.right_panel, UiState::default().right_panel);
        assert_eq!(
            state.drawer,
            PaneState {
                open: true,
                size: 300.
            }
        );
        assert_eq!(state.theme, None);
    }

    #[test]
    fn values_out_of_range_are_clamped() {
        let dir = tempdir().unwrap();
        std::fs::write(
            UiState::path(dir.path()),
            r#"{"window":{"x":1e30,"y":0,"width":3,"height":99999999},
                "sidebar":{"open":true,"size":5},
                "right_panel":{"open":true,"size":99999},
                "drawer":{"open":true,"size":-4}}"#,
        )
        .unwrap();
        let state = UiState::load(dir.path());
        assert!(
            !state.window.has_position(),
            "an absurd position is dropped"
        );
        assert_eq!(state.window.width, MIN_WINDOW.0);
        assert_eq!(state.window.height, 16_000.);
        assert_eq!(state.sidebar.size, sidebar::MIN);
        assert_eq!(state.right_panel.size, right_panel::MAX);
        assert_eq!(state.drawer.size, drawer::MIN);
    }

    #[test]
    fn a_byte_order_mark_is_ignored() {
        let dir = tempdir().unwrap();
        std::fs::write(
            UiState::path(dir.path()),
            "\u{feff}{\"sidebar\":{\"open\":false,\"size\":300}}",
        )
        .unwrap();
        assert_eq!(
            UiState::load(dir.path()).sidebar,
            PaneState {
                open: false,
                size: 300.
            }
        );
    }
}
