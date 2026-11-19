//! Port of `src/main/agent-login.ts` — which agent CLIs the user is already signed in to.
//!
//! Detects agent CLIs the user is *already* signed in to, so onboarding can show
//! "connected automatically" instead of asking for an API key they don't need.
//! Credentials are only inspected for shape — tokens are never logged or
//! returned to the UI.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::acp_detect::{exec_file_stdout, js_trim};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LoginAgentId {
    #[serde(rename = "claude-code")]
    ClaudeCode,
    #[serde(rename = "codex")]
    Codex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginInfo {
    pub logged_in: bool,
    /// Plan/mode the credential advertises ('max', 'chatgpt', 'api-key'), or null.
    pub plan: Option<String>,
}

impl LoginInfo {
    pub const NO_LOGIN: Self = Self {
        logged_in: false,
        plan: None,
    };

    pub(crate) fn logged_in(plan: Option<&str>) -> Self {
        Self {
            logged_in: true,
            plan: plan.map(str::to_owned),
        }
    }
}

/// The login state of every agent that signs in through its own CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLogins {
    #[serde(rename = "claude-code")]
    pub claude_code: LoginInfo,
    pub codex: LoginInfo,
}

impl AgentLogins {
    pub fn get(&self, agent: LoginAgentId) -> &LoginInfo {
        match agent {
            LoginAgentId::ClaudeCode => &self.claude_code,
            LoginAgentId::Codex => &self.codex,
        }
    }
}

/// Milliseconds since the Unix epoch (`Date.now()`).
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

fn parse(raw: &str) -> Option<Value> {
    serde_json::from_str(raw).ok()
}

/// A string field, or "" when it is missing or not a string.
fn text<'a>(object: &'a Value, key: &str) -> &'a str {
    object.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn non_empty(text: &str) -> Option<&str> {
    Some(text).filter(|text| !text.is_empty())
}

/// Read a Claude Code credential blob (keychain item or ~/.claude/.credentials.json).
/// An expired access token still counts as logged in while the refresh token is
/// alive — the CLI refreshes on its own.
pub fn read_claude_login(raw: &str, now_ms: i64) -> LoginInfo {
    let Some(root) = parse(raw) else {
        return LoginInfo::NO_LOGIN;
    };
    let Some(oauth) = root.get("claudeAiOauth") else {
        return LoginInfo::NO_LOGIN;
    };
    if text(oauth, "accessToken").is_empty() {
        return LoginInfo::NO_LOGIN;
    }

    // Timestamps stay f64: the file is JSON, where they are JavaScript numbers.
    let now = now_ms as f64;
    let number = |key: &str| oauth.get(key).and_then(Value::as_f64);
    let login = LoginInfo::logged_in(non_empty(text(oauth, "subscriptionType")));
    if number("expiresAt").is_none_or(|expires_at| expires_at > now) {
        return login;
    }

    // Access token expired — fall back to the refresh token.
    if text(oauth, "refreshToken").is_empty() {
        return LoginInfo::NO_LOGIN;
    }
    if number("refreshTokenExpiresAt").is_some_and(|expires_at| expires_at <= now) {
        return LoginInfo::NO_LOGIN;
    }
    login
}

/// Read a Codex CLI ~/.codex/auth.json blob.
pub fn read_codex_login(raw: &str) -> LoginInfo {
    let Some(root) = parse(raw) else {
        return LoginInfo::NO_LOGIN;
    };
    let has_token = root
        .get("tokens")
        .is_some_and(|tokens| !text(tokens, "access_token").is_empty());
    if has_token {
        return LoginInfo::logged_in(non_empty(text(&root, "auth_mode")));
    }
    if !text(&root, "OPENAI_API_KEY").is_empty() {
        return LoginInfo::logged_in(Some("api-key"));
    }
    LoginInfo::NO_LOGIN
}

/// Login state from the credential blobs that could be read.
pub fn logins_from_blobs(
    claude_raw: Option<&str>,
    codex_raw: Option<&str>,
    now_ms: i64,
) -> AgentLogins {
    AgentLogins {
        claude_code: claude_raw.map_or(LoginInfo::NO_LOGIN, |raw| read_claude_login(raw, now_ms)),
        codex: codex_raw.map_or(LoginInfo::NO_LOGIN, read_codex_login),
    }
}

