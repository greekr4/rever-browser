//! Port of `src/main/mcp/frames.ts` — child frames, resolved far enough to splice
//! each one's accessibility tree into the outline under its `<iframe>`.
//!
//! The TypeScript asked Chrome for every frame's tree over the DevTools protocol.
//! Here the page script (see `snapshot.rs`) walks each same-origin frame document
//! while it walks the page and applies the depth and count limits below; this
//! module turns what it reports into frame trees. A cross-origin frame's document
//! cannot be read from the page, so it never appears here: the outline keeps its
//! bare `Iframe` line and counts it as not entered.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use crate::snapshot::AxNode;

/// One child frame, resolved far enough to splice its accessibility tree into
/// the parent outline under the `<iframe>` element that owns it.
#[derive(Clone, Debug)]
pub struct FrameTree {
    pub frame_id: String,
    /// backendNodeId of the owning `<iframe>` element in the PARENT document.
    pub owner_backend_node_id: u64,
    pub by_id: HashMap<String, AxNode>,
    pub root_node_id: String,
    /// Document URL, used to label a frame that has no anchor in the outline.
    pub url: Option<String>,
}

/// Frames deeper than this are ignored; matches the iframe nesting seen in practice.
pub const MAX_DEPTH: usize = 5;

/// Hard cap so an ad-riddled page cannot turn one snapshot into hundreds of frames.
pub const MAX_FRAMES: usize = 24;

/// A tree this small is a bare document root — the frame has not painted yet.
/// Three, where the TypeScript had two: the page script always reports the
/// document, `<html>` and `<body>`, while Chrome's tree of a blank frame held two nodes.
const EMPTY_TREE_NODES: usize = 3;

/// One frame as the page script reports it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FramePayload {
    pub frame_id: String,
    pub owner_backend_node_id: Option<u64>,
    pub url: Option<String>,
    pub nodes: Vec<AxNode>,
}

#[derive(Clone, Debug, Default)]
pub struct CollectedFrames {
    pub frames: Vec<FrameTree>,
    /// Frames that answered with nothing usable.
    pub unreachable: usize,
    /// Frames entered but still empty (likely still loading).
    pub empty: usize,
}

/// Pick the frame's document root by ROLE, not by position or by reachability.
/// nodes[0] is not reliably the root, and "the node nothing claims as a child"
/// is not either: a per-frame response carries some of the parent document's
/// nodes too, so several are unclaimed and the walk can start on one that
/// renders nothing. RootWebArea is the document itself and is unambiguous.
pub fn pick_root(nodes: &[AxNode]) -> Option<&AxNode> {
    let claimed: HashSet<&str> = nodes
        .iter()
        .flat_map(|n| n.child_ids.iter().map(String::as_str))
        .collect();
    nodes
        .iter()
        .find(|n| n.role() == "RootWebArea")
        .or_else(|| {
            nodes
                .iter()
                .find(|n| !claimed.contains(n.node_id.as_str()) && !n.child_ids.is_empty())
        })
        .or_else(|| nodes.first())
}

