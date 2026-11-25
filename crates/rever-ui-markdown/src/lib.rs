//! Markdown rendering for chat messages

pub mod markdown;
pub(crate) mod selection;

pub use markdown::{CopySelection, Markdown, markdown, plain};
