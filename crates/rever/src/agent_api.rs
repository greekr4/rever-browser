//! Port of `src/main/providers/chat-completions-provider.ts` and `anthropic-provider.ts`:
//! chat sessions that call a provider's API directly with the user's own key, running
//! the agent loop in this process.
//!
//! One turn is one thread: it streams the model's reply, runs the tools the model asks
//! for against the app's own MCP server, feeds the results back, and repeats until a
//! reply asks for no tool. The conversation is kept here, since these APIs are
//! stateless.
//!
//! Two wire formats: OpenAI's Chat Completions (OpenAI, xAI Grok and Z.ai GLM speak it)
//! and Anthropic's Messages.

use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rever_control_server::ToolProfileState;
use rever_tool_profile::direct_tool_gate::{begin_direct_turn, take_direct_tool};
use rever_tool_profile::openai_tool_cap::cap_tools_for_openai;
use serde_json::{Value, json};

use crate::agent::{AgentEvent, ChatAgent};
use crate::control_client::ControlClient;

/// The Messages API version this code is written against.
const ANTHROPIC_VERSION: &str = "2026-10-21";
/// Room for a long reply; the request streams, so no timeout is at stake.
const ANTHROPIC_MAX_TOKENS: u64 = 64_000;
/// Lets the API hand a request a safety classifier declined to another model.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    ChatCompletions,
    Anthropic,
}

/// What tells one provider from another.
#[derive(Clone, Debug)]
pub struct ApiConfig {
    /// In error messages: "OpenAI", "Grok".
    pub label: &'static str,
    pub wire: Wire,
    /// Up to, not including, `/chat/completions` or `/messages`.
    pub base_url: &'static str,
    pub default_model: &'static str,
    pub missing_key: &'static str,
}

impl ApiConfig {
    pub const OPENAI: Self = Self {
        label: "OpenAI",
        wire: Wire::ChatCompletions,
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o",
        missing_key: "No OpenAI API key set. Add one in the model picker.",
    };
    pub const GROK: Self = Self {
        label: "Grok",
        wire: Wire::ChatCompletions,
        base_url: rever_agent::grok_models::XAI_BASE_URL,
        default_model: rever_agent::grok_models::GROK_DEFAULT_MODEL,
        missing_key: "Not signed in to Grok. Run `grok login`, or add an xAI API key.",
    };
    pub const GLM: Self = Self {
        label: "GLM",
        wire: Wire::ChatCompletions,
        base_url: "https://api.z.ai/api/paas/v4",
        default_model: "glm-5.3",
        missing_key: "No Z.ai API key set. Add one in the model picker.",
    };
    pub const ANTHROPIC: Self = Self {
        label: "Anthropic",
        wire: Wire::Anthropic,
        base_url: "https://api.anthropic.com/v1",
        default_model: "claude-opus-5-5",
        missing_key: "No Anthropic API key set. Add one in the model picker.",
    };
}

/// A tool the model asked for. `input` is `Err` when its JSON did not parse.
#[derive(Debug, PartialEq)]
struct ToolCall {
    id: String,
    name: String,
    input: Result<Value, String>,
}

fn parse_input(json: &str) -> Result<Value, String> {
    let text = if json.trim().is_empty() { "{}" } else { json };
    match serde_json::from_str::<Value>(text) {
        Ok(value) if value.is_object() => Ok(value),
        Ok(_) => Err("INVALID_JSON: the tool input is not an object".to_owned()),
        Err(error) => Err(format!("INVALID_JSON: {error}")),
    }
}

// ---------------------------------------------------------------------------
// Chat Completions
// ---------------------------------------------------------------------------

