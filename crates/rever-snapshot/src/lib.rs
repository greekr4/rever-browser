//! Page snapshots with element refs, frames and layout, for the browser tools.
//!
//! A library: no tools are registered here. [`snapshot::Snapshotter`] takes the
//! outline through a `Browser` and resolves its refs; `layout` and `frames` hold
//! the pure parts it is built from.

pub mod frames;
pub mod layout;
pub mod snapshot;
