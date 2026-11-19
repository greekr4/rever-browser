//! The HTTP and WebSocket repeater panels
//!
//! The host makes one [`repeater::RepeaterStore`] entity (the traffic list's "↻R" button
//! loads a request into it) and hands it to [`repeater_panel::RepeaterPanel`] together
//! with a [`repeater_panel::RepeaterSender`]. [`ws_repeater_panel::WsRepeaterPanel`] reads
//! the traffic store and sends through the WebSocket panel's `WsHost`.

pub mod repeater;
pub mod repeater_panel;
mod text_area;
mod ui;
pub mod ws_repeater_panel;

pub use repeater::{Headers, HistoryEntry, RepeaterRequestSpec, RepeaterResponse, RepeaterStore};
pub use repeater_panel::{RepeaterPanel, RepeaterSender};
pub use ws_repeater_panel::WsRepeaterPanel;
