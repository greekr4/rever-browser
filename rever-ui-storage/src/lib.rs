//! The storage panel: cookies, local and session storage
//!
//! [`cookies_panel::CookiesPanel`] is the view. It reads and changes the page's cookies
//! and storages through a [`cookies_panel::StorageHost`] the host application
//! implements; [`memory_host::MemoryStorageHost`] is the in-memory one the preview and
//! the tests use.

pub mod cookies_panel;
pub mod memory_host;
mod ui;