/// The MCP tools the model may call this round, in the Chat Completions shape and
/// within OpenAI's cap on how many a request may carry.
fn chat_tools(tools: &[Value], allowed: &HashSet<String>) -> Vec<Value> {
    let mapped: Vec<Value> = tools
        .iter()
        .filter(|tool| {
            tool["name"]
                .as_str()
                .is_some_and(|name| allowed.contains(name))
        })
        .map(|tool| {
            json!({ "type": "function", "function": {
                "name": tool["name"],
                "description": tool["description"],
                "parameters": tool["inputSchema"],
            } })
        })
        .collect();
    cap_tools_for_openai(mapped, |tool| {
        tool["function"]["name"].as_str().unwrap_or_default()
    })
    .tools
}

fn chat_request(model: &str, messages: &[Value], tools: Vec<Value>) -> Value {
    let mut body = json!({ "model": model, "messages": messages, "stream": true });
    // An empty tool list is an error on some of these APIs.
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    body
}

/// One streamed Chat Completions reply, assembled from its chunks.
#[derive(Default)]
struct ChatReply {
    text: String,
    /// By the chunk's `index`: (id, name, the argument JSON so far).
    calls: BTreeMap<u64, (String, String, String)>,
    finish_reason: Option<String>,
}

impl ChatReply {
    /// Take one chunk in; the text it added is returned to be shown.
    fn feed(&mut self, chunk: &Value) -> Option<String> {
        let choice = chunk["choices"].get(0)?;
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish_reason = Some(reason.to_owned());
        }
        let delta = &choice["delta"];
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            let entry = self
                .calls
                .entry(call["index"].as_u64().unwrap_or(0))
                .or_default();
            if let Some(id) = call["id"].as_str().filter(|id| !id.is_empty()) {
                entry.0 = id.to_owned();
            }
            if let Some(name) = call["function"]["name"]
                .as_str()
                .filter(|name| !name.is_empty())
            {
                entry.1 = name.to_owned();
            }
            entry
                .2
                .push_str(call["function"]["arguments"].as_str().unwrap_or_default());
        }
        let text = delta["content"].as_str().filter(|text| !text.is_empty())?;
        self.text.push_str(text);
        Some(text.to_owned())
    }

    /// The assistant message to keep in the conversation, and the tools to run. No
    /// tools means the turn is over.
    fn finish(self) -> (Value, Vec<ToolCall>) {
        if self.finish_reason.as_deref() != Some("tool_calls") || self.calls.is_empty() {
            return (
                json!({ "role": "assistant", "content": self.text }),
                Vec::new(),
            );
        }
        let wire: Vec<Value> = self
            .calls
            .values()
            .map(|(id, name, arguments)| {
                let arguments = if arguments.is_empty() { "{}" } else { arguments };
                json!({ "id": id, "type": "function", "function": { "name": name, "arguments": arguments } })
            })
            .collect();
        let content = if self.text.is_empty() {
            Value::Null
        } else {
            json!(self.text)
        };
        let message = json!({ "role": "assistant", "content": content, "tool_calls": wire });
        let calls = self
            .calls
            .into_values()
            .map(|(id, name, arguments)| ToolCall {
                id,
                name,
                input: parse_input(&arguments),
            })
            .collect();
        (message, calls)
    }
}

// ---------------------------------------------------------------------------
// Anthropic Messages
// ---------------------------------------------------------------------------

/// The models whose requests can name a fallback for a declined request.
fn takes_fallbacks(model: &str) -> bool {
    [
        "claude-fable-5-2",
        "claude-fable-5-1",
        "claude-opus-5-5",
        "claude-opus-5",
        "claude-sonnet-5-5",
    ]
    .contains(&model)
}

fn anthropic_tools(tools: &[Value], allowed: &HashSet<String>) -> Vec<Value> {
    tools
        .iter()
        .filter(|tool| {
            tool["name"]
                .as_str()
                .is_some_and(|name| allowed.contains(name))
        })
        .map(|tool| {
            json!({
                "name": tool["name"],
                "description": tool["description"],
                "input_schema": tool["inputSchema"],
                // Inputs stream as they are written; a broken one is caught in `parse_input`.
                "eager_input_streaming": true,
            })
        })
        .collect()
}

