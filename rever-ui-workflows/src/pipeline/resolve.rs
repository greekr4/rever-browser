//! Port of `src/renderer/src/workflows/pipeline/resolve.ts` — the editor's node graph compiled into the runnable tree.

use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::{PipeCond, PipeNode};
use crate::js::{js_string, js_trim};

/// Runnable form (mirrors preload/main ResolvedPipeNode): tool inputs parsed to
/// objects with `{{var}}` substituted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ResolvedPipeNode {
    Tool {
        id: String,
        tool: String,
        input: Value,
    },
    If {
        id: String,
        cond: PipeCond,
        then: Vec<ResolvedPipeNode>,
        #[serde(rename = "else")]
        otherwise: Vec<ResolvedPipeNode>,
    },
}

/// `/\{\{\s*([\w.-]+)\s*\}\}/g`. Spelled out because the regex crate's `\s` and `\w`
/// are Unicode-wide, while JavaScript's `\w` is ASCII and its `\s` is its own set.
pub(crate) static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    const WS: &str =
        r"[\t-\r \x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*";
    // The pattern is a constant; it cannot fail to compile.
    #[allow(clippy::expect_used)]
    Regex::new(&format!(r"\{{\{{{WS}([A-Za-z0-9_.-]+){WS}\}}\}}")).expect("placeholder pattern")
});

/// `JSON.parse(varsText)` when there is any text, else an empty object.
pub(crate) fn parse_vars(vars_text: &str) -> Result<Value, String> {
    if js_trim(vars_text).is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str(vars_text).map_err(|_| "Variables are not valid JSON".to_owned())
}

/// `vars[key]`: `Ok(None)` is `undefined`, `Err` the TypeError of reading a property
/// of `null`. Own properties only — an object's inherited ones (`toString`,
/// `constructor`) are not looked up.
pub(crate) fn lookup<'a>(vars: &'a Value, key: &str) -> Result<Option<LookedUp<'a>>, String> {
    Ok(match vars {
        Value::Null => {
            return Err(format!("Cannot read properties of null (reading '{key}')"));
        }
        Value::Object(map) => map.get(key).map(LookedUp::Value),
        Value::Array(items) if key == "length" => Some(LookedUp::Text(items.len().to_string())),
        Value::Array(items) => index_key(key)
            .and_then(|index| items.get(index))
            .map(LookedUp::Value),
        Value::String(text) if key == "length" => {
            Some(LookedUp::Text(text.encode_utf16().count().to_string()))
        }
        Value::String(text) => index_key(key)
            .and_then(|index| text.encode_utf16().nth(index))
            .map(|unit| LookedUp::Text(String::from_utf16_lossy(&[unit]))),
        Value::Bool(_) | Value::Number(_) => None,
    })
}

pub(crate) enum LookedUp<'a> {
    Value(&'a Value),
    Text(String),
}

impl LookedUp<'_> {
    /// `v == null ? None : String(v)`.
    pub(crate) fn text(self) -> Option<String> {
        match self {
            LookedUp::Value(Value::Null) => None,
            LookedUp::Value(value) => Some(js_string(Some(value))),
            LookedUp::Text(text) => Some(text),
        }
    }
}

/// A canonical array index: "0", "12", not "01" or "+1".
fn index_key(key: &str) -> Option<usize> {
    let canonical =
        key == "0" || (!key.starts_with('0') && key.bytes().all(|b| b.is_ascii_digit()));
    canonical.then(|| key.parse().ok()).flatten()
}

fn substitute(text: &str, vars: &Value) -> Result<String, String> {
    let mut failure = None;
    let out = PLACEHOLDER.replace_all(text, |found: &Captures| {
        let key = found.get(1).map_or("", |key| key.as_str());
        match lookup(vars, key) {
            Ok(value) => value.and_then(LookedUp::text).unwrap_or_default(),
            Err(error) => {
                failure.get_or_insert(error);
                String::new()
            }
        }
    });
    match failure {
        Some(error) => Err(error),
        None => Ok(out.into_owned()),
    }
}

fn resolve_list(nodes: &[PipeNode], vars: &Value) -> Result<Vec<ResolvedPipeNode>, String> {
    nodes
        .iter()
        .map(|node| match node {
            PipeNode::Tool { id, tool, input } => {
                if tool.is_empty() {
                    return Err("A step has no tool selected".to_owned());
                }
                let substituted = substitute(input, vars)?;
                let input = if js_trim(&substituted).is_empty() {
                    Value::Object(Default::default())
                } else {
                    serde_json::from_str(&substituted)
                        .map_err(|_| format!("Step \"{tool}\" input is not valid JSON"))?
                };
                Ok(ResolvedPipeNode::Tool {
                    id: id.clone(),
                    tool: tool.clone(),
                    input,
                })
            }
            PipeNode::If {
                id,
                cond,
                then,
                otherwise,
            } => Ok(ResolvedPipeNode::If {
                id: id.clone(),
                cond: cond.clone(),
                then: resolve_list(then, vars)?,
                otherwise: resolve_list(otherwise, vars)?,
            }),
        })
        .collect()
}

