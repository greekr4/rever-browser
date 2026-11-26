//! The MCP adapter: serves the registered tools over streamable HTTP and
//! publishes its endpoint. The transport-free half lives in `rever-control`.

pub mod server;

pub use rever_control::write_private_file;
pub use rever_control::{ToolProfileState, paging, tool_profile, tool_profile_state, validation};
pub use server::{
    McpServer, PageEvents, ServerOptions, as_tool_call, in_tool_call, start, start_with,
};