/// Thinking is left to the model's default (adaptive on every current model), and the
/// stable prefix (tools, then the conversation so far) is cached.
fn anthropic_request(model: &str, messages: &[Value], tools: Vec<Value>) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": ANTHROPIC_MAX_TOKENS,
        "stream": true,
        "cache_control": { "type": "ephemeral" },
        "messages": messages,
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    if takes_fallbacks(model) {
        body["fallbacks"] = json!("default");
    }
    body
}

/// One streamed Messages reply, assembled from its events. Every block is kept as it
/// came — thinking blocks with their signatures, fallback blocks — because the API
/// wants the assistant turn handed back unchanged.
#[derive(Default)]
struct AnthropicReply {
    blocks: Vec<Value>,
    /// The input JSON of the tool-use block being streamed, by block index.
    inputs: BTreeMap<u64, String>,
    stop_reason: Option<String>,
    refusal: Option<String>,
}

impl AnthropicReply {
    /// Take one event in; the text it added is returned to be shown. An `error` event
    /// ends the reply.
    fn feed(&mut self, event: &Value) -> Result<Option<String>, String> {
        let index = event["index"].as_u64().unwrap_or(0);
        match event["type"].as_str() {
            Some("content_block_start") => {
                let at = index as usize;
                if self.blocks.len() <= at {
                    self.blocks.resize(at + 1, Value::Null);
                }
                self.blocks[at] = event["content_block"].clone();
            }
            Some("content_block_delta") => {
                let delta = &event["delta"];
                let Some(block) = self.blocks.get_mut(index as usize) else {
                    return Ok(None);
                };
                let append = |block: &mut Value, field: &str, more: &str| {
                    let so_far = block[field].as_str().unwrap_or_default().to_owned();
                    block[field] = json!(so_far + more);
                };
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let text = delta["text"].as_str().unwrap_or_default();
                        append(block, "text", text);
                        return Ok((!text.is_empty()).then(|| text.to_owned()));
                    }
                    Some("thinking_delta") => append(
                        block,
                        "thinking",
                        delta["thinking"].as_str().unwrap_or_default(),
                    ),
                    Some("signature_delta") => block["signature"] = delta["signature"].clone(),
                    Some("input_json_delta") => self
                        .inputs
                        .entry(index)
                        .or_default()
                        .push_str(delta["partial_json"].as_str().unwrap_or_default()),
                    _ => {}
                }
            }
            Some("message_delta") => {
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(reason.to_owned());
                }
                if let Some(category) = event["delta"]["stop_details"]["category"].as_str() {
                    self.refusal = Some(category.to_owned());
                }
            }
            Some("error") => {
                let message = event["error"]["message"]
                    .as_str()
                    .unwrap_or("the stream failed");
                return Err(message.to_owned());
            }
            _ => {}
        }
        Ok(None)
    }

    /// The assistant message to keep, and the tools to run. No tools means the turn is
    /// over; a reply that was declined or cut off is an error, and its tools do not run.
    fn finish(mut self) -> Result<(Value, Vec<ToolCall>), String> {
        match self.stop_reason.as_deref() {
            Some("refusal") => {
                let category = self.refusal.as_deref().unwrap_or("unspecified");
                return Err(format!("the model declined this request ({category})"));
            }
            Some("max_tokens") => return Err("the reply hit the output limit".to_owned()),
            _ => {}
        }
        let mut calls = Vec::new();
        for (index, block) in self.blocks.iter_mut().enumerate() {
            if block["type"] != "tool_use" {
                continue;
            }
            let input = parse_input(self.inputs.get(&(index as u64)).map_or("", String::as_str));
            // The block goes back to the API with the input it had; a broken one as
            // an empty object, answered with an error result.
            block["input"] = input.clone().unwrap_or_else(|_| json!({}));
            calls.push(ToolCall {
                id: block["id"].as_str().unwrap_or_default().to_owned(),
                name: block["name"].as_str().unwrap_or_default().to_owned(),
                input,
            });
        }
        let message = json!({ "role": "assistant", "content": self.blocks });
        if self.stop_reason.as_deref() != Some("tool_use") {
            calls.clear();
        }
        Ok((message, calls))
    }
}

