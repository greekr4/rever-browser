//! The traffic panel: request list, filters and the detail drawer
//!
//! The host makes one [`store::TrafficState`] entity and hands it to both views:
//! [`traffic_list::TrafficList`] goes in the bottom panel, and
//! [`detail_drawer::DetailDrawer`] in a positioned container over the page.

pub mod detail_drawer;
pub mod json_view;
pub mod store;
pub mod traffic_detail_drawer;
pub mod traffic_list;
pub mod types;
mod ui;