/// Collect the accessibility tree of every reachable child frame.
///
/// Payment widgets, embedded auth and OAuth consent screens all live in
/// frames, and those are the highest value targets for reversing, so each
/// frame's tree is kept apart — node ids collide across documents — and
/// spliced in by the outline walk.
pub fn collect_frames(payload: Vec<FramePayload>) -> CollectedFrames {
    let mut out = CollectedFrames::default();
    for frame in payload.into_iter().take(MAX_FRAMES) {
        let (Some(owner), Some(root)) = (frame.owner_backend_node_id, pick_root(&frame.nodes))
        else {
            out.unreachable += 1;
            continue;
        };
        // A frame that answers with nothing but its root has not finished loading.
        if frame.nodes.len() <= EMPTY_TREE_NODES {
            out.empty += 1;
        }
        let root_node_id = root.node_id.clone();
        out.frames.push(FrameTree {
            frame_id: frame.frame_id,
            owner_backend_node_id: owner,
            // Empty counts as absent, like the `||` the TypeScript read it with.
            url: frame.url.filter(|url| !url.is_empty()),
            root_node_id,
            by_id: frame
                .nodes
                .into_iter()
                .map(|n| (n.node_id.clone(), n))
                .collect(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn nodes(value: Value) -> Vec<AxNode> {
        serde_json::from_value(value).unwrap()
    }

    fn frames(value: Value) -> Vec<FramePayload> {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn 루트는_위치가_아니라_root_web_area_역할로_고른다() {
        let list = nodes(json!([
            { "nodeId": "7", "role": { "value": "generic" }, "childIds": ["8"] },
            { "nodeId": "8", "role": { "value": "button" } },
            { "nodeId": "1", "role": { "value": "RootWebArea" }, "childIds": ["7"] }
        ]));

        assert_eq!(pick_root(&list).unwrap().node_id, "1");
    }

    #[test]
    fn 루트_역할이_없으면_자식을_가진_미소유_노드를_고른다() {
        let list = nodes(json!([
            { "nodeId": "5" },
            { "nodeId": "6", "childIds": ["9"] },
            { "nodeId": "9", "childIds": ["5"] }
        ]));

        assert_eq!(pick_root(&list).unwrap().node_id, "6");
    }

    #[test]
    fn 고를_기준이_없으면_첫_노드를_쓰고_비어_있으면_없다() {
        let list = nodes(json!([{ "nodeId": "3" }, { "nodeId": "4" }]));

        assert_eq!(pick_root(&list).unwrap().node_id, "3");
        assert!(pick_root(&[]).is_none());
    }

    #[test]
    fn 프레임마다_노드_맵과_루트를_만든다() {
        let collected = collect_frames(frames(json!([{
            "frameId": "f12",
            "ownerBackendNodeId": 12,
            "url": "https://pay.test/widget",
            "nodes": [
                { "nodeId": "20", "role": { "value": "RootWebArea" }, "childIds": ["21"] },
                { "nodeId": "21", "role": { "value": "generic" }, "childIds": ["22"] },
                { "nodeId": "22", "role": { "value": "generic" }, "childIds": ["23"] },
                { "nodeId": "23", "role": { "value": "button" } }
            ]
        }])));

        assert_eq!(collected.unreachable, 0);
        assert_eq!(collected.empty, 0);
        let frame = &collected.frames[0];
        assert_eq!(frame.frame_id, "f12");
        assert_eq!(frame.owner_backend_node_id, 12);
        assert_eq!(frame.root_node_id, "20");
        assert_eq!(frame.url.as_deref(), Some("https://pay.test/widget"));
        assert_eq!(frame.by_id.len(), 4);
    }

    #[test]
    fn 노드가_없거나_소유_요소를_모르는_프레임은_도달_불가로_센다() {
        let collected = collect_frames(frames(json!([
            { "frameId": "a", "ownerBackendNodeId": 1, "nodes": [] },
            { "frameId": "b", "nodes": [{ "nodeId": "1" }] },
            {}
        ])));

        assert!(collected.frames.is_empty());
        assert_eq!(collected.unreachable, 3);
    }

    #[test]
    fn 문서_뼈대만_있는_프레임은_들어가되_빈_프레임으로_센다() {
        let collected = collect_frames(frames(json!([{
            "frameId": "f1",
            "ownerBackendNodeId": 1,
            "url": "",
            "nodes": [
                { "nodeId": "2", "role": { "value": "RootWebArea" }, "childIds": ["3"] },
                { "nodeId": "3", "role": { "value": "generic" }, "childIds": ["4"] },
                { "nodeId": "4", "role": { "value": "generic" } }
            ]
        }])));

        assert_eq!(collected.frames.len(), 1);
        assert_eq!(collected.empty, 1);
        assert_eq!(collected.frames[0].url, None);
    }

    #[test]
    fn 프레임_수는_상한에서_잘린다() {
        let many: Vec<Value> = (0..MAX_FRAMES + 6)
            .map(|i| {
                json!({
                    "frameId": format!("f{i}"),
                    "ownerBackendNodeId": i,
                    "nodes": [{ "nodeId": "1" }, { "nodeId": "2" }, { "nodeId": "3" }]
                })
            })
            .collect();

        assert_eq!(collect_frames(frames(json!(many))).frames.len(), MAX_FRAMES);
    }
}
