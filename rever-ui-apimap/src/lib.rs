//! The API map panel
//!
//! [`api_map_panel::ApiMapPanel`] draws the captured XHR/Fetch endpoints as a ring
//! around their domain. The host hands it the capture store, calls `refresh` when
//! traffic arrives, and subscribes to [`api_map_panel::ApiMapEvent`].

pub mod api_map_panel;