// ---------------------------------------------------------------------------
// The session
// ---------------------------------------------------------------------------

struct Conversation {
    messages: Vec<Value>,
    /// Names that were handed to the MCP server; refusals never enter.
    dispatched: HashSet<String>,
    /// Connected at the first turn: the client and the tools it listed.
    mcp: Option<(ControlClient, Vec<Value>)>,
}

struct Shared {
    config: ApiConfig,
    api_key: Box<dyn Fn() -> Option<String> + Send + Sync>,
    mcp: Option<(String, String)>,
    profile: Option<Arc<ToolProfileState>>,
    wake: Box<dyn Fn() + Send + Sync>,
    inbox: Mutex<Vec<AgentEvent>>,
    conversation: Mutex<Conversation>,
    model: Mutex<String>,
    busy: AtomicBool,
    stopped: AtomicBool,
}

pub struct ApiSession {
    shared: Arc<Shared>,
}

impl Shared {
    fn emit(&self, event: AgentEvent) {
        self.inbox.lock().unwrap().push(event);
        (self.wake)();
    }

    /// Send one request and read its event stream, handing each `data:` payload to
    /// `each`.
    fn stream(
        &self,
        key: &str,
        body: &Value,
        mut each: impl FnMut(&Value) -> Result<(), String>,
    ) -> Result<(), String> {
        let config = &self.config;
        let http = reqwest::blocking::Client::builder()
            // A model can think for minutes before the first token.
            .timeout(Duration::from_secs(900))
            .build()
            .map_err(|error| error.to_string())?;
        let request = match config.wire {
            Wire::ChatCompletions => http
                .post(format!("{}/chat/completions", config.base_url))
                .header("Authorization", format!("Bearer {key}")),
            Wire::Anthropic => {
                let request = http
                    .post(format!("{}/messages", config.base_url))
                    .header("x-api-key", key)
                    .header("anthropic-version", ANTHROPIC_VERSION);
                match body.get("fallbacks") {
                    Some(_) => request.header("anthropic-beta", FALLBACK_BETA),
                    None => request,
                }
            }
        };
        let response = request
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .map_err(|error| format!("{}: {error}", config.label))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|json| json["error"]["message"].as_str().map(str::to_owned))
                .unwrap_or(text);
            return Err(format!("{} answered {status}: {detail}", config.label));
        }
        for line in BufReader::new(response).lines() {
            if self.stopped.load(Ordering::Relaxed) {
                return Err("stopped".to_owned());
            }
            let line = line.map_err(|error| format!("{}: {error}", config.label))?;
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data == "[DONE]" {
                break;
            }
            if let Ok(payload) = serde_json::from_str::<Value>(data) {
                each(&payload)?;
            }
        }
        Ok(())
    }

    /// One request: the assistant message it produced and the tools it asked for.
    fn round(
        &self,
        key: &str,
        model: &str,
        messages: &[Value],
        tools: &[Value],
        allowed: &HashSet<String>,
    ) -> Result<(Value, Vec<ToolCall>), String> {
        match self.config.wire {
            Wire::ChatCompletions => {
                let body = chat_request(model, messages, chat_tools(tools, allowed));
                let mut reply = ChatReply::default();
                self.stream(key, &body, |chunk| {
                    if let Some(text) = reply.feed(chunk) {
                        self.emit(AgentEvent::Text(text));
                    }
                    Ok(())
                })?;
                Ok(reply.finish())
            }
            Wire::Anthropic => {
                let body = anthropic_request(model, messages, anthropic_tools(tools, allowed));
                let mut reply = AnthropicReply::default();
                self.stream(key, &body, |event| {
                    if let Some(text) = reply.feed(event)? {
                        self.emit(AgentEvent::Text(text));
                    }
                    Ok(())
                })?;
                reply.finish()
            }
        }
    }

    /// The whole turn: rounds until a reply asks for no tool.
    fn turn(&self, text: &str) -> Result<(), String> {
        let key = (self.api_key)().ok_or(self.config.missing_key)?;
        let mut conversation = self.conversation.lock().unwrap();
        if conversation.mcp.is_none()
            && let Some((url, auth)) = &self.mcp
        {
            let mut client = ControlClient::connect(url, auth)?;
            let tools = client.list_tools()?;
            conversation.mcp = Some((client, tools));
        }
        conversation
            .messages
            .push(json!({ "role": "user", "content": text }));
        loop {
            let model = self.model.lock().unwrap().clone();
            let tools = conversation
                .mcp
                .as_ref()
                .map(|(_, tools)| tools.clone())
                .unwrap_or_default();
            // Without a profile (no MCP server) there are no tools to gate.
            let allowed = match &self.profile {
                Some(profile) => begin_direct_turn(profile.get(), &conversation.dispatched),
                None => HashSet::new(),
            };
            let (message, calls) =
                self.round(&key, &model, &conversation.messages, &tools, &allowed)?;
            conversation.messages.push(message);
            if calls.is_empty() {
                return Ok(());
            }
            let mut results = Vec::new();
            for call in calls {
                let shown = call.input.clone().unwrap_or(Value::Null);
                self.emit(AgentEvent::ToolCall(call.name.clone(), shown));
                let (output, failed) = self.run_tool(&mut conversation, &call, &allowed);
                self.emit(AgentEvent::ToolResult(call.name.clone(), output.clone()));
                results.push((call.id, output, failed));
            }
            // Chat Completions takes one message per result; Messages takes them all in
            // one user turn.
            match self.config.wire {
                Wire::ChatCompletions => {
                    for (id, output, _) in results {
                        conversation
                            .messages
                            .push(json!({ "role": "tool", "tool_call_id": id, "content": output }));
                    }
                }
                Wire::Anthropic => {
                    let blocks: Vec<Value> = results
                        .into_iter()
                        .map(|(id, output, failed)| {
                            json!({ "type": "tool_result", "tool_use_id": id, "content": output, "is_error": failed })
                        })
                        .collect();
                    conversation
                        .messages
                        .push(json!({ "role": "user", "content": blocks }));
                }
            }
        }
    }

    /// The tool's output and whether it failed. A refused or broken call is a failed
    /// result the model reads, not an error of the turn.
    fn run_tool(
        &self,
        conversation: &mut Conversation,
        call: &ToolCall,
        allowed: &HashSet<String>,
    ) -> (String, bool) {
        let input = match &call.input {
            Ok(input) => input,
            Err(error) => return (error.clone(), true),
        };
        if let Err(refusal) = take_direct_tool(&call.name, allowed, &mut conversation.dispatched) {
            return (refusal, true);
        }
        match conversation.mcp.as_mut() {
            Some((client, _)) => client
                .call_tool(&call.name, input)
                .unwrap_or_else(|error| (error, true)),
            None => ("no tool server".to_owned(), true),
        }
    }
}

