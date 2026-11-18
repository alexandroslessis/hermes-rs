//! The design layer: the tokens every view takes its colours, gaps, radii and sizes from
//! ([`tokens`]) and, from CP2.2 on, the components built on them (`Button`, `ListRow`, `Panel`…).
//! Colours reach views through [`crate::theme::DesignTheme`] (`cx.palette()`), the active palette.

pub mod tokens;

pub use tokens::{Appearance, Color, Palette};
