//! Port of `src/renderer/src/workflows/core/types.ts` — a saved workflow and its id.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A saved workflow. The core stays deliberately ignorant of each kind's shape:
/// `kind` is a plain string and `data` is opaque, so a workflow module can be
/// added or deleted without the core (store/panel) ever referencing its types.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Workflow {
    pub id: String,
    pub kind: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Kind-specific payload. Each module reads this as its own type.
    pub data: Value,
    /// Milliseconds since the Unix epoch (`Date.now()`).
    pub created_at: u64,
    pub updated_at: u64,
}

/// `Date.now()`.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// `Number#toString(36)` for a whole number.
fn base36(mut number: u64) -> String {
    let mut digits = Vec::new();
    loop {
        // number % 36 is always a valid base-36 digit.
        digits.extend(char::from_digit((number % 36) as u32, 36));
        number /= 36;
        if number == 0 {
            break;
        }
    }
    digits.iter().rev().collect()
}

/// `${prefix}${Date.now().toString(36)}${Math.random().toString(36).slice(2, 2 + digits)}`.
pub(crate) fn new_id(prefix: char, now_ms: u64, digits: u32) -> String {
    // A fresh RandomState is keyed from the OS: std's randomness without a `rand` dependency.
    let random = RandomState::new().build_hasher().finish() % 36u64.pow(digits);
    format!(
        "{prefix}{}{:0>width$}",
        base36(now_ms),
        base36(random),
        width = digits as usize
    )
}

pub fn new_workflow_id() -> String {
    new_id('w', now_ms(), 6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 워크플로_id는_w와_시각과_난수_여섯_자리다() {
        // (1700000000000).toString(36) === "loyw3v28"
        let id = new_id('w', 1_700_000_000_000, 6);
        assert!(id.starts_with("wloyw3v28"), "{id}");
        assert_eq!(id.len(), 1 + 8 + 6);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
        );
        assert!(new_workflow_id().starts_with('w'));
    }

    #[test]
    fn 연달아_만든_id는_서로_다르다() {
        let ids: std::collections::HashSet<String> = (0..50).map(|_| new_id('w', 1, 6)).collect();
        assert!(ids.len() > 45);
    }

    #[test]
    fn 저장_형식은_camel_case이고_없는_설명은_쓰지_않는다() {
        let workflow = Workflow {
            id: "w1".into(),
            kind: "template".into(),
            name: "Hello".into(),
            description: None,
            data: json!({ "body": "x" }),
            created_at: 5,
            updated_at: 7,
        };
        assert_eq!(
            serde_json::to_string(&workflow).unwrap(),
            r#"{"id":"w1","kind":"template","name":"Hello","data":{"body":"x"},"createdAt":5,"updatedAt":7}"#
        );
        let with_description = Workflow {
            description: Some("d".into()),
            ..workflow.clone()
        };
        assert_eq!(
            serde_json::to_value(&with_description).unwrap()["description"],
            json!("d")
        );
        assert_eq!(
            serde_json::from_value::<Workflow>(serde_json::to_value(&workflow).unwrap()).unwrap(),
            workflow
        );
    }

    #[test]
    fn 빠진_필드는_기본값으로_읽는다() {
        let workflow: Workflow = serde_json::from_value(json!({ "id": "w1" })).unwrap();
        assert_eq!(workflow.id, "w1");
        assert_eq!(workflow.kind, "");
        assert_eq!(workflow.data, Value::Null);
        assert_eq!(workflow.created_at, 0);
    }
}
