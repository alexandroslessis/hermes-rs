//! The composer (Phase 5, CP5.1 "Input and sending"): where the user writes to the open thread or
//! starts a new one. [`ComposerModel`] holds what the footer shows and does the sends, the queue
//! and the picks through the [`crate::model::Backend`]; [`ComposerView`] is the card (the input,
//! the footer, the queue above it); [`data`] has the parts with no GPUI in them (the modes, the
//! efforts, the model list and its filter, the queue, the drafts kept in `native-ui.json`).
//!
//! t3code: `apps/web/src/components/chat/ChatComposer.tsx` and `Composer*`; references and the
//! parts left out are named in [`view`]'s header. Later checkpoints add `@` mentions, the `/`
//! menu and attachments (CP5.2) and the approval and question panels (CP5.3).

pub mod data;
pub mod model;
#[cfg(test)]
mod real_tests;
#[cfg(test)]
mod tests;
pub mod view;
#[cfg(test)]
mod wiring_tests;

pub use model::{Choices, ComposerEvent, ComposerModel, Target};
pub use view::{ComposerView, ComposerViewEvent, Menu};
