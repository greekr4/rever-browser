//! Port of `src/main/mcp/tools/tool_profile.ts` — `tool_profile_set`: the agent
//! reads the in-app tool profile or switches it between browse and reverse.

use rever_tool_profile::tool_profile::{
    TOOL_PROFILE_SET_DESCRIPTION, ToolProfile, advertised_names, tool_profile_read_text,
    tool_profile_switched_text,
};
use rever_tools_api::tool::{BoxFuture, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{ToolResult, err, ok};
use serde_json::{Value, json};

use crate::tool_profile_state::{ProfileSource, ToolProfileState};
use crate::validation::{invalid_arguments, object};

pub struct ToolProfileSet;

impl Tool for ToolProfileSet {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "tool_profile_set",
            TOOL_PROFILE_SET_DESCRIPTION,
            json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "profile": {
                        "description": "Omit to read. browse or reverse switches. probe is rejected for the agent.",
                        "type": "string",
                        "enum": ["browse", "reverse", "probe"]
                    }
                }
            }),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { tool_profile_set(&ToolProfileState::of(ctx), &args) })
    }
}

fn tool_profile_set(state: &ToolProfileState, args: &Value) -> ToolResult {
    let args = match object(args) {
        Ok(args) => args,
        Err(issue) => return invalid_arguments("tool_profile_set", &[issue]),
    };
    let Some(value) = args.get("profile") else {
        let current = state.get();
        return ok(tool_profile_read_text(
            current,
            advertised_names(current, []).len(),
        ));
    };
    // Only a missing key reads: an explicit null is a wrong value, as it was to zod.
    let Some(profile) = value.as_str().and_then(ToolProfile::parse) else {
        return invalid_arguments(
            "tool_profile_set",
            &[json!({
                "code": "invalid_value",
                "values": ["browse", "reverse", "probe"],
                "path": ["profile"],
                "message": "Invalid option: expected one of \"browse\"|\"reverse\"|\"probe\"",
            })],
        );
    };
    match state.set(profile, ProfileSource::Agent) {
        Ok(change) => ok(tool_profile_switched_text(
            change.profile,
            change.advertised,
        )),
        Err(error) => err(error),
    }
}

pub fn register_tool_profile_tools(registry: &mut ToolRegistry) {
    registry.register(ToolProfileSet);
}

#[cfg(test)]
mod tests {
    use rever_tool_profile::tool_profile::TOOL_PROFILE_PROBE_REJECTED;

    use super::*;
    use crate::tool_profile_state::tests::TempDir;

    fn state() -> (TempDir, ToolProfileState) {
        let dir = TempDir::new("tool-profile-set");
        let state = ToolProfileState::new(&dir.0);
        (dir, state)
    }

    #[test]
    fn profile을_생략하면_현재_프로파일과_도구_수를_읽는다() {
        let (_dir, state) = state();
        let advertised = advertised_names(ToolProfile::Browse, []).len();
        assert_eq!(
            tool_profile_set(&state, &json!({})),
            ok(format!(
                "profile=browse advertised={advertised}. Switch with profile \"browse\" or \"reverse\". You cannot select probe; the user sets Tools → Access. bp_* is not in any profile."
            ))
        );
        assert_eq!(state.get(), ToolProfile::Browse);
    }

    #[test]
    fn reverse로_바꾸면_바뀐_프로파일과_도구_수를_알린다() {
        let (_dir, state) = state();
        let advertised = advertised_names(ToolProfile::Reverse, []).len();
        assert_eq!(
            tool_profile_set(&state, &json!({ "profile": "reverse" })),
            ok(format!(
                "profile=reverse advertised={advertised}. If grep_scripts is not in your tool list yet, ask the user to send one more message."
            ))
        );
        assert_eq!(state.get(), ToolProfile::Reverse);

        // Reading afterwards reports the new profile.
        let read = tool_profile_set(&state, &json!({}));
        assert_eq!(
            read,
            ok(tool_profile_read_text(ToolProfile::Reverse, advertised))
        );
    }

    #[test]
    fn 에이전트가_probe를_고르면_거절하고_프로파일을_그대로_둔다() {
        let (_dir, state) = state();
        assert_eq!(
            tool_profile_set(&state, &json!({ "profile": "probe" })),
            err(TOOL_PROFILE_PROBE_REJECTED)
        );
        assert_eq!(state.get(), ToolProfile::Browse);
    }

    #[test]
    fn 세_가지가_아닌_값은_sdk와_같은_검증_오류로_거절한다() {
        let (_dir, state) = state();
        // The text the TypeScript SDK returned for the same arguments (zod 4).
        let expected = err(
            "MCP error -32602: Input validation error: Invalid arguments for tool tool_profile_set: [\n  {\n    \"code\": \"invalid_value\",\n    \"values\": [\n      \"browse\",\n      \"reverse\",\n      \"probe\"\n    ],\n    \"path\": [\n      \"profile\"\n    ],\n    \"message\": \"Invalid option: expected one of \\\"browse\\\"|\\\"reverse\\\"|\\\"probe\\\"\"\n  }\n]",
        );
        for bad in [json!("Browse"), json!(null), json!(1), json!(["browse"])] {
            assert_eq!(
                tool_profile_set(&state, &json!({ "profile": bad })),
                expected
            );
        }
        assert_eq!(state.get(), ToolProfile::Browse);
    }

    #[test]
    fn 인자가_객체가_아니면_거절한다() {
        let (_dir, state) = state();
        assert_eq!(
            tool_profile_set(&state, &Value::Null),
            err(
                "MCP error -32602: Input validation error: Invalid arguments for tool tool_profile_set: [\n  {\n    \"expected\": \"object\",\n    \"code\": \"invalid_type\",\n    \"path\": [],\n    \"message\": \"Invalid input: expected object, received undefined\"\n  }\n]"
            )
        );
        let result = tool_profile_set(&state, &json!("reverse"));
        assert!(result.is_error);
        assert_eq!(state.get(), ToolProfile::Browse);
    }

    #[test]
    fn 모르는_키는_무시한다() {
        let (_dir, state) = state();
        let result = tool_profile_set(&state, &json!({ "profile": "reverse", "extra": true }));
        assert!(!result.is_error);
        assert_eq!(state.get(), ToolProfile::Reverse);
    }

    #[test]
    fn 정의는_이름과_설명과_스키마를_그대로_싣는다() {
        let def = ToolProfileSet.def();
        assert_eq!(def.name, "tool_profile_set");
        assert_eq!(def.description, TOOL_PROFILE_SET_DESCRIPTION);
        // What the TypeScript SDK listed for the zod shape (printed by node).
        assert_eq!(
            def.input_schema.to_string(),
            r##"{"$schema":"http://json-schema.org/draft-07/schema#","type":"object","properties":{"profile":{"description":"Omit to read. browse or reverse switches. probe is rejected for the agent.","type":"string","enum":["browse","reverse","probe"]}}}"##
        );

        let mut registry = ToolRegistry::default();
        register_tool_profile_tools(&mut registry);
        assert!(registry.find("tool_profile_set").is_some());
    }

    #[test]
    fn 도구는_컨텍스트의_상태를_쓴다() {
        let dir = TempDir::new("tool-profile-set");
        let context = ToolContext::new(
            Default::default(),
            std::sync::Arc::new(rever_tools_api::tool::NoBrowser),
            dir.0.clone(),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let result =
            runtime.block_on(ToolProfileSet.call(&context, json!({ "profile": "reverse" })));
        assert!(!result.is_error);
        assert_eq!(ToolProfileState::of(&context).get(), ToolProfile::Reverse);
    }
}
