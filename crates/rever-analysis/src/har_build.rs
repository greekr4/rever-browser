//! Port of `src/main/mcp/har-build.ts`.

use rever_core::traffic::Headers;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HarHeader {
    pub name: String,
    pub value: String,
}

/// Convert a header record into HAR's name/value array form.
pub fn to_headers(h: Option<&Headers>) -> Vec<HarHeader> {
    let Some(h) = h else {
        return Vec::new();
    };
    // CDP folds repeated header fields (Set-Cookie, Warning, ...) into one
    // '\n'-joined value; HAR requires one entry per field line, and header
    // field-values themselves can never contain a raw newline, so splitting
    // is always safe.
    h.iter()
        .flat_map(|(name, value)| {
            value.split('\n').map(move |line| HarHeader {
                name: name.clone(),
                value: line.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(headers: &[HarHeader]) -> Vec<(&str, &str)> {
        headers
            .iter()
            .map(|h| (h.name.as_str(), h.value.as_str()))
            .collect()
    }

    // ── cases of src/main/mcp/pure-helpers.test.ts ──────────────────────────

    #[test]
    fn 레코드를_name_value_쌍으로_바꾸고_없는_헤더를_처리한다() {
        let h = Headers::from([("A".to_owned(), "1".to_owned())]);
        assert_eq!(pairs(&to_headers(Some(&h))), [("A", "1")]);
        assert_eq!(to_headers(None), []);
    }

    #[test]
    fn cdp가_접은_반복_헤더를_별도의_har_항목으로_나눈다() {
        let h = Headers::from([(
            "Set-Cookie".to_owned(),
            "a=1; HttpOnly\nb=2; Secure".to_owned(),
        )]);
        assert_eq!(
            pairs(&to_headers(Some(&h))),
            [
                ("Set-Cookie", "a=1; HttpOnly"),
                ("Set-Cookie", "b=2; Secure")
            ]
        );
    }

    // ── not in the TypeScript suite ─────────────────────────────────────────

    #[test]
    fn 헤더가_없으면_빈_배열이다() {
        assert_eq!(to_headers(None), []);
        assert_eq!(to_headers(Some(&Headers::new())), []);
    }

    #[test]
    fn 헤더_레코드를_name_value_배열로_바꾼다() {
        let h = Headers::from([
            ("accept".to_owned(), "*/*".to_owned()),
            ("x-empty".to_owned(), String::new()),
        ]);
        assert_eq!(
            pairs(&to_headers(Some(&h))),
            [("accept", "*/*"), ("x-empty", "")]
        );
    }

    #[test]
    fn 줄바꿈으로_접힌_헤더는_줄마다_한_항목으로_나눈다() {
        let h = Headers::from([("set-cookie".to_owned(), "a=1; Path=/\nb=2\n".to_owned())]);
        assert_eq!(
            pairs(&to_headers(Some(&h))),
            [
                ("set-cookie", "a=1; Path=/"),
                ("set-cookie", "b=2"),
                ("set-cookie", "")
            ]
        );
    }

    #[test]
    fn har_필드_이름으로_직렬화한다() {
        let h = Headers::from([("a".to_owned(), "b".to_owned())]);
        assert_eq!(
            serde_json::to_string(&to_headers(Some(&h))).unwrap(),
            r#"[{"name":"a","value":"b"}]"#
        );
    }
}
