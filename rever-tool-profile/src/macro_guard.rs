//! Port of `src/main/mcp/tools/macro-guard.ts` — screens an agent-invoked `run_macro` for steps that must prompt.
//!
//! The TypeScript awaits `resolve` and `lookup`; here they are plain closures.
//! A rejected promise is an `Err` from `resolve` and a `None` from `lookup`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::risky_tools::{ALWAYS_RISKY_TOOLS, METHOD_GATED_TOOLS, extract_method};
use crate::tool_profile::CatalogKind;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MacroStep {
    pub tool: String,
    /// Stored macros are user- and agent-written JSON, so the input is kept as
    /// found; anything that is not an object simply has no fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
}

impl MacroStep {
    fn input_str(&self, key: &str) -> Option<&str> {
        self.input.as_ref()?.as_object()?.get(key)?.as_str()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliEffect {
    Read,
    Write,
    Unknown,
}

/// `(id, command)` → what the compiled CLI command does. `None` when the
/// lookup itself failed, which the guard treats like any non-read answer.
pub type CliEffectLookup<'a> = dyn Fn(&str, &str) -> Option<CliEffect> + 'a;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ForbiddenStep {
    pub index: usize,
    pub tool: String,
}

pub fn macro_run_needs_guard(catalog: CatalogKind) -> bool {
    matches!(catalog, CatalogKind::Follow | CatalogKind::Direct)
}

pub fn macro_run_refusal(index: usize, tool: &str) -> String {
    format!(
        "run_macro refused step {index} ({tool}): agent-invoked macros cannot run ALWAYS_RISKY_TOOLS, a non-GET replay_request/repeater_send, cli_writes, or a non-read cli_run. Call that tool directly so ACP can prompt, or so an API model can run it once it is advertised. The Workflows panel is unchanged."
    )
}

fn cli_step_forbidden(step: &MacroStep, lookup: Option<&CliEffectLookup>) -> bool {
    if step.tool == "cli_writes" {
        return true;
    }
    if step.tool != "cli_run" {
        return false;
    }
    let Some(lookup) = lookup else { return true };
    match (step.input_str("id"), step.input_str("command")) {
        (Some(id), Some(command)) if !id.is_empty() && !command.is_empty() => {
            lookup(id, command) != Some(CliEffect::Read)
        }
        _ => true,
    }
}

fn is_forbidden_leaf(step: &MacroStep) -> bool {
    let tool = step.tool.as_str();
    if ALWAYS_RISKY_TOOLS.contains(&tool) {
        return true;
    }
    if !METHOD_GATED_TOOLS.contains(&tool) {
        return false;
    }
    let method = step.input.as_ref().and_then(extract_method);
    !method.is_some_and(|m| matches!(m.to_uppercase().as_str(), "GET" | "HEAD"))
}

/// The first step that may not run, or `None`. A nested `run_macro` is reported
/// at the outer step's index with the inner tool's name. `stack` holds the ids
/// of the macros being expanded; an id already on it is a cycle.
///
/// The TypeScript recurses through awaits, which does not deepen the call
/// stack; this walks an explicit stack so a long chain of macros calling
/// macros cannot overflow it either.
pub fn find_forbidden_macro_step<E>(
    steps: &[MacroStep],
    resolve: &mut dyn FnMut(&str) -> Result<Vec<MacroStep>, E>,
    stack: &[String],
    lookup: Option<&CliEffectLookup>,
) -> Result<Option<ForbiddenStep>, E> {
    let mut ids = stack.to_vec();
    // The macros being expanded below the top level, each with its next step.
    let mut nested: Vec<(Vec<MacroStep>, usize)> = Vec::new();
    let mut top = 0;
    loop {
        let step = match nested.last_mut() {
            Some((inner, next)) => {
                *next += 1;
                inner.get(*next - 1)
            }
            None => steps.get(top),
        };
        let Some(step) = step else {
            if nested.pop().is_none() {
                return Ok(None);
            }
            ids.pop();
            // The top-level run_macro step that opened this chain is done.
            if nested.is_empty() {
                top += 1;
            }
            continue;
        };
        let hit = |tool: &str| {
            Ok(Some(ForbiddenStep {
                index: top,
                tool: tool.to_owned(),
            }))
        };
        if step.tool == "run_macro" {
            let id = match step.input_str("id") {
                Some(id) if !ids.iter().any(|s| s == id) => id.to_owned(),
                _ => return hit("run_macro"),
            };
            let inner = resolve(&id)?;
            ids.push(id);
            nested.push((inner, 0));
            continue;
        }
        if is_forbidden_leaf(step) || cli_step_forbidden(step, lookup) {
            return hit(&step.tool);
        }
        if nested.is_empty() {
            top += 1;
        }
    }
}

/// follow/direct `run_macro` calls this before running the workflow.
/// `Ok(Ok(()))` is the only path through: `Ok(Err(text))` is the refusal to
/// show the agent, and the outer `Err` is `resolve` failing.
pub fn screen_macro_run<E>(
    steps: &[MacroStep],
    resolve: &mut dyn FnMut(&str) -> Result<Vec<MacroStep>, E>,
    stack: &[String],
    lookup: Option<&CliEffectLookup>,
) -> Result<Result<(), String>, E> {
    Ok(
        match find_forbidden_macro_step(steps, resolve, stack, lookup)? {
            None => Ok(()),
            Some(hit) => Err(macro_run_refusal(hit.index, &hit.tool)),
        },
    )
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use serde_json::json;

    use super::*;

    fn step(tool: &str) -> MacroStep {
        MacroStep {
            tool: tool.to_owned(),
            input: None,
        }
    }

    fn step_with(tool: &str, input: Value) -> MacroStep {
        MacroStep {
            tool: tool.to_owned(),
            input: Some(input),
        }
    }

    fn stack(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    fn noop_resolve(_id: &str) -> Result<Vec<MacroStep>, Infallible> {
        Ok(Vec::new())
    }

    fn forbidden(index: usize, tool: &str) -> Option<ForbiddenStep> {
        Some(ForbiddenStep {
            index,
            tool: tool.to_owned(),
        })
    }

    /// `findForbiddenMacroStep(steps, noopResolve, stack, lookup?)`.
    fn find(
        steps: &[MacroStep],
        ids: &[&str],
        lookup: Option<&CliEffectLookup>,
    ) -> Option<ForbiddenStep> {
        match find_forbidden_macro_step(steps, &mut noop_resolve, &stack(ids), lookup) {
            Ok(hit) => hit,
            Err(never) => match never {},
        }
    }

    #[test]
    fn full_카탈로그는_검사하지_않고_follow와_direct만_검사한다() {
        assert!(!macro_run_needs_guard(CatalogKind::Full));
        assert!(macro_run_needs_guard(CatalogKind::Follow));
        assert!(macro_run_needs_guard(CatalogKind::Direct));
    }

    #[test]
    fn 중첩_run_macro가_intruder_run이면_그_스텝을_거부하고_실행_경로를_열지_않는다() {
        let steps = [step_with("run_macro", json!({ "id": "A" }))];
        let mut resolve = |id: &str| -> Result<Vec<MacroStep>, Infallible> {
            assert_eq!(id, "A");
            Ok(vec![step("intruder_run")])
        };
        let hit = find_forbidden_macro_step(&steps, &mut resolve, &stack(&["B"]), None);
        assert_eq!(hit, Ok(forbidden(0, "intruder_run")));
        let screen = screen_macro_run(&steps, &mut resolve, &stack(&["B"]), None);
        assert_eq!(screen, Ok(Err(macro_run_refusal(0, "intruder_run"))));
    }

    #[test]
    fn 안쪽이_browser_snapshot뿐이면_none이라_실행으로_진행한다() {
        let steps = [step_with("run_macro", json!({ "id": "A" }))];
        let mut resolve =
            |_: &str| -> Result<Vec<MacroStep>, Infallible> { Ok(vec![step("browser_snapshot")]) };
        assert_eq!(
            find_forbidden_macro_step(&steps, &mut resolve, &stack(&["B"]), None),
            Ok(None)
        );
        assert_eq!(
            screen_macro_run(&steps, &mut resolve, &stack(&["B"]), None),
            Ok(Ok(()))
        );
    }

    #[test]
    fn 메서드가_없거나_get_head가_아닌_replay는_거부하고_get은_통과한다() {
        assert_eq!(
            find(&[step("replay_request")], &[], None),
            forbidden(0, "replay_request")
        );
        assert_eq!(
            find(
                &[step_with("repeater_send", json!({ "method": "post" }))],
                &[],
                None
            ),
            forbidden(0, "repeater_send")
        );
        assert_eq!(
            find(
                &[step_with("replay_request", json!({ "method": "GET" }))],
                &[],
                None
            ),
            None
        );
        assert_eq!(
            find(
                &[step_with(
                    "repeater_send",
                    json!({ "modifications": { "method": "head" } })
                )],
                &[],
                None
            ),
            None
        );
    }

    #[test]
    fn 이름_일부만_겹치는_도구와_순환_없는_id는_각각_통과하거나_run_macro로_거부한다() {
        assert_eq!(find(&[step("not_intruder_run_helper")], &[], None), None);
        assert_eq!(
            find(
                &[step_with("run_macro", json!({ "id": "A" }))],
                &["A"],
                None
            ),
            forbidden(0, "run_macro")
        );
        assert_eq!(
            find(&[step_with("run_macro", json!({}))], &["B"], None),
            forbidden(0, "run_macro")
        );
    }

    #[test]
    fn cli_writes와_쓰거나_알_수_없는_cli_run은_거부하고_읽기만_통과한다() {
        let lookup = |_id: &str, command: &str| {
            Some(match command {
                "get-echo" => CliEffect::Read,
                "post-login" => CliEffect::Write,
                _ => CliEffect::Unknown,
            })
        };
        assert_eq!(
            find(&[step_with("cli_writes", json!({ "id": "c1" }))], &[], None),
            forbidden(0, "cli_writes")
        );
        assert_eq!(
            find(
                &[step_with(
                    "cli_run",
                    json!({ "id": "c1", "command": "post-login" })
                )],
                &[],
                Some(&lookup)
            ),
            forbidden(0, "cli_run")
        );
        assert_eq!(
            find(
                &[step_with(
                    "cli_run",
                    json!({ "id": "c1", "command": "mystery" })
                )],
                &[],
                Some(&lookup)
            ),
            forbidden(0, "cli_run")
        );
        assert_eq!(
            find(
                &[step_with(
                    "cli_run",
                    json!({ "id": "c1", "command": "get-echo" })
                )],
                &[],
                Some(&lookup)
            ),
            None
        );
        assert_eq!(
            find(
                &[step_with("cli_run", json!({ "id": "c1" }))],
                &[],
                Some(&lookup)
            ),
            forbidden(0, "cli_run")
        );
        // The TypeScript lookup throws here; a failed lookup is `None`.
        let throwing = |_: &str, _: &str| -> Option<CliEffect> { None };
        assert_eq!(
            find(
                &[step_with(
                    "cli_run",
                    json!({ "id": "c1", "command": "get-echo" })
                )],
                &[],
                Some(&throwing)
            ),
            forbidden(0, "cli_run")
        );
    }

    #[test]
    fn lookup이_없으면_cli_run을_거부한다() {
        assert_eq!(
            find(
                &[step_with(
                    "cli_run",
                    json!({ "id": "c1", "command": "get-echo" })
                )],
                &[],
                None
            ),
            forbidden(0, "cli_run")
        );
    }

    #[test]
    fn 중첩_매크로_안의_cli_writes도_거부한다() {
        let mut resolve = |_: &str| -> Result<Vec<MacroStep>, Infallible> {
            Ok(vec![step_with("cli_writes", json!({ "id": "c1" }))])
        };
        assert_eq!(
            find_forbidden_macro_step(
                &[step_with("run_macro", json!({ "id": "A" }))],
                &mut resolve,
                &stack(&["B"]),
                None
            ),
            Ok(forbidden(0, "cli_writes"))
        );
    }

    #[test]
    fn resolve가_실패하면_screen_macro_run도_실패하고_실행으로_넘어가지_않는다() {
        let mut resolve =
            |_: &str| -> Result<Vec<MacroStep>, String> { Err("missing macro".to_owned()) };
        assert_eq!(
            screen_macro_run(
                &[step_with("run_macro", json!({ "id": "A" }))],
                &mut resolve,
                &stack(&["B"]),
                None
            ),
            Err("missing macro".to_owned())
        );
    }

    // --- Not in the TypeScript tests. ---

    #[test]
    fn 거부_문구는_스텝_번호와_도구_이름만_바뀐다() {
        assert_eq!(
            macro_run_refusal(3, "cli_run"),
            "run_macro refused step 3 (cli_run): agent-invoked macros cannot run ALWAYS_RISKY_TOOLS, a non-GET replay_request/repeater_send, cli_writes, or a non-read cli_run. Call that tool directly so ACP can prompt, or so an API model can run it once it is advertised. The Workflows panel is unchanged."
        );
    }

    #[test]
    fn 거부되는_첫_스텝의_번호를_돌려주고_빈_id와_빈_command는_거부한다() {
        let read = |_: &str, _: &str| Some(CliEffect::Read);
        let steps = [
            step("browser_snapshot"),
            step_with(
                "replay_request",
                json!({ "overrides": { "method": "get" } }),
            ),
            step_with("cli_run", json!({ "id": "c1", "command": "x" })),
            step("lfi_probe"),
            step("burst_send"),
        ];
        assert_eq!(find(&steps, &[], Some(&read)), forbidden(3, "lfi_probe"));
        for input in [
            json!({ "id": "", "command": "x" }),
            json!({ "id": "c1", "command": "" }),
            json!({ "id": 1, "command": "x" }),
            json!(["c1", "x"]),
            json!("c1"),
        ] {
            assert_eq!(
                find(&[step_with("cli_run", input.clone())], &[], Some(&read)),
                forbidden(0, "cli_run"),
                "{input}"
            );
        }
    }

    #[test]
    fn 한_번_거친_매크로를_안쪽에서_다시_부르면_순환으로_거부한다() {
        let mut calls = Vec::new();
        let mut resolve = |id: &str| -> Result<Vec<MacroStep>, Infallible> {
            calls.push(id.to_owned());
            Ok(vec![step_with(
                "run_macro",
                json!({ "id": if id == "A" { "B" } else { "A" } }),
            )])
        };
        assert_eq!(
            find_forbidden_macro_step(
                &[step_with("run_macro", json!({ "id": "A" }))],
                &mut resolve,
                &[],
                None
            ),
            Ok(forbidden(0, "run_macro"))
        );
        assert_eq!(calls, ["A", "B"]);
    }

    // The TypeScript has no depth limit: only a cycle or a forbidden step stops a chain.
    #[test]
    fn 깊게_이어진_매크로_사슬도_끝까지_따라가_안쪽_도구를_찾는다() {
        const DEPTH: usize = 5_000;
        let mut calls = 0usize;
        let mut resolve = |id: &str| -> Result<Vec<MacroStep>, Infallible> {
            calls += 1;
            let n: usize = id.parse().unwrap_or(DEPTH);
            Ok(if n + 1 < DEPTH {
                vec![
                    step("browser_snapshot"),
                    step_with("run_macro", json!({ "id": (n + 1).to_string() })),
                ]
            } else {
                vec![step("lfi_probe")]
            })
        };
        let steps = [
            step("browser_snapshot"),
            step_with("run_macro", json!({ "id": "0" })),
        ];
        assert_eq!(
            find_forbidden_macro_step(&steps, &mut resolve, &[], None),
            Ok(forbidden(1, "lfi_probe"))
        );
        assert_eq!(calls, DEPTH);
    }

    #[test]
    fn 중첩_매크로가_끝나면_바깥_스텝을_이어서_검사하고_같은_매크로를_나란히_두_번_불러도_된다() {
        let mut resolve = |id: &str| -> Result<Vec<MacroStep>, Infallible> {
            Ok(match id {
                "A" => vec![
                    step_with("run_macro", json!({ "id": "B" })),
                    step_with("run_macro", json!({ "id": "B" })),
                    step("browser_snapshot"),
                ],
                _ => vec![step("browser_click")],
            })
        };
        let run_a = step_with("run_macro", json!({ "id": "A" }));
        assert_eq!(
            find_forbidden_macro_step(
                &[run_a.clone(), run_a.clone(), step("browser_snapshot")],
                &mut resolve,
                &[],
                None
            ),
            Ok(None)
        );
        assert_eq!(
            find_forbidden_macro_step(
                &[
                    step("browser_snapshot"),
                    run_a.clone(),
                    run_a,
                    step("cli_writes"),
                    step("burst_send")
                ],
                &mut resolve,
                &[],
                None
            ),
            Ok(forbidden(3, "cli_writes"))
        );
    }

    #[test]
    fn 스텝은_input이_없거나_null이어도_읽히고_없는_input은_쓰지_않는다() {
        let steps: Vec<MacroStep> = serde_json::from_value(json!([
            { "tool": "browser_snapshot" },
            { "tool": "cli_run", "input": null },
            { "tool": "run_macro", "input": { "id": "A" }, "note": "ignored" }
        ]))
        .unwrap();
        assert_eq!(steps[0], step("browser_snapshot"));
        assert_eq!(steps[1], step("cli_run"));
        assert_eq!(steps[2], step_with("run_macro", json!({ "id": "A" })));
        assert_eq!(
            serde_json::to_value(&steps[0]).unwrap(),
            json!({ "tool": "browser_snapshot" })
        );
        assert_eq!(
            serde_json::to_value(forbidden(2, "run_macro")).unwrap(),
            json!({ "index": 2, "tool": "run_macro" })
        );
    }
}
