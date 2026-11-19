//! Scripts injected into pages (stealth, visualizer) and frame helpers, ported from src/main

pub mod oopif;
pub mod stealth_init;
pub mod visualizer;

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;
    use std::process::Command;

    /// Evaluates a string export of a TypeScript module with bun, so a test can
    /// compare the Rust constant with what the TypeScript really produces.
    /// `None` when bun or the TypeScript source is not there: the comparison is
    /// then skipped, the structural tests still run.
    pub(crate) fn ts_string_export(path_from_repo_root: &str, export: &str) -> Option<String> {
        ts_string_export_on(path_from_repo_root, export, None)
    }

    /// Same, with Node's `process.platform` / `process.arch` overridden before
    /// the module loads, so a value the TypeScript derives from the host can be
    /// compared for hosts other than the one running the test.
    pub(crate) fn ts_string_export_on(
        path_from_repo_root: &str,
        export: &str,
        node_host: Option<(&str, &str)>,
    ) -> Option<String> {
        let (platform, arch) = node_host.unwrap_or_default();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path_from_repo_root);
        if !path.is_file() {
            eprintln!("skipped: {} is not there", path.display());
            return None;
        }
        let output = Command::new("bun")
            .args([
                "-e",
                "const e = process.env; \
                 if (e.REVER_TS_PLATFORM) { \
                   Object.defineProperty(process, 'platform', { value: e.REVER_TS_PLATFORM }); \
                   Object.defineProperty(process, 'arch', { value: e.REVER_TS_ARCH }); \
                 } \
                 const m = await import(e.REVER_TS_PATH); \
                 process.stdout.write(m[e.REVER_TS_EXPORT])",
            ])
            .env("REVER_TS_PLATFORM", platform)
            .env("REVER_TS_ARCH", arch)
            .env("REVER_TS_PATH", &path)
            .env("REVER_TS_EXPORT", export)
            .output();
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                eprintln!("skipped: bun did not start ({error})");
                return None;
            }
        };
        assert!(
            output.status.success(),
            "bun failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        Some(String::from_utf8(output.stdout).expect("bun prints UTF-8"))
    }
}
