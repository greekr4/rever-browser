//! Chat panel parts: tabs, history menu, model picker, provider icons, permission prompt.
//!
//! Each view takes what the React component got from props or stores as a closure read
//! on every render, what it got from `window.rev` as a host trait, and reports what it
//! cannot carry out itself (open a conversation, switch model, answer a prompt) as a
//! GPUI event.

pub mod chat_history_menu;
pub mod chat_tabs;
pub mod model_catalog;
pub mod model_menu;
pub mod model_picker;
pub mod permission_prompt;
pub mod provider_icon;
pub mod tool_profile;
pub mod ui;
