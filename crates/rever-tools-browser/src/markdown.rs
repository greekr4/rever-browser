//! Page HTML to Markdown for the snapshot tool.
//!
//! The snapshot tree tells the agent where things are; Markdown tells it what
//! the page says. Parsing happens on a serialized `outerHTML` string, so the
//! adapter below flattens an html5ever `RcDom` into the arena the converter
//! walks (the same shape as the crate's own test support, minus the test
//! wrapper).

use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{NodeData, RcDom};
use rever_html2md::{Dom, NodeKind};
use serde_json::Value;

use crate::cdp_eval::eval_in_page;
use rever_tools_api::tool::Browser;

/// How much Markdown one snapshot may append. The tree still has to fit the
/// result budget after it.
const MARKDOWN_CAP: usize = 24_000;

/// The page's body text as Markdown, truncated to [`MARKDOWN_CAP`].
pub(crate) fn html_to_markdown(html: &str) -> String {
    let tree = Tree::parse(html);
    let root = tree.body_root();
    let markdown = rever_html2md::convert(&tree, root);
    truncate(&markdown)
}

/// The section `browser_snapshot` appends for `markdown=true`. The active
/// tab's serialized document, converted; empty when evaluation fails so a
/// busy page costs the snapshot nothing extra.
pub(crate) async fn markdown_section(browser: &dyn Browser) -> String {
    let html = match eval_in_page(browser, "document.documentElement.outerHTML").await {
        Ok(Value::String(html)) => html,
        _ => return String::new(),
    };
    format!("\n\n--- page markdown ---\n{}\n", html_to_markdown(&html))
}

fn truncate(markdown: &str) -> String {
    match markdown.char_indices().nth(MARKDOWN_CAP) {
        Some((at, _)) => format!(
            "{}\n\n[markdown truncated at {MARKDOWN_CAP} chars]",
            &markdown[..at]
        ),
        None => markdown.to_owned(),
    }
}

/// A flattened copy of one parsed document.
struct Tree {
    nodes: Vec<Node>,
    body: Option<usize>,
}

struct Node {
    tag: Option<String>,
    text: Option<String>,
    attrs: Vec<(String, String)>,
    first: Option<usize>,
    next: Option<usize>,
}

impl Tree {
    fn parse(html: &str) -> Self {
        let parsed =
            html5ever::parse_document(RcDom::default(), Default::default()).one(html.to_owned());
        let mut nodes: Vec<Node> = Vec::new();
        let mut body = None;
        // RcDom's destructor clears descendants even when handles survive.
        // Keep the parsed document alive until the complete arena is copied.
        let mut stack = vec![(parsed.document.clone(), None::<usize>)];
        let mut last_children: Vec<Option<usize>> = Vec::new();
        while let Some((node, parent)) = stack.pop() {
            let id = nodes.len();
            let (tag, text, attrs) = match &node.data {
                NodeData::Document => (None, None, Vec::new()),
                NodeData::Element { name, attrs, .. } => (
                    Some(name.local.to_string()),
                    None,
                    attrs
                        .borrow()
                        .iter()
                        .map(|a| (a.name.local.to_string(), a.value.to_string()))
                        .collect(),
                ),
                NodeData::Text { contents } => {
                    (None, Some(contents.borrow().to_string()), Vec::new())
                }
                _ => (None, None, Vec::new()),
            };
            if tag.as_deref() == Some("body") {
                body = Some(id);
            }
            nodes.push(Node {
                tag,
                text,
                attrs,
                first: None,
                next: None,
            });
            last_children.push(None);
            if let Some(parent) = parent {
                if let Some(previous) = last_children[parent] {
                    nodes[previous].next = Some(id);
                } else {
                    nodes[parent].first = Some(id);
                }
                last_children[parent] = Some(id);
            }
            stack.extend(
                node.children
                    .borrow()
                    .iter()
                    .rev()
                    .map(|child| (child.clone(), Some(id))),
            );
        }
        Self { nodes, body }
    }

    /// `body` when the parse produced one, else the document node.
    fn body_root(&self) -> usize {
        self.body.unwrap_or(0)
    }
}

impl Dom for Tree {
    type NodeId = usize;

    fn node_kind(&self, node: usize) -> NodeKind<'_> {
        let node = &self.nodes[node];
        if let Some(tag) = &node.tag {
            NodeKind::Element(tag)
        } else if let Some(text) = &node.text {
            NodeKind::Text(text)
        } else if node.first.is_some() {
            // The document node itself: children still get visited.
            NodeKind::Document
        } else {
            NodeKind::Other
        }
    }

    fn first_child(&self, node: usize) -> Option<usize> {
        self.nodes[node].first
    }

    fn next_sibling(&self, node: usize) -> Option<usize> {
        self.nodes[node].next
    }

    fn attribute(&self, node: usize, name: &str) -> Option<&str> {
        self.nodes[node]
            .attrs
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 제목과_강조를_마크다운으로_옮긴다() {
        let html = "<html><body><h1>보고서</h1><p>첫 줄 <b>강조</b> 그리고 <a href='/x'>링크</a></p></body></html>";
        let markdown = html_to_markdown(html);
        assert!(markdown.contains("# 보고서"), "{markdown}");
        assert!(markdown.contains("**강조**"), "{markdown}");
        assert!(markdown.contains("[링크](/x)"), "{markdown}");
    }

    #[test]
    fn 표를_마크다운_표로_옮긴다() {
        let html = "<table><tr><th>이름</th><th>값</th></tr><tr><td>a</td><td>1</td></tr></table>";
        let markdown = html_to_markdown(html);
        assert!(markdown.contains("| 이름 |"), "{markdown}");
        assert!(markdown.contains("| a | 1 |"), "{markdown}");
    }

    #[test]
    fn body가_없으면_문서_전체를_변환한다() {
        let markdown = html_to_markdown("<p>조각 텍스트</p>");
        assert!(markdown.contains("조각 텍스트"), "{markdown}");
    }

    #[test]
    fn 길면_잘리고_표시를_남긴다() {
        let html = format!("<p>{}</p>", "긴글".repeat(20_000));
        let markdown = html_to_markdown(&html);
        assert!(markdown.contains("[markdown truncated"), "{markdown}");
    }
}
