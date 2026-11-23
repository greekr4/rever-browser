//! A small blocking client for the app's own MCP server, for the sessions that run a
//! provider's API directly: they list the tools once and call them as the model asks.
//!
//! It connects with the `direct` catalog, as the Electron providers did: every tool is
//! listed, and the session decides per turn which ones the model may call.

use std::time::Duration;

use serde_json::{Value, json};

pub struct McpClient {
    http: reqwest::blocking::Client,
    url: String,
    auth_header: String,
    session: Option<String>,
    next_id: u64,
}

/// The answer to request `id` in a response body, which is either the JSON message
/// itself or a server-sent event stream carrying it in a `data:` line.
fn answer(body: &str, id: u64) -> Result<Value, String> {
    let lines = body.lines().filter_map(|line| line.strip_prefix("data:"));
    let message = std::iter::once(body)
        .chain(lines)
        .filter_map(|text| serde_json::from_str::<Value>(text.trim()).ok())
        .find(|message| message["id"].as_u64() == Some(id))
        .ok_or_else(|| "the MCP server sent no answer".to_owned())?;
    match message.get("error") {
        Some(error) => Err(error["message"].as_str().unwrap_or("MCP error").to_owned()),
        None => Ok(message["result"].clone()),
    }
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

impl McpClient {
    pub fn connect(url: &str, auth_header: &str) -> Result<Self, String> {
        let http = reqwest::blocking::Client::builder()
            // A tool may wait on the page (a navigation, a promise) for minutes.
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|error| error.to_string())?;
        let mut client = Self {
            http,
            url: url.to_owned(),
            auth_header: auth_header.to_owned(),
            session: None,
            next_id: 1,
        };
        let hello = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "rever-direct", "version": env!("CARGO_PKG_VERSION") },
        });
        client.call("initialize", hello)?;
        client.post(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        Ok(client)
    }

    fn post(&mut self, message: &Value) -> Result<String, String> {
        let mut request = self
            .http
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("Authorization", &self.auth_header)
            .header("X-Rever-Catalog", "direct");
        if let Some(session) = &self.session {
            request = request.header("Mcp-Session-Id", session);
        }
        let response = request
            .body(message.to_string())
            .send()
            .map_err(|error| format!("MCP server: {error}"))?;
        if let Some(session) = response.headers().get("mcp-session-id") {
            self.session = session.to_str().ok().map(str::to_owned);
        }
        let status = response.status();
        let body = response.text().map_err(|error| error.to_string())?;
        if !status.is_success() {
            return Err(format!("MCP server answered {status}: {body}"));
        }
        Ok(body)
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let body =
            self.post(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        answer(&body, id)
    }

    /// Every tool as the server describes it: `name`, `description`, `inputSchema`.
    pub fn list_tools(&mut self) -> Result<Vec<Value>, String> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor.map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
            let page = self.call("tools/list", params)?;
            tools.extend(page["tools"].as_array().cloned().unwrap_or_default());
            cursor = page["nextCursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
    }

    /// Run a tool: its text, and whether the tool reported an error.
    pub fn call_tool(&mut self, name: &str, arguments: &Value) -> Result<(String, bool), String> {
        let result = self.call(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )?;
        Ok(tool_text(&result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 응답은_json_본문이나_이벤트_스트림의_data_줄에서_찾는다() {
        let plain = r#"{"jsonrpc":"2.0","id":3,"result":{"tools":[]}}"#;
        assert_eq!(answer(plain, 3).unwrap(), json!({ "tools": [] }));
        let stream = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"ok\":true}}\n\n";
        assert_eq!(answer(stream, 3).unwrap(), json!({ "ok": true }));
        // 다른 요청의 응답은 이 요청의 답이 아니다.
        assert!(answer(plain, 4).is_err());
    }

    #[test]
    fn 오류_응답은_그_메시지로_실패한다() {
        let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"bad arguments"}}"#;
        assert_eq!(answer(body, 1), Err("bad arguments".into()));
    }

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
