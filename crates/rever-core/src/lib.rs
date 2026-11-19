//! Engine-independent logic for rever. Nothing here knows about Servo or GPUI,
//! so it builds and tests in seconds.

pub mod address;
pub mod har_import;
pub mod input;
pub mod paths;
pub mod secret_scan;
pub mod session_log;
pub mod tab_partition;
pub mod time;
pub mod traffic;