pub fn resolve_pipeline(
    nodes: &[PipeNode],
    vars_text: &str,
) -> Result<Vec<ResolvedPipeNode>, String> {
    let vars = parse_vars(vars_text)?;
    resolve_list(nodes, &vars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::types::{CondOn, CondOp};
    use serde_json::json;

    fn tool(id: &str, tool: &str, input: &str) -> PipeNode {
        PipeNode::Tool {
            id: id.into(),
            tool: tool.into(),
            input: input.into(),
        }
    }

    fn resolved(id: &str, tool: &str, input: Value) -> ResolvedPipeNode {
        ResolvedPipeNode::Tool {
            id: id.into(),
            tool: tool.into(),
            input,
        }
    }

    #[test]
    fn 도구_입력을_json으로_읽는다() {
        let nodes = [tool(
            "n1",
            "browser_navigate",
            r#"{ "url": "https://a.test" }"#,
        )];
        assert_eq!(
            resolve_pipeline(&nodes, ""),
            Ok(vec![resolved(
                "n1",
                "browser_navigate",
                json!({ "url": "https://a.test" })
            )])
        );
    }

    #[test]
    fn 빈_입력은_빈_객체다() {
        for input in ["", "   ", "\n\t", "\u{feff}"] {
            assert_eq!(
                resolve_pipeline(&[tool("n1", "t", input)], ""),
                Ok(vec![resolved("n1", "t", json!({}))]),
                "{input:?}"
            );
        }
    }

    #[test]
    fn 변수를_입력에_끼워_넣는다() {
        let nodes = [tool(
            "n1",
            "t",
            r#"{ "url": "https://{{host}}/{{ path.to-x }}?n={{n}}&ok={{ok}}" }"#,
        )];
        let vars = r#"{ "host": "example.com", "path.to-x": "a", "n": 3, "ok": true }"#;
        assert_eq!(
            resolve_pipeline(&nodes, vars),
            Ok(vec![resolved(
                "n1",
                "t",
                json!({ "url": "https://example.com/a?n=3&ok=true" })
            )])
        );
    }

    #[test]
    fn 없는_변수와_null_변수는_빈_문자열이_된다() {
        let nodes = [tool(
            "n1",
            "t",
            r#"{ "a": "[{{missing}}]", "b": "[{{nothing}}]" }"#,
        )];
        assert_eq!(
            resolve_pipeline(&nodes, r#"{ "nothing": null }"#),
            Ok(vec![resolved("n1", "t", json!({ "a": "[]", "b": "[]" }))])
        );
    }

    #[test]
    fn 값은_이스케이프_없이_그대로_들어간다() {
        // 매크로와 달리 따옴표가 든 값은 JSON을 깨뜨린다.
        let nodes = [tool("n1", "t", r#"{ "q": "{{q}}" }"#)];
        assert_eq!(
            resolve_pipeline(&nodes, r#"{ "q": "a\"b" }"#),
            Err("Step \"t\" input is not valid JSON".to_owned())
        );
        // 따옴표 없이 쓰면 숫자나 객체 자리에도 들어간다.
        let nodes = [tool("n1", "t", r#"{ "n": {{n}}, "list": [{{list}}] }"#)];
        assert_eq!(
            resolve_pipeline(&nodes, r#"{ "n": 1.50, "list": [1, 2] }"#),
            Ok(vec![resolved(
                "n1",
                "t",
                json!({ "n": 1.5, "list": [1, 2] })
            )])
        );
    }

    #[test]
    fn 자리표시자_이름은_ascii_글자와_점과_줄표만_된다() {
        let nodes = [tool(
            "n1",
            "t",
            r#"{ "a": "{{한글}}", "b": "{{a b}}", "c": "{{}}" }"#,
        )];
        assert_eq!(
            resolve_pipeline(&nodes, r#"{ "한글": "x" }"#),
            Ok(vec![resolved(
                "n1",
                "t",
                json!({ "a": "{{한글}}", "b": "{{a b}}", "c": "{{}}" })
            )])
        );
    }

    #[test]
    fn 자리표시자_안의_공백은_자바스크립트_공백이다() {
        let nodes = [tool("n1", "t", "{ \"a\": \"{{\u{feff}\u{a0} x \t}}\" }")];
        assert_eq!(
            resolve_pipeline(&nodes, r#"{ "x": "ok" }"#),
            Ok(vec![resolved("n1", "t", json!({ "a": "ok" }))])
        );
        // U+0085는 자바스크립트 공백이 아니다.
        let nodes = [tool("n1", "t", "{ \"a\": \"{{\u{85}x}}\" }")];
        assert_eq!(
            resolve_pipeline(&nodes, r#"{ "x": "ok" }"#),
            Ok(vec![resolved("n1", "t", json!({ "a": "{{\u{85}x}}" }))])
        );
    }

    #[test]
    fn 변수가_json이_아니면_오류다() {
        assert_eq!(
            resolve_pipeline(&[tool("n1", "t", "{}")], "{ host: 1 }"),
            Err("Variables are not valid JSON".to_owned())
        );
        // 공백뿐인 변수는 변수가 없는 것이다.
        assert!(resolve_pipeline(&[tool("n1", "t", "{}")], "  \n").is_ok());
    }

    #[test]
    fn 도구를_고르지_않은_단계는_오류다() {
        assert_eq!(
            resolve_pipeline(&[tool("n1", "", "{}")], ""),
            Err("A step has no tool selected".to_owned())
        );
    }

    #[test]
    fn 입력이_json이_아니면_도구_이름과_함께_오류다() {
        assert_eq!(
            resolve_pipeline(&[tool("n1", "browser_click", "{ ref: 1 }")], ""),
            Err("Step \"browser_click\" input is not valid JSON".to_owned())
        );
    }

    #[test]
    fn 조건_노드는_두_가지를_모두_풀고_조건은_그대로_둔다() {
        let cond = PipeCond {
            on: CondOn::Output,
            op: CondOp::Contains,
            value: "{{host}}".into(),
        };
        let nodes = [PipeNode::If {
            id: "n1".into(),
            cond: cond.clone(),
            then: vec![tool("n2", "a", r#"{ "h": "{{host}}" }"#)],
            otherwise: vec![PipeNode::If {
                id: "n3".into(),
                cond: PipeCond::default(),
                then: vec![],
                otherwise: vec![tool("n4", "b", "")],
            }],
        }];
        assert_eq!(
            resolve_pipeline(&nodes, r#"{ "host": "x" }"#),
            Ok(vec![ResolvedPipeNode::If {
                id: "n1".into(),
                cond,
                then: vec![resolved("n2", "a", json!({ "h": "x" }))],
                otherwise: vec![ResolvedPipeNode::If {
                    id: "n3".into(),
                    cond: PipeCond::default(),
                    then: vec![],
                    otherwise: vec![resolved("n4", "b", json!({}))],
                }],
            }])
        );
    }

    #[test]
    fn 가지_안의_오류도_전체_오류다() {
        let nodes = [PipeNode::If {
            id: "n1".into(),
            cond: PipeCond::default(),
            then: vec![],
            otherwise: vec![tool("n2", "", "")],
        }];
        assert_eq!(
            resolve_pipeline(&nodes, ""),
            Err("A step has no tool selected".to_owned())
        );
    }

    #[test]
    fn 빈_파이프라인은_빈_목록이다() {
        assert_eq!(resolve_pipeline(&[], ""), Ok(vec![]));
    }

    #[test]
    fn 객체가_아닌_변수도_자바스크립트처럼_읽는다() {
        let nodes = [tool(
            "n1",
            "t",
            r#"{ "a": "{{0}}", "b": "{{length}}", "c": "{{x}}" }"#,
        )];
        assert_eq!(
            resolve_pipeline(&nodes, r#"["p", "q"]"#),
            Ok(vec![resolved(
                "n1",
                "t",
                json!({ "a": "p", "b": "2", "c": "" })
            )])
        );
        assert_eq!(
            resolve_pipeline(&nodes, r#""hi""#),
            Ok(vec![resolved(
                "n1",
                "t",
                json!({ "a": "h", "b": "2", "c": "" })
            )])
        );
        assert_eq!(
            resolve_pipeline(&nodes, "5"),
            Ok(vec![resolved(
                "n1",
                "t",
                json!({ "a": "", "b": "", "c": "" })
            )])
        );
    }

    #[test]
    fn 변수가_null이면_자리표시자를_읽을_때_오류다() {
        assert_eq!(
            resolve_pipeline(&[tool("n1", "t", r#"{ "a": "{{x}}" }"#)], "null"),
            Err("Cannot read properties of null (reading 'x')".to_owned())
        );
        // 자리표시자가 없으면 읽을 일도 없다.
        assert!(resolve_pipeline(&[tool("n1", "t", "{}")], "null").is_ok());
    }

    // Expected values below were read from the TypeScript run under bun.
    #[test]
    fn 숫자와_배열_변수는_자바스크립트_문자열로_들어간다() {
        let nodes = [tool("n1", "t", r#"{"a":"{{x}}"}"#)];
        assert_eq!(
            resolve_pipeline(&nodes, r#"{"x":[1,[2,null],{"a":1},true]}"#),
            Ok(vec![resolved(
                "n1",
                "t",
                json!({ "a": "1,2,,[object Object],true" })
            )])
        );
        let nodes = [tool(
            "n1",
            "t",
            r#"{"a":"{{a}} {{b}} {{c}} {{d}} {{e}} {{f}}"}"#,
        )];
        let vars = r#"{"a":1e21,"b":1e-7,"c":-0,"d":0.30000000000000004,"e":123456789012345678901234567890,"f":100.0}"#;
        assert_eq!(
            resolve_pipeline(&nodes, vars),
            Ok(vec![resolved(
                "n1",
                "t",
                json!({ "a": "1e+21 1e-7 0 0.30000000000000004 1.2345678901234568e+29 100" })
            )])
        );
        // false와 0과 빈 문자열은 null이 아니다.
        let nodes = [tool("n1", "t", r#"{"a":"[{{f}}|{{z}}|{{e}}]"}"#)];
        assert_eq!(
            resolve_pipeline(&nodes, r#"{"f":false,"z":0,"e":""}"#),
            Ok(vec![resolved("n1", "t", json!({ "a": "[false|0|]" }))])
        );
    }

    #[test]
    fn 값에_든_달러_기호는_치환_패턴이_아니다() {
        let nodes = [tool("n1", "t", r#"{"a":"{{x}}"}"#)];
        assert_eq!(
            resolve_pipeline(&nodes, r#"{"x":"$& $1 $$"}"#),
            Ok(vec![resolved("n1", "t", json!({ "a": "$& $1 $$" }))])
        );
    }

    #[test]
    fn 이름에는_점과_줄표만_있어도_되고_겹친_중괄호는_안쪽만_바뀐다() {
        let nodes = [tool(
            "n1",
            "t",
            r#"{"a":"{{a-}}|{{.}}|{{-}}|{{a.b}}|{{ _ }}"}"#,
        )];
        assert_eq!(
            resolve_pipeline(&nodes, r#"{"a-":1,".":2,"-":3,"a.b":4,"_":5}"#),
            Ok(vec![resolved("n1", "t", json!({ "a": "1|2|3|4|5" }))])
        );
        let nodes = [tool("n1", "t", r#"{"a":"{{{x}}}"}"#)];
        assert_eq!(
            resolve_pipeline(&nodes, r#"{"x":"ok"}"#),
            Ok(vec![resolved("n1", "t", json!({ "a": "{ok}" }))])
        );
        // 자리표시자 안의 줄바꿈도 공백이다.
        let nodes = [tool("n1", "t", "{\"a\":\"{{\n x \n}}\"}")];
        assert_eq!(
            resolve_pipeline(&nodes, r#"{"x":"ok"}"#),
            Ok(vec![resolved("n1", "t", json!({ "a": "ok" }))])
        );
    }

    #[test]
    fn 배열_변수는_정식_색인만_읽는다() {
        let nodes = [tool("n1", "t", r#"{"a":"{{1}}|{{01}}|{{length}}|{{-1}}"}"#)];
        assert_eq!(
            resolve_pipeline(&nodes, r#"["p","q"]"#),
            Ok(vec![resolved("n1", "t", json!({ "a": "q||2|" }))])
        );
        assert_eq!(
            resolve_pipeline(&[tool("n1", "t", r#"{"a":"{{x}}"}"#)], "true"),
            Ok(vec![resolved("n1", "t", json!({ "a": "" }))])
        );
    }

    #[test]
    fn 같은_키가_두_번_나오면_뒤의_값이_처음_자리에_남는다() {
        let resolved_nodes =
            resolve_pipeline(&[tool("n1", "t", r#"{"a":1,"b":2,"a":3}"#)], "").unwrap();
        assert_eq!(
            serde_json::to_string(&resolved_nodes).unwrap(),
            r#"[{"type":"tool","id":"n1","tool":"t","input":{"a":3,"b":2}}]"#
        );
    }

    #[test]
    fn 풀린_노드는_type과_else_이름으로_직렬화한다() {
        let nodes = vec![ResolvedPipeNode::If {
            id: "n1".into(),
            cond: PipeCond::default(),
            then: vec![resolved("n2", "t", json!({ "a": 1 }))],
            otherwise: vec![],
        }];
        assert_eq!(
            serde_json::to_value(&nodes).unwrap(),
            json!([{
                "type": "if",
                "id": "n1",
                "cond": { "on": "output", "op": "contains", "value": "" },
                "then": [{ "type": "tool", "id": "n2", "tool": "t", "input": { "a": 1 } }],
                "else": []
            }])
        );
    }
}
