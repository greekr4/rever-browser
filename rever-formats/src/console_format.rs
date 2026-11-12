//! Port of `src/main/console-format.ts` — formatting of a CDP Runtime.evaluate
//! reply for the console panel's command line, close to how Chrome's console
//! prints values.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::is_js_whitespace;

#[derive(Clone, Debug, Deserialize)]
pub struct PropertyPreview {
    pub name: String,
    pub r#type: String,
    pub value: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ObjectPreview {
    pub subtype: Option<String>,
    #[serde(default)]
    pub overflow: bool,
    pub properties: Vec<PropertyPreview>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteObject {
    pub r#type: String,
    pub subtype: Option<String>,
    /// `Some(Value::Null)` is a JSON null, `None` a missing field: JavaScript
    /// printed them as "null" and "undefined".
    #[serde(default, deserialize_with = "present")]
    pub value: Option<Value>,
    pub unserializable_value: Option<String>,
    pub description: Option<String>,
    pub preview: Option<ObjectPreview>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExceptionDetails {
    pub text: String,
    pub exception: Option<RemoteObject>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalReply {
    pub result: RemoteObject,
    pub exception_details: Option<ExceptionDetails>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalOutput {
    pub text: String,
    pub is_error: bool,
}

fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// JSON.stringify of a string.
fn quote(s: &str) -> String {
    Value::from(s).to_string()
}

/// JavaScript's `String(value)` for a JSON value.
fn js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                if item.is_null() {
                    String::new()
                } else {
                    js_string(Some(item))
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".into(),
        // shortcut: floats at or past 1e21 or below 1e-6 print without JavaScript's
        // exponent form. CDP sends `description` for every number, so this is only
        // reached by a reply that breaks the protocol; port Number::toString if one shows up.
        Some(Value::Number(n)) => match n.as_f64() {
            // `+ 0.0` turns -0 into 0, which is how JavaScript prints it.
            Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() < 1e21 => {
                format!("{:.0}", f + 0.0)
            }
            _ => n.to_string(),
        },
        Some(other) => other.to_string(),
    }
}

fn format_preview_value(p: &PropertyPreview) -> String {
    if p.r#type == "string" {
        quote(p.value.as_deref().unwrap_or(""))
    } else {
        p.value.clone().unwrap_or_else(|| p.r#type.clone())
    }
}

fn format_object(o: &RemoteObject) -> String {
    let described = || o.description.clone().unwrap_or_else(|| "Object".into());
    let has = |subtype: &Option<String>| subtype.as_deref().is_some_and(|s| !s.is_empty());
    if o.subtype.as_deref() == Some("null") {
        return "null".into();
    }
    let Some(pv) = &o.preview else {
        return described();
    };
    let more = if pv.overflow { ", …" } else { "" };
    if pv.subtype.as_deref() == Some("array") {
        let items: Vec<String> = pv.properties.iter().map(format_preview_value).collect();
        return format!("[{}{more}]", items.join(", "));
    }
    // Errors, DOM nodes, functions etc. read better as their description.
    if has(&pv.subtype) || has(&o.subtype) {
        return described();
    }
    let body: Vec<String> = pv
        .properties
        .iter()
        .map(|p| format!("{}: {}", p.name, format_preview_value(p)))
        .collect();
    let name = match o.description.as_deref() {
        Some(d) if !d.is_empty() && d != "Object" => format!("{d} "),
        _ => String::new(),
    };
    format!("{name}{{{}{more}}}", body.join(", "))
}

fn format_remote_object(o: &RemoteObject) -> String {
    match o.r#type.as_str() {
        "undefined" => "undefined".into(),
        // JSON.stringify(undefined) is undefined, which the panel showed as that word.
        "string" => o
            .value
            .as_ref()
            .map_or_else(|| "undefined".into(), Value::to_string),
        "object" => format_object(o),
        _ => o
            .unserializable_value
            .clone()
            .or_else(|| o.description.clone())
            .unwrap_or_else(|| js_string(o.value.as_ref())),
    }
}

pub fn format_eval_result(r: &EvalReply) -> EvalOutput {
    if let Some(details) = &r.exception_details {
        let desc = details.exception.as_ref().map_or_else(String::new, |ex| {
            ex.description
                .clone()
                .unwrap_or_else(|| format_remote_object(ex))
        });
        let text = format!("{} {desc}", details.text);
        return EvalOutput {
            text: text.trim_matches(is_js_whitespace).to_owned(),
            is_error: true,
        };
    }
    EvalOutput {
        text: format_remote_object(&r.result),
        is_error: false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Formats a reply given as the JSON CDP sends.
    fn format(reply: Value) -> EvalOutput {
        format_eval_result(&serde_json::from_value(reply).unwrap())
    }

    #[test]
    fn 문자열은_따옴표로_감싼다() {
        assert_eq!(
            format(json!({ "result": { "type": "string", "value": "zz" } })),
            EvalOutput {
                text: "\"zz\"".into(),
                is_error: false
            }
        );
    }

    #[test]
    fn undefined와_null을_그대로_보여준다() {
        assert_eq!(
            format(json!({ "result": { "type": "undefined" } })).text,
            "undefined"
        );
        assert_eq!(
            format(json!({ "result": { "type": "object", "subtype": "null", "value": null } }))
                .text,
            "null"
        );
    }

    #[test]
    fn 숫자와_nan_같은_직렬화_불가_값을_보여준다() {
        assert_eq!(
            format(json!({ "result": { "type": "number", "value": 3, "description": "3" } })).text,
            "3"
        );
        assert_eq!(
            format(json!({
                "result": { "type": "number", "unserializableValue": "NaN", "description": "NaN" }
            }))
            .text,
            "NaN"
        );
    }

    #[test]
    fn 객체_미리보기를_키_값_형태로_만든다() {
        let r = format(json!({
            "result": {
                "type": "object",
                "description": "Object",
                "preview": {
                    "type": "object",
                    "overflow": true,
                    "properties": [
                        { "name": "a", "type": "number", "value": "1" },
                        { "name": "b", "type": "string", "value": "x" }
                    ]
                }
            }
        }));
        assert_eq!(r.text, "{a: 1, b: \"x\", …}");
    }

    #[test]
    fn 배열_미리보기를_값_목록_형태로_만든다() {
        let r = format(json!({
            "result": {
                "type": "object",
                "subtype": "array",
                "description": "Array(2)",
                "preview": {
                    "type": "object",
                    "subtype": "array",
                    "overflow": false,
                    "properties": [
                        { "name": "0", "type": "number", "value": "1" },
                        { "name": "1", "type": "number", "value": "2" }
                    ]
                }
            }
        }));
        assert_eq!(r.text, "[1, 2]");
    }

    #[test]
    fn 예외는_에러로_표시하고_스택_설명을_쓴다() {
        let r = format(json!({
            "result": { "type": "object", "subtype": "error" },
            "exceptionDetails": {
                "text": "Uncaught",
                "exception": {
                    "type": "object",
                    "subtype": "error",
                    "description": "ReferenceError: x is not defined"
                }
            }
        }));
        assert_eq!(
            r,
            EvalOutput {
                text: "Uncaught ReferenceError: x is not defined".into(),
                is_error: true
            }
        );
    }

    // Not in the TypeScript suite: the value-less paths lean on JavaScript's String().

    #[test]
    fn 설명이_없는_원시값은_자바스크립트_string처럼_보여준다() {
        assert_eq!(
            format(json!({ "result": { "type": "boolean", "value": true } })).text,
            "true"
        );
        assert_eq!(
            format(json!({ "result": { "type": "number", "value": 3.0 } })).text,
            "3"
        );
        assert_eq!(
            format(json!({ "result": { "type": "number", "value": null } })).text,
            "null"
        );
        assert_eq!(
            format(json!({ "result": { "type": "number" } })).text,
            "undefined"
        );
        assert_eq!(
            serde_json::to_value(format(json!({ "result": { "type": "undefined" } }))).unwrap(),
            json!({ "text": "undefined", "isError": false })
        );
    }
}