impl ApiSession {
    /// `api_key` is asked at every turn, so a key added meanwhile is picked up. `mcp` is
    /// the app's MCP server as (url, Authorization header), and `profile` its tool
    /// profile, which decides the tools each request offers.
    pub fn new(
        config: ApiConfig,
        model: Option<&str>,
        api_key: impl Fn() -> Option<String> + Send + Sync + 'static,
        mcp: Option<(&str, &str)>,
        profile: Option<Arc<ToolProfileState>>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let model = model.unwrap_or(config.default_model).to_owned();
        Self {
            shared: Arc::new(Shared {
                config,
                api_key: Box::new(api_key),
                mcp: mcp.map(|(url, auth)| (url.to_owned(), auth.to_owned())),
                profile,
                wake: Box::new(wake),
                inbox: Mutex::new(Vec::new()),
                conversation: Mutex::new(Conversation {
                    messages: Vec::new(),
                    dispatched: HashSet::new(),
                    mcp: None,
                }),
                model: Mutex::new(model),
                busy: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
            }),
        }
    }
}

impl ChatAgent for ApiSession {
    fn send(&mut self, text: &str) -> Result<(), String> {
        if self.shared.busy.swap(true, Ordering::SeqCst) {
            return Err("the previous turn is still running".to_owned());
        }
        let (shared, text) = (self.shared.clone(), text.to_owned());
        std::thread::spawn(move || {
            let outcome = shared.turn(&text);
            shared.busy.store(false, Ordering::SeqCst);
            shared.emit(match outcome {
                Ok(()) => AgentEvent::Done,
                Err(error) => AgentEvent::Failed(error),
            });
        });
        Ok(())
    }

