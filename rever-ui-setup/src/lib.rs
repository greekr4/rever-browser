//! Onboarding, browser import, profile menu and proxy settings.
//!
//! Everything the React components did over `window.rev.*`, and the renderer stores they
//! reached into, is behind [`host::SetupHost`]; the host implements it once and hands it
//! to each view. [`profiles`] is the profile store, a plain struct the host wraps in an
//! `Entity`; the other modules are the GPUI views.

pub mod browser_import_modal;
pub mod host;
pub mod onboarding_modal;
pub mod profile_menu;
pub mod profiles;
pub mod proxy_button;
pub mod ui;
