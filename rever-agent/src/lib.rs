//! Agent protocol translation and model catalogs, ported from src/main

pub mod acp_detect;
pub mod acp_models;
pub mod acp_turn;
pub mod agent_env;
pub mod agent_login;
pub mod agent_probe;
pub mod claude_cli;
pub mod claude_cli_protocol;
pub mod grok_login;
pub mod grok_models;
pub mod model_catalog;
pub mod model_catalog_parse;
pub mod skill_install;
pub mod turn_pulse;
pub mod win_shell;

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh directory under the system temp dir, removed on drop.
    pub(crate) struct TempDir(PathBuf);

    impl TempDir {
        pub(crate) fn new(tag: &str) -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("rever-agent-{tag}-{}-{unique}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create a test directory");
            Self(dir)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
