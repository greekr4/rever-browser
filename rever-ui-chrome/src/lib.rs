//! Browser chrome widgets: tab bar with overflow, markup editor, resizable splitters, find bar
//!
//! Each view takes what the React component got from props, stores or `window.rev`
//! as closures or a trait object, and reports what the user did as GPUI events.

pub mod find_in_page;
pub mod markup_editor;
pub mod resizable;
pub mod tab_bar;
pub mod ui;
