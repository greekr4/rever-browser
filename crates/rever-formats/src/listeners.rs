//! Port of `src/main/mcp/listeners.ts` — pure helpers for inspect_listeners.

use serde::{Deserialize, Serialize};

use crate::is_js_whitespace;

/// The CDP DOMDebugger.EventListener fields this tool reads.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawListener {
    pub r#type: String,
    pub use_capture: bool,
    pub passive: bool,
    pub once: bool,
    pub script_id: String,
    pub line_number: i64,
    pub column_number: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListenerOwner {
    /// 0 = the selected element, n = n levels up (document / window last).
    pub depth: u32,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenerInfo {
    pub r#type: String,
    pub on: String,
    pub capture: bool,
    pub passive: bool,
    pub once: bool,
    /// 1-based `url:line:col`, or `script#<id>:line:col` when the URL is unknown.
    pub location: String,
    pub url: Option<String>,
    pub line: i64,
    pub column: i64,
}

pub const LISTENER_LIMITATIONS: [&str; 3] = [
    "Only listeners registered with addEventListener / on* properties are listed; framework registries (React props, Vue handlers) are not read directly — with ancestors on, their root-level dispatcher shows up instead.",
    "Closed shadow roots and listeners added after this call are not covered.",
    "Handlers are never invoked; source is the function text, not proof it runs on click.",
];

/// `attributes` is CDP's flat `[name, value, name, value, …]` list.
pub fn node_label<S: AsRef<str>>(node_name: &str, attributes: &[S]) -> String {
    if node_name == "#document" {
        return "document".into();
    }
    if node_name == "window" {
        return "window".into();
    }
    let pairs = || {
        attributes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[name, value]| (name.as_ref(), value.as_ref()))
    };
    let mut label = node_name.to_lowercase();
    for (name, value) in pairs() {
        if name == "id" && !value.is_empty() {
            label.push('#');
            label.push_str(value);
        }
    }
    for (name, value) in pairs() {
        if name != "class" {
            continue;
        }
        // Utility-class frameworks put a dozen classes on one node; three is
        // enough to recognise it.
        let mut classes = value.split(is_js_whitespace).filter(|cls| !cls.is_empty());
        for cls in classes.by_ref().take(3) {
            label.push('.');
            label.push_str(cls);
        }
        if classes.next().is_some() {
            label.push('…');
        }
    }
    label
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SnippetOptions {
    /// Default 40.
    pub max_lines: Option<usize>,
    /// Default 1,500, counted in UTF-16 code units like every length here.
    pub max_chars: Option<usize>,
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `s.slice(start, end)` in JavaScript. A cut through the middle of a surrogate
/// pair leaves half a character, which a Rust string cannot hold: it becomes U+FFFD.
fn utf16_slice(s: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = s
        .encode_utf16()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Source around a 0-based (line, column). Multi-line sources return whole lines
/// from `line`; a minified bundle's single huge line returns a character window
/// starting at the column instead, marked with … where it was cut.
pub fn snippet_at(source: &str, line: i64, column: i64, opts: SnippetOptions) -> String {
    let max_lines = opts.max_lines.unwrap_or(40);
    let max_chars = opts.max_chars.unwrap_or(1_500);
    let Ok(line) = usize::try_from(line) else {
        return String::new();
    };
    let mut lines = source.split('\n').skip(line);
    let Some(target) = lines.next() else {
        return String::new();
    };
    let target_len = utf16_len(target);
    if target_len > max_chars {
        let start = usize::try_from(column).unwrap_or(0).min(target_len);
        let end = target_len.min(start.saturating_add(max_chars));
        return format!(
            "{}{}{}",
            if start > 0 { "…" } else { "" },
            utf16_slice(target, start, end),
            if end < target_len { "…" } else { "" }
        );
    }
    let mut out = String::new();
    if max_lines > 0 {
        out.push_str(target);
        for next in lines.take(max_lines - 1) {
            out.push('\n');
            out.push_str(next);
        }
    }
    if utf16_len(&out) > max_chars {
        format!("{}…", utf16_slice(&out, 0, max_chars))
    } else {
        out
    }
}

pub fn normalize_listener(
    raw: &RawListener,
    owner: &ListenerOwner,
    url: Option<&str>,
) -> ListenerInfo {
    let line = raw.line_number.saturating_add(1);
    let column = raw.column_number.saturating_add(1);
    let on = if owner.depth == 0 {
        format!("self ({})", owner.label)
    } else {
        format!("ancestor +{} ({})", owner.depth, owner.label)
    };
    let script = url.map_or_else(|| format!("script#{}", raw.script_id), str::to_owned);
    ListenerInfo {
        r#type: raw.r#type.clone(),
        on,
        capture: raw.use_capture,
        passive: raw.passive,
        once: raw.once,
        location: format!("{script}:{line}:{column}"),
        url: url.map(str::to_owned),
        line,
        column,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const NONE: [&str; 0] = [];

    fn raw() -> RawListener {
        serde_json::from_value(json!({
            "type": "click",
            "useCapture": false,
            "passive": true,
            "once": false,
            "scriptId": "42",
            "lineNumber": 9,
            "columnNumber": 4
        }))
        .unwrap()
    }

    // nodeLabel

    #[test]
    fn 태그_id_class로_짧은_라벨을_만든다() {
        assert_eq!(
            node_label(
                "BUTTON",
                &["id", "login", "class", "btn  primary", "type", "submit"]
            ),
            "button#login.btn.primary"
        );
        assert_eq!(node_label("DIV", &NONE), "div");
    }

    #[test]
    fn class는_3개까지만_쓰고_나머지는_말줄임표로_줄인다() {
        assert_eq!(
            node_label(
                "BUTTON",
                &["class", "flex w-12 h-12 rounded-full items-center"]
            ),
            "button.flex.w-12.h-12…"
        );
    }

    #[test]
    fn document와_window는_이름_그대로_쓴다() {
        assert_eq!(node_label("#document", &NONE), "document");
        assert_eq!(node_label("window", &NONE), "window");
    }

    // snippetAt

    #[test]
    fn 여러_줄_소스는_해당_줄부터_max_lines줄을_자른다() {
        let src = ["a", "b", "function onClick() {", "  send()", "}", "z"].join("\n");
        let opts = SnippetOptions {
            max_lines: Some(3),
            ..Default::default()
        };
        assert_eq!(
            snippet_at(&src, 2, 0, opts),
            "function onClick() {\n  send()\n}"
        );
    }

    #[test]
    fn 한_줄짜리_minified_번들은_컬럼_주변만_잘라_앞뒤에_말줄임표를_붙인다() {
        let src = format!(
            "{}function h(e){{sign(e)}}{}",
            "x".repeat(5_000),
            "y".repeat(5_000)
        );
        let opts = SnippetOptions {
            max_chars: Some(60),
            ..Default::default()
        };
        let s = snippet_at(&src, 0, 5_000, opts);
        assert!(s.starts_with("…function h(e){sign(e)}"));
        assert!(s.ends_with('…'));
        assert!(utf16_len(&s) <= 62);
    }

    #[test]
    fn 범위를_벗어난_위치는_빈_문자열이다() {
        assert_eq!(snippet_at("a\nb", 9, 0, SnippetOptions::default()), "");
    }

    // normalizeListener

    #[test]
    fn 영_기반_위치를_1_기반_url_line_col로_바꾸고_붙은_요소를_표시한다() {
        let owner = ListenerOwner {
            depth: 0,
            label: "button#login".into(),
        };
        let l = normalize_listener(&raw(), &owner, Some("https://a.io/app.js"));
        let l = serde_json::to_value(l).unwrap();
        for (field, expected) in [
            ("type", json!("click")),
            ("on", json!("self (button#login)")),
            ("capture", json!(false)),
            ("passive", json!(true)),
            ("once", json!(false)),
            ("location", json!("https://a.io/app.js:10:5")),
        ] {
            assert_eq!(l[field], expected, "{field}");
        }
    }

    #[test]
    fn 조상_리스너는_몇_단계_위인지_url이_없으면_script_id로_표시한다() {
        let owner = ListenerOwner {
            depth: 3,
            label: "div#root".into(),
        };
        let l = normalize_listener(&raw(), &owner, None);
        assert_eq!(l.on, "ancestor +3 (div#root)");
        assert_eq!(l.location, "script#42:10:5");
    }

    // Not in the TypeScript suite: lengths are UTF-16, and the cut ends are marked.

    #[test]
    fn 길이는_utf16_단위로_세고_잘린_쪽에만_말줄임표를_붙인다() {
        let opts = |max_chars| SnippetOptions {
            max_chars: Some(max_chars),
            ..Default::default()
        };
        // Two UTF-16 units each: four units fit, a fifth is cut off.
        assert_eq!(snippet_at("😀😀😀", 0, 0, opts(4)), "😀😀…");
        assert_eq!(snippet_at("😀😀😀", 0, 1, opts(4)), "…\u{fffd}😀\u{fffd}…");
        assert_eq!(snippet_at("abcdef", 0, 4, opts(3)), "…ef");
        assert_eq!(snippet_at("abcdef", 0, -5, opts(3)), "abc…");
        assert_eq!(snippet_at("ab\ncd\nef", 1, 0, opts(4)), "cd\ne…");
        assert_eq!(snippet_at("a\nb", -1, 0, SnippetOptions::default()), "");
    }
}
