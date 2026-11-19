//! The design layer: the tokens every view takes its colours, gaps, radii and sizes from
//! ([`tokens`]) and the components built on them, which views use instead of building controls
//! from bare `div`s: [`Button`] and its icon form, [`ListRow`] and [`ThreadRow`], [`SectionHeader`],
//! [`Panel`], [`Badge`], [`Kbd`], [`Tooltip`], [`Toast`], [`EmptyState`] and [`MiddleTruncate`].
//! Each wraps GPUI Kit's component where Kit has one that fits (the button's activation, focus and
//! accessibility are Kit's `Button`; icons are Kit's `Icon` over the Lucide set), takes every colour
//! from the active palette (`cx.design_theme().palette`, [`crate::theme::DesignTheme`]) and is a
//! story in the gallery with all its states. Each module's header names the t3code source it
//! copies and the reference screenshot it was checked against.

pub mod badge;
pub mod button;
pub mod empty_state;
pub mod icons;
pub mod kbd;
pub mod list_row;
pub mod middle_truncate;
pub mod panel;
pub mod section_header;
pub mod toast;
pub mod tokens;
pub mod tooltip;

pub use badge::Badge;
pub use button::{Button, IconButton};
pub use empty_state::EmptyState;
pub use icons::AppAssets;
pub use kbd::{Kbd, KbdGroup};
pub use list_row::{ListRow, ThreadRow};
pub use middle_truncate::MiddleTruncate;
pub use panel::Panel;
pub use section_header::SectionHeader;
pub use toast::{Toast, ToastSpec, Toasts};
pub use tokens::{Appearance, Color, Palette};
pub use tooltip::Tooltip;
