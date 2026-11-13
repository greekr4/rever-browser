//! Popup menus, chip selects, toasts and small toolbar widgets.
//!
//! Each view takes what the React component got from props, stores or `window.rev`
//! as closures or a trait object, so the host decides where the data comes from.

pub mod app_menu;
pub mod bot_check_button;
pub mod chip_select;
pub mod copy_toast;
pub mod ip_badge;
pub mod page_event_toast;
pub mod ui;