    fn drain(&mut self) -> Vec<AgentEvent> {
        std::mem::take(&mut *self.shared.inbox.lock().unwrap())
    }

    fn set_model(&mut self, model: &str) -> Result<(), String> {
        *self.shared.model.lock().unwrap() = model.to_owned();
        Ok(())
    }
}

impl Drop for ApiSession {
    fn drop(&mut self) {
        // The turn thread stops at the next line of its stream.
        self.shared.stopped.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn mcp_tools() -> Vec<Value> {
        vec![
            json!({ "name": "list_requests", "description": "List", "inputSchema": { "type": "object" } }),
            json!({ "name": "intruder_run", "description": "Fuzz", "inputSchema": { "type": "object" } }),
        ]
    }

    #[test]
    fn 허용된_도구만_각_api의_형식으로_요청에_넣는다() {
        let chat = chat_tools(&mcp_tools(), &allowed(&["list_requests"]));
        assert_eq!(chat.len(), 1);
        assert_eq!(chat[0]["type"], "function");
        assert_eq!(chat[0]["function"]["name"], "list_requests");
        assert_eq!(chat[0]["function"]["parameters"]["type"], "object");

        let anthropic = anthropic_tools(&mcp_tools(), &allowed(&["list_requests"]));
        assert_eq!(anthropic.len(), 1);
        assert_eq!(anthropic[0]["input_schema"]["type"], "object");
        assert_eq!(anthropic[0]["eager_input_streaming"], true);
    }

    #[test]
    fn 도구가_없으면_요청에_tools를_넣지_않는다() {
        let body = chat_request("gpt-4o", &[], Vec::new());
        assert!(body.get("tools").is_none());
        assert_eq!(body["stream"], true);
        let body = anthropic_request("claude-haiku-5-5", &[], Vec::new());
        assert!(body.get("tools").is_none());
        // 생각하기 설정은 보내지 않는다: 지금 모델들의 기본값(adaptive)을 쓴다.
        assert!(body.get("thinking").is_none());
        // 거절 대체 모델을 받는 모델에만 fallbacks를 넣는다.
        assert!(body.get("fallbacks").is_none());
        assert_eq!(
            anthropic_request("claude-opus-5-5", &[], Vec::new())["fallbacks"],
            "default"
        );
    }

    #[test]
    fn 채팅_컴플리션_조각을_모아_글과_도구_호출을_만든다() {
        let mut reply = ChatReply::default();
        let chunk = |delta: Value, finish: Value| json!({ "choices": [{ "delta": delta, "finish_reason": finish }] });
        assert_eq!(
            reply.feed(&chunk(json!({ "content": "확인" }), Value::Null)),
            Some("확인".into())
        );
        // 도구 호출은 조각마다 인자 JSON이 이어 붙는다.
        reply.feed(&chunk(
            json!({ "tool_calls": [{ "index": 0, "id": "c1", "function": { "name": "list_requests", "arguments": "{\"li" } }] }),
            Value::Null,
        ));
        reply.feed(&chunk(
            json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "mit\":5}" } }] }),
            json!("tool_calls"),
        ));
        // 사용량만 담은 마지막 조각에는 choices가 없다.
        assert_eq!(reply.feed(&json!({ "choices": [], "usage": {} })), None);
        let (message, calls) = reply.finish();
        assert_eq!(message["content"], "확인");
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            "{\"limit\":5}"
        );
        assert_eq!(
            calls,
            [ToolCall {
                id: "c1".into(),
                name: "list_requests".into(),
                input: Ok(json!({ "limit": 5 }))
            }]
        );
    }

    #[test]
    fn 도구를_부르지_않은_답은_턴의_끝이다() {
        let mut reply = ChatReply::default();
        reply.feed(
            &json!({ "choices": [{ "delta": { "content": "끝" }, "finish_reason": "stop" }] }),
        );
        let (message, calls) = reply.finish();
        assert_eq!(message, json!({ "role": "assistant", "content": "끝" }));
        assert!(calls.is_empty());
    }

    #[test]
    fn 메시지_api_이벤트를_모아_블록을_그대로_되돌려줄_수_있게_만든다() {
        let mut reply = AnthropicReply::default();
        let events = [
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "", "signature": "" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta", "signature": "sig" } }),
            json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": "확인" } }),
            json!({ "type": "content_block_start", "index": 2, "content_block": { "type": "tool_use", "id": "t1", "name": "list_requests", "input": {} } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "{\"limit\"" } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": ":5}" } }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" } }),
        ];
        let shown: Vec<String> = events
            .iter()
            .filter_map(|event| reply.feed(event).unwrap())
            .collect();
        assert_eq!(shown, ["확인"]);
        let (message, calls) = reply.finish().unwrap();
        // 생각 블록은 서명과 함께 그대로, 도구 블록은 파싱된 입력으로 남는다.
        assert_eq!(message["content"][0]["signature"], "sig");
        assert_eq!(message["content"][1]["text"], "확인");
        assert_eq!(message["content"][2]["input"], json!({ "limit": 5 }));
        assert_eq!(
            calls,
            [ToolCall {
                id: "t1".into(),
                name: "list_requests".into(),
                input: Ok(json!({ "limit": 5 }))
            }]
        );
    }

    #[test]
    fn 거절되거나_잘린_답은_도구를_돌리지_않고_오류다() {
        let mut refused = AnthropicReply::default();
        refused
            .feed(&json!({ "type": "message_delta", "delta": { "stop_reason": "refusal", "stop_details": { "category": "cyber" } } }))
            .unwrap();
        assert_eq!(
            refused.finish().unwrap_err(),
            "the model declined this request (cyber)"
        );

        let mut cut = AnthropicReply::default();
        cut.feed(&json!({ "type": "message_delta", "delta": { "stop_reason": "max_tokens" } }))
            .unwrap();
        assert!(cut.finish().is_err());

        let mut failed = AnthropicReply::default();
        let error = json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } });
        assert_eq!(failed.feed(&error), Err("Overloaded".into()));
    }

    #[test]
    fn 깨진_도구_입력은_빈_객체로_되돌리고_오류로_표시한다() {
        assert_eq!(parse_input(""), Ok(json!({})));
        assert!(
            parse_input("{\"a\":")
                .unwrap_err()
                .starts_with("INVALID_JSON")
        );
        assert!(parse_input("[1]").unwrap_err().starts_with("INVALID_JSON"));

        let mut reply = AnthropicReply::default();
        for event in [
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "tool_use", "id": "t1", "name": "x", "input": {} } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "input_json_delta", "partial_json": "{\"a\":" } }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" } }),
        ] {
            reply.feed(&event).unwrap();
        }
        let (message, calls) = reply.finish().unwrap();
        assert_eq!(message["content"][0]["input"], json!({}));
        assert!(calls[0].input.is_err());
    }
}
