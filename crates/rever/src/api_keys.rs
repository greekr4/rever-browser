//! The API keys of the providers the app calls directly (`src/main/settings.ts`).
//!
//! Kept in the macOS login keychain, one item per provider, read and written with the
//! `security` tool. A provider without a saved key falls back to its environment
//! variable. The key files the Electron app wrote are not read: they were encrypted
//! with that app's own keychain secret.

use std::io::Write;
use std::process::{Command, Stdio};

/// The keychain service every provider's key is saved under; the provider is the account.
const SERVICE: &str = "rever-browser API key";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiProvider {
    Anthropic,
    Openai,
    Grok,
    Glm,
}

impl ApiProvider {
    fn account(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Grok => "grok",
            Self::Glm => "glm",
        }
    }

    fn env_var(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::Openai => "OPENAI_API_KEY",
            Self::Grok => "XAI_API_KEY",
            Self::Glm => "GLM_API_KEY",
        }
    }
}

/// The provider's key: the saved one first, then the environment variable.
pub fn api_key(provider: ApiProvider) -> Option<String> {
    let saved = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            SERVICE,
            "-a",
            provider.account(),
            "-w",
        ])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned());
    saved
        .or_else(|| {
            std::env::var(provider.env_var())
                .ok()
                .map(|key| key.trim().to_owned())
        })
        .filter(|key| !key.is_empty())
}

/// The line `security -i` runs to save `key`, or `None` for a key it cannot carry. The
/// line is written to the tool's input, so the key never shows in a process list; that
/// input is split like a shell's, so a key with a space, a quote or a backslash is
/// refused instead of being saved as something else.
fn save_command(provider: ApiProvider, key: &str) -> Option<String> {
    let plain = key
        .chars()
        .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\'' | '\\'));
    (plain && !key.is_empty()).then(|| {
        let account = provider.account();
        format!("add-generic-password -U -s \"{SERVICE}\" -a {account} -w {key}\n")
    })
}

/// Save the provider's key; an empty one removes it.
pub fn set_api_key(provider: ApiProvider, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        // Deleting a key that is not there fails, which is the wanted end state.
        let _ = Command::new("security")
            .args([
                "delete-generic-password",
                "-s",
                SERVICE,
                "-a",
                provider.account(),
            ])
            .output();
        return Ok(());
    }
    let line = save_command(provider, key).ok_or("this does not look like an API key")?;
    let mut child = Command::new("security")
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not run `security`: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(line.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    let out = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    // `security -i` reports a failed command on stderr and still exits 0.
    let complaint = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    if out.status.success() && complaint.is_empty() {
        Ok(())
    } else {
        Err(format!("the keychain did not take the key: {complaint}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 키는_키체인_도구의_입력으로_넘길_한_줄이_된다() {
        assert_eq!(
            save_command(ApiProvider::Openai, "sk-proj_Ab-12").as_deref(),
            Some(
                "add-generic-password -U -s \"rever-browser API key\" -a openai -w sk-proj_Ab-12\n"
            )
        );
    }

    #[test]
    fn 줄을_깨뜨릴_수_있는_글자가_든_키는_받지_않는다() {
        for key in ["", "sk a", "sk\"a", "sk'a", "sk\\a", "sk\na", "키"] {
            assert_eq!(save_command(ApiProvider::Glm, key), None, "{key:?}");
        }
    }
}

#[cfg(test)]
mod keychain {
    use super::*;

    /// Touches the real login keychain, so it only runs with `REVER_KEYCHAIN_TEST=1`.
    #[test]
    fn 저장한_키를_다시_읽고_빈_값으로_지운다() {
        if std::env::var("REVER_KEYCHAIN_TEST").is_err() {
            return;
        }
        // GLM is used only because no real key is saved under it here.
        assert_eq!(
            api_key(ApiProvider::Glm),
            None,
            "a real GLM key is saved: not touching it"
        );
        set_api_key(ApiProvider::Glm, "  sk-test_Round-trip.1  ").unwrap();
        assert_eq!(
            api_key(ApiProvider::Glm).as_deref(),
            Some("sk-test_Round-trip.1")
        );
        // Saving again replaces the item instead of failing on a duplicate.
        set_api_key(ApiProvider::Glm, "sk-second").unwrap();
        assert_eq!(api_key(ApiProvider::Glm).as_deref(), Some("sk-second"));
        set_api_key(ApiProvider::Glm, "").unwrap();
        assert_eq!(api_key(ApiProvider::Glm), None);
    }
}
