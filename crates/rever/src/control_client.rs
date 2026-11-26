//! A small blocking client for the app's own control socket, for the sessions
//! that run a provider's API directly: they list the tools once and call them as
//! the model asks.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde_json::{Value, json};

/// A tool may wait on the page (a navigation, a promise) for minutes.
const TOOL_TIMEOUT: Duration = Duration::from_secs(600);

pub struct ControlClient {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    token: String,
}

/// A tool result as the model reads it: the text blocks joined, and whether it failed.
fn tool_text(result: &Value) -> (String, bool) {
    let parts: Vec<&str> = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect();
    (parts.join("\n"), result["isError"] == true)
}

impl ControlClient {
    /// `path` is the socket the app published; `token` comes from the same file.
    pub fn connect(path: &str, token: &str) -> Result<Self, String> {
        // The endpoint file carries the HTTP-era `Bearer …` header; the socket
        // ticket is just the token.
        let token = token.trim_start_matches("Bearer ").trim();
        let stream = UnixStream::connect(path)
            .map_err(|error| format!("control socket {path}: {error}"))?;
        stream
            .set_read_timeout(Some(TOOL_TIMEOUT))
            .map_err(|error| error.to_string())?;
        let reader = BufReader::new(stream.try_clone().map_err(|error| error.to_string())?);
        Ok(Self {
            writer: stream,
            reader,
            token: token.to_owned(),
        })
    }

    fn request(&mut self, tool: &str, args: Value) -> Result<Value, String> {
        let line = json!({ "token": self.token, "tool": tool, "args": args }).to_string();
        self.writer
            .write_all(format!("{line}\n").as_bytes())
            .map_err(|error| format!("control socket: {error}"))?;
        self.writer
            .flush()
            .map_err(|error| format!("control socket: {error}"))?;
        let mut reply = String::new();
        self.reader
            .read_line(&mut reply)
            .map_err(|error| format!("control socket: {error}"))?;
        if reply.is_empty() {
            return Err("the app closed the control socket".to_owned());
        }
        let reply: Value = serde_json::from_str(&reply).map_err(|error| error.to_string())?;
        if reply["ok"] != true {
            return Err(reply["error"]
                .as_str()
                .unwrap_or("the tool call failed")
                .to_owned());
        }
        Ok(reply["result"].clone())
    }

    /// Every tool as the app describes it: `name`, `description`, `inputSchema`.
    pub fn list_tools(&mut self) -> Result<Vec<Value>, String> {
        let catalog = self.request("__catalog", Value::Null)?;
        Ok(catalog["tools"].as_array().cloned().unwrap_or_default())
    }

    /// Run a tool: its text, and whether the tool reported an error.
    pub fn call_tool(&mut self, name: &str, arguments: &Value) -> Result<(String, bool), String> {
        let result = self.request(name, arguments.clone())?;
        Ok(tool_text(&result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 도구_결과는_글_블록만_이어_붙이고_오류_여부를_함께_준다() {
        let result = json!({ "content": [
            { "type": "text", "text": "첫 줄" },
            { "type": "image", "data": "..." },
            { "type": "text", "text": "둘째 줄" },
        ], "isError": true });
        assert_eq!(tool_text(&result), ("첫 줄\n둘째 줄".to_owned(), true));
        assert_eq!(tool_text(&json!({})), (String::new(), false));
    }
}