/// Like Node's `readFile(path, 'utf8')`, bytes that are not UTF-8 are replaced
/// rather than failing the read.
pub(crate) fn read_file_or_none(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// macOS stores the Claude credential in the login keychain; other platforms
/// keep it in ~/.claude/.credentials.json. Try the keychain first, then the file.
fn read_claude_credential_blob(home: Option<&Path>) -> Option<String> {
    if cfg!(target_os = "macos") {
        // Not in the keychain (or access denied) — fall through to the file.
        let found = exec_file_stdout(
            "security",
            &[
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ],
            Duration::from_millis(3_000),
        );
        if let Some(blob) = found.as_deref().map(js_trim).and_then(non_empty) {
            return Some(blob.to_owned());
        }
    }
    read_file_or_none(&home?.join(".claude").join(".credentials.json"))
}

/// Probe every agent that can be signed in via its own CLI.
pub fn detect_agent_logins() -> AgentLogins {
    let home = std::env::home_dir();
    let claude_raw = read_claude_credential_blob(home.as_deref());
    let codex_raw = home
        .as_deref()
        .and_then(|home| read_file_or_none(&home.join(".codex").join("auth.json")));
    logins_from_blobs(claude_raw.as_deref(), codex_raw.as_deref(), now_ms())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const NOW: i64 = 1_700_000_000_000;
    const HOUR: i64 = 3_600_000;

    fn logged_in(plan: Option<&str>) -> LoginInfo {
        LoginInfo::logged_in(plan)
    }

    /// `over` replaces fields; a `None` value drops the field, as an
    /// `undefined` does under `JSON.stringify`.
    fn claude_blob(over: &[(&str, Option<Value>)]) -> String {
        let mut oauth = json!({
            "accessToken": "at",
            "refreshToken": "rt",
            "expiresAt": NOW + HOUR,
            "refreshTokenExpiresAt": NOW + 30 * 24 * HOUR,
            "subscriptionType": "max"
        });
        if let Some(fields) = oauth.as_object_mut() {
            for (key, value) in over {
                match value {
                    Some(value) => fields.insert((*key).to_owned(), value.clone()),
                    None => fields.remove(*key),
                };
            }
        }
        json!({ "claudeAiOauth": oauth }).to_string()
    }

    // ── readClaudeLogin ──

    #[test]
    fn 유효한_액세스_토큰이면_로그인으로_본다() {
        assert_eq!(
            read_claude_login(&claude_blob(&[]), NOW),
            logged_in(Some("max"))
        );
    }

    #[test]
    fn 액세스_토큰이_만료돼도_리프레시_토큰이_살아있으면_로그인이다() {
        let raw = claude_blob(&[("expiresAt", Some(json!(NOW - HOUR)))]);
        assert_eq!(read_claude_login(&raw, NOW), logged_in(Some("max")));
    }

    #[test]
    fn 두_토큰이_모두_만료되면_로그인이_아니다() {
        let raw = claude_blob(&[
            ("expiresAt", Some(json!(NOW - HOUR))),
            ("refreshTokenExpiresAt", Some(json!(NOW - HOUR))),
        ]);
        assert_eq!(read_claude_login(&raw, NOW), LoginInfo::NO_LOGIN);
    }

    #[test]
    fn 만료_시각이_없으면_토큰_존재만으로_판단한다() {
        let raw = claude_blob(&[("expiresAt", None), ("refreshTokenExpiresAt", None)]);
        assert!(read_claude_login(&raw, NOW).logged_in);
    }

    #[test]
    fn subscription_type이_없으면_plan은_null이다() {
        let raw = claude_blob(&[("subscriptionType", None)]);
        assert_eq!(read_claude_login(&raw, NOW), logged_in(None));
    }

    #[test]
    fn 액세스_토큰이_비었으면_로그인이_아니다() {
        let raw = claude_blob(&[("accessToken", Some(json!("")))]);
        assert!(!read_claude_login(&raw, NOW).logged_in);
    }

    #[test]
    fn claude_ai_oauth_키가_없으면_로그인이_아니다() {
        let raw = json!({ "mcpOAuth": {} }).to_string();
        assert!(!read_claude_login(&raw, NOW).logged_in);
    }

    #[test]
    fn claude_json이_깨졌으면_던지지_않고_로그인_아님으로_처리한다() {
        assert_eq!(read_claude_login("not json", NOW), LoginInfo::NO_LOGIN);
        assert_eq!(read_claude_login("", NOW), LoginInfo::NO_LOGIN);
    }

    // ── readCodexLogin ──

    #[test]
    fn chatgpt_토큰이_있으면_로그인이다() {
        let raw = json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": { "access_token": "at", "refresh_token": "rt" }
        })
        .to_string();
        assert_eq!(read_codex_login(&raw), logged_in(Some("chatgpt")));
    }

    #[test]
    fn api_키만_있어도_로그인이다() {
        let raw = json!({ "OPENAI_API_KEY": "sk-x", "tokens": null }).to_string();
        assert_eq!(read_codex_login(&raw), logged_in(Some("api-key")));
    }

    #[test]
    fn 토큰도_키도_없으면_로그인이_아니다() {
        let raw =
            json!({ "auth_mode": "chatgpt", "OPENAI_API_KEY": null, "tokens": {} }).to_string();
        assert_eq!(read_codex_login(&raw), LoginInfo::NO_LOGIN);
    }

    #[test]
    fn codex_json이_깨졌으면_던지지_않고_로그인_아님으로_처리한다() {
        assert_eq!(read_codex_login("{{"), LoginInfo::NO_LOGIN);
    }

    // ── not covered by the TypeScript tests ──

    #[test]
    fn 만료_시각과_같은_순간은_만료로_본다() {
        let raw = claude_blob(&[("expiresAt", Some(json!(NOW)))]);
        // Access token just expired, refresh token alive.
        assert!(read_claude_login(&raw, NOW).logged_in);
        let raw = claude_blob(&[
            ("expiresAt", Some(json!(NOW))),
            ("refreshTokenExpiresAt", Some(json!(NOW))),
        ]);
        assert!(!read_claude_login(&raw, NOW).logged_in);
        let raw = claude_blob(&[
            ("expiresAt", Some(json!(NOW))),
            ("refreshTokenExpiresAt", Some(json!(NOW + 1))),
        ]);
        assert!(read_claude_login(&raw, NOW).logged_in);
    }

    #[test]
    fn 액세스_토큰이_만료됐는데_리프레시_토큰이_없으면_로그인이_아니다() {
        let raw = claude_blob(&[
            ("expiresAt", Some(json!(NOW - HOUR))),
            ("refreshToken", None),
        ]);
        assert_eq!(read_claude_login(&raw, NOW), LoginInfo::NO_LOGIN);
        // No refresh expiry recorded: a refresh token alone keeps the login.
        let raw = claude_blob(&[
            ("expiresAt", Some(json!(NOW - HOUR))),
            ("refreshTokenExpiresAt", None),
        ]);
        assert_eq!(read_claude_login(&raw, NOW), logged_in(Some("max")));
    }

    #[test]
    fn 타입이_맞지_않는_필드는_없는_것으로_본다() {
        // A numeric token is not a token; a string expiry is "no expiry".
        let raw = claude_blob(&[("accessToken", Some(json!(12345)))]);
        assert!(!read_claude_login(&raw, NOW).logged_in);
        let raw = claude_blob(&[
            ("expiresAt", Some(json!("0"))),
            ("subscriptionType", Some(json!(7))),
        ]);
        assert_eq!(read_claude_login(&raw, NOW), logged_in(None));
        let raw = claude_blob(&[("expiresAt", Some(json!(1.5)))]);
        assert!(read_claude_login(&raw, NOW).logged_in);
    }

    #[test]
    fn 객체가_아닌_json도_던지지_않는다() {
        for raw in [
            "null",
            "5",
            "\"text\"",
            "[]",
            "[{\"claudeAiOauth\":{\"accessToken\":\"at\"}}]",
            "{\"claudeAiOauth\":null}",
            "{\"claudeAiOauth\":\"at\"}",
            "{\"claudeAiOauth\":[\"at\"]}",
        ] {
            assert_eq!(read_claude_login(raw, NOW), LoginInfo::NO_LOGIN, "{raw}");
        }
        for raw in [
            "null",
            "5",
            "[]",
            "{\"tokens\":\"access_token\"}",
            "{\"tokens\":[\"at\"]}",
            "{\"tokens\":{\"access_token\":1},\"OPENAI_API_KEY\":true}",
        ] {
            assert_eq!(read_codex_login(raw), LoginInfo::NO_LOGIN, "{raw}");
        }
    }

    #[test]
    fn codex_토큰이_있는데_auth_mode가_없으면_plan은_null이다() {
        let raw = json!({ "tokens": { "access_token": "at" }, "OPENAI_API_KEY": "sk-x" });
        assert_eq!(read_codex_login(&raw.to_string()), logged_in(None));
    }

    #[test]
    fn 읽지_못한_자격_증명은_로그인_아님이다() {
        let codex = json!({ "OPENAI_API_KEY": "sk-x" }).to_string();
        let logins = logins_from_blobs(None, Some(&codex), NOW);
        assert_eq!(logins.claude_code, LoginInfo::NO_LOGIN);
        assert_eq!(logins.codex, logged_in(Some("api-key")));

        let logins = logins_from_blobs(Some(&claude_blob(&[])), None, NOW);
        assert_eq!(
            logins.get(LoginAgentId::ClaudeCode),
            &logged_in(Some("max"))
        );
        assert_eq!(logins.get(LoginAgentId::Codex), &LoginInfo::NO_LOGIN);
        assert_eq!(
            logins_from_blobs(Some(""), Some(""), NOW),
            logins_from_blobs(None, None, NOW)
        );
    }

    #[test]
    fn 직렬화하면_타입스크립트와_같은_필드_이름을_쓴다() {
        let logins = logins_from_blobs(Some(&claude_blob(&[])), None, NOW);
        assert_eq!(
            serde_json::to_value(&logins).ok(),
            Some(json!({
                "claude-code": { "loggedIn": true, "plan": "max" },
                "codex": { "loggedIn": false, "plan": null }
            }))
        );
        assert_eq!(
            serde_json::to_value([LoginAgentId::ClaudeCode, LoginAgentId::Codex]).ok(),
            Some(json!(["claude-code", "codex"]))
        );
    }

    #[test]
    fn 파일이_없으면_none이고_utf8이_아닌_바이트는_대체_문자로_읽는다() {
        let dir = crate::test_support::TempDir::new("agent-login");
        assert_eq!(read_file_or_none(&dir.path().join("missing.json")), None);
        let file = dir.path().join("auth.json");
        assert!(std::fs::write(&file, b"{\"a\":\"\xff\"}").is_ok());
        assert_eq!(
            read_file_or_none(&file).as_deref(),
            Some("{\"a\":\"\u{fffd}\"}")
        );
    }
}
