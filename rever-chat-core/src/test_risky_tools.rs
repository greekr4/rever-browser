//! Test-only stand-in for the shared risky-tools module (src/shared/risky-tools.ts).
//!
//! That module is ported in another crate, which this crate must not depend on.
//! The ported tests of the permission queue and the session grants feed the same
//! requests as the TypeScript tests, so they need the same decisions: this mirrors
//! the TypeScript closely enough for those inputs. It is not the port — the
//! inline-script HTTP pattern (python/node one-liners) is left out.

use serde_json::Value;
use url::Url;

use crate::RiskyTools;

const ALWAYS_RISKY_TOOLS: [&str; 6] = [
    "intruder_run",
    "burst_send",
    "payload_probe",
    "crlf_test",
    "path_probe",
    "lfi_probe",
];
const METHOD_GATED_TOOLS: [&str; 2] = ["replay_request", "repeater_send"];
const INPUT_WRAPPERS: [&str; 3] = ["request", "overrides", "modifications"];
const SHELL_HTTP_TOOL: &str = "shell_http";
const SHELL_HTTP_CLIENTS: [&str; 7] = ["curl", "wget", "xh", "http", "https", "ncat", "nc"];

pub(crate) struct TsRiskyTools;

fn raw_input(req: &Value) -> Option<&Value> {
    req.pointer("/toolCall/rawInput")
}

fn tool_haystack(req: &Value) -> String {
    let field = |key: &str| {
        req.pointer(&format!("/toolCall/{key}"))
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    format!("{} {}", field("toolCallId"), field("title")).to_lowercase()
}

fn input_field<'a>(raw_input: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    let o = raw_input?.as_object()?;
    o.get(key).or_else(|| {
        INPUT_WRAPPERS
            .iter()
            .find_map(|w| o.get(*w)?.as_object()?.get(key))
    })
}

fn url_host(url: &str) -> Option<String> {
    let u = Url::parse(url).ok()?;
    let host = u.host_str().unwrap_or_default();
    Some(match u.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

fn shell_command(raw_input: Option<&Value>) -> Option<&str> {
    raw_input?.get("command")?.as_str()
}

/// An HTTP client in command position: line start or after ; & | ( ` — so
/// `grep curl src/` (curl as an argument) does not match.
fn is_shell_http_command(command: &str) -> bool {
    let starts = std::iter::once(0).chain(
        command
            .match_indices([';', '&', '|', '(', '`', '\n', '\r'])
            .map(|(at, sep)| at + sep.len()),
    );
    starts.filter_map(|at| command.get(at..)).any(|rest| {
        let mut words = rest.split_whitespace();
        let first = words.next();
        let client = match first {
            Some("sudo" | "env") => words.next(),
            other => other,
        };
        client.is_some_and(|client| SHELL_HTTP_CLIENTS.contains(&client))
    })
}

fn is_shell_http_request(req: &Value) -> bool {
    shell_command(raw_input(req)).is_some_and(is_shell_http_command)
}

/// `/https?:\/\/[^\s'"`)]+/`
fn first_http_url(command: &str) -> Option<&str> {
    command.match_indices("http").find_map(|(at, _)| {
        let rest = command.get(at..)?;
        let after = rest
            .strip_prefix("https://")
            .or_else(|| rest.strip_prefix("http://"))?;
        let end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '`' | ')'))
            .unwrap_or(after.len());
        (end > 0).then(|| rest.get(..rest.len() - after.len() + end))?
    })
}

fn options(req: &Value) -> &[Value] {
    req.get("options")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn kind(option: &Value) -> &str {
    option
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn option_id(option: &Value) -> String {
    option
        .get("optionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn option_of_kind(req: &Value, wanted: &str) -> Option<String> {
    options(req)
        .iter()
        .find(|o| kind(o) == wanted)
        .map(option_id)
}

fn first_option_id(req: &Value) -> String {
    options(req).first().map(option_id).unwrap_or_default()
}

impl RiskyTools for TsRiskyTools {
    fn shell_http_tool(&self) -> &str {
        SHELL_HTTP_TOOL
    }

    fn matched_risky_tool(&self, req: &Value) -> Option<String> {
        if is_shell_http_request(req) {
            return Some(SHELL_HTTP_TOOL.to_string());
        }
        let hay = tool_haystack(req);
        if let Some(tool) = ALWAYS_RISKY_TOOLS.iter().find(|t| hay.contains(**t)) {
            return Some(tool.to_string());
        }
        METHOD_GATED_TOOLS
            .iter()
            .filter(|t| hay.contains(**t))
            .find(|_| {
                // An unknown method is treated as risky (fail safe).
                let method = input_field(raw_input(req), "method").and_then(Value::as_str);
                !method.is_some_and(|m| matches!(m.to_uppercase().as_str(), "GET" | "HEAD"))
            })
            .map(|tool| tool.to_string())
    }

    fn gated_tool_name(&self, req: &Value) -> Option<String> {
        if is_shell_http_request(req) {
            return Some(SHELL_HTTP_TOOL.to_string());
        }
        let hay = tool_haystack(req);
        ALWAYS_RISKY_TOOLS
            .iter()
            .chain(&METHOD_GATED_TOOLS)
            .find(|t| hay.contains(**t))
            .map(|tool| tool.to_string())
    }

    fn extract_target_host(&self, raw_input: Option<&Value>) -> Option<String> {
        // path_probe/lfi_probe target a base URL under `baseUrl`, not `url`.
        let url = input_field(raw_input, "url")
            .filter(|url| !url.is_null())
            .or_else(|| input_field(raw_input, "baseUrl"));
        if let Some(url) = url.and_then(Value::as_str) {
            return url_host(url);
        }
        if let Some(host) = input_field(raw_input, "host").and_then(Value::as_str) {
            return Some(host.to_string());
        }
        url_host(first_http_url(shell_command(raw_input)?)?)
    }

    fn choose_allow_option(&self, req: &Value) -> String {
        let preferred = if self.gated_tool_name(req).is_some() {
            option_of_kind(req, "allow_once")
        } else {
            option_of_kind(req, "allow_always")
        };
        preferred
            .filter(|id| !id.is_empty())
            .or_else(|| {
                options(req)
                    .iter()
                    .find(|o| kind(o).starts_with("allow"))
                    .map(option_id)
            })
            .unwrap_or_else(|| first_option_id(req))
    }

    fn find_reject_option(&self, req: &Value) -> String {
        options(req)
            .iter()
            .find(|o| kind(o).starts_with("reject"))
            .map(option_id)
            .unwrap_or_else(|| first_option_id(req))
    }

    fn sanitize_selection(&self, req: &Value, picked: &str) -> String {
        let picked_kind = options(req)
            .iter()
            .find(|o| option_id(o) == picked)
            .map(kind);
        if self.gated_tool_name(req).is_none() || picked_kind != Some("allow_always") {
            return picked.to_string();
        }
        option_of_kind(req, "allow_once").unwrap_or_else(|| picked.to_string())
    }
}
