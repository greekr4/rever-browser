//! The tool catalog and control plane.
//!
//! Argument validation, result paging and the per-session tool profile — the
//! parts of serving a tool call that do not depend on how the call arrives.
//! A transport (the MCP adapter today, the CLI socket next) owns the wire.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

pub mod paging;
#[cfg(unix)]
pub mod socket;
pub mod tool_profile;
pub mod tool_profile_state;
pub mod validation;

pub use tool_profile_state::ToolProfileState;

/// Node's `writeFileSync(file, text, { mode: 0o600 })` followed by `chmodSync`:
/// both files this crate writes carry a secret or a setting only the user may read.
pub fn write_private_file(file: &Path, text: &str) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(file)?.write_all(text.as_bytes())?;
    // `mode` only applies when the file is created; an existing file keeps its
    // old bits, so tighten it explicitly.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
