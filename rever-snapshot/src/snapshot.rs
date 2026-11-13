//! Port of `src/main/mcp/snapshot.ts` — the page as an accessibility outline whose
//! actionable lines carry short refs (`r1`, `r2`, …) the agent clicks and types by.
//!
//! The TypeScript read Chrome's accessibility tree, DOM snapshot and frame trees
//! over the DevTools protocol. This engine offers none of that: the only page
//! access is [`Browser::evaluate`]. So one script, `page_script.js`, walks the
//! DOM inside the page and reports the same facts in the same payload shapes
//! (role, name, value, states, boxes, click signals, same-origin frames), and the
//! outline is built from them here exactly as before. What was a backendNodeId
//! is a number the script gives each element for as long as it lives; the
//! elements behind the current refs are kept page-side, so a later action finds
//! them again by ref.
//!
//! Gestures are not here. Clicking, hovering, typing and dragging were this
//! file's wrappers around the human-shaped input path; a tool builds them from
//! [`Snapshotter::point_for_ref`] and its own input code, so no shortcut exists
//! that skips that path.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use rever_tools_api::tool::{Browser, Input};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::frames::{self, FramePayload, FrameTree, collect_frames};
use crate::layout::{
    CaptureResult, ClickScanPayload, MAX_SCANNED_ELEMENTS, MAX_SHADOW_DEPTH, NodeLayout,
    PageLayout, Viewport, capture_page_layout, scan_clickable,
};

/// The page-side half: a function expression called as `(script)(command, argument)`.
const PAGE_SCRIPT: &str = include_str!("page_script.js");

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct AxValue {
    pub value: Option<Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct AxProperty {
    pub name: String,
    pub value: AxValue,
}

/// One accessibility node, in the shape Chrome's `Accessibility.getFullAXTree`
/// reported it and the page script now does.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AxNode {
    pub node_id: String,
    pub ignored: bool,
    pub child_ids: Vec<String>,
    pub role: Option<AxValue>,
    pub name: Option<AxValue>,
    pub value: Option<AxValue>,
    pub properties: Vec<AxProperty>,
    #[serde(rename = "backendDOMNodeId")]
    pub backend_dom_node_id: Option<u64>,
}

impl AxNode {
    /// The role name; empty when the node has none.
    pub fn role(&self) -> String {
        text_of(self.role.as_ref())
    }

    /// The accessible name, trimmed; empty when the node has none.
    pub fn name(&self) -> String {
        js_trim(&text_of(self.name.as_ref())).to_owned()
    }
}

fn text_of(value: Option<&AxValue>) -> String {
    value
        .and_then(|v| v.value.as_ref())
        .map(js_string)
        .unwrap_or_default()
}

/// JavaScript's `String(value)` for the values a payload carries.
fn js_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        // A whole number prints without a fraction, however it was encoded.
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() < 1e21 => format!("{f:.0}"),
            _ => n.to_string(),
        },
        other => other.to_string(),
    }
}

/// JavaScript truthiness.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// JavaScript's `String.prototype.trim`: its whitespace set has U+FEFF and lacks
/// U+0085, unlike Rust's.
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

#[derive(Clone, Debug, PartialEq)]
pub struct RefEntry {
    /// The page script's number for the element.
    pub backend_node_id: u64,
    pub role: String,
    pub name: String,
}

const SKIP_ROLES: [&str; 9] = [
    "none",
    "generic",
    "InlineTextBox",
    "LineBreak",
    "presentation",
    "LayoutTable",
    "LayoutTableRow",
    "LayoutTableCell",
    "LayoutTableColumn",
];

/// Roles whose subtree is label, not structure. A button or link already prints
/// its accessible name, so everything under it repeats information the agent
/// has - except a control nested inside, which keeps its own ref and survives.
const LABEL_ONLY_SUBTREE: [&str; 6] = ["button", "link", "menuitem", "tab", "option", "switch"];

const ACTIONABLE_ROLES: [&str; 13] = [
    "button",
    "link",
    "textbox",
    "searchbox",
    "combobox",
    "checkbox",
    "radio",
    "menuitem",
    "tab",
    "switch",
    "slider",
    "option",
    // A `<select multiple>` renders as a listbox whose options carry refs while
    // the element itself carried none — leaving browser_select_option with
    // nothing to address, so a multi-select could not be answered at all.
    "listbox",
];

/// Depth-limited: labels come from a node's own text, not a whole subtree dump.
const TEXT_LABEL_DEPTH: usize = 3;

/// First piece of visible text under a node, used to name role-less click targets.
pub fn first_text(node: &AxNode, scope: &HashMap<String, AxNode>, depth: usize) -> String {
    if depth > TEXT_LABEL_DEPTH {
        return String::new();
    }
    for id in &node.child_ids {
        let Some(child) = scope.get(id) else { continue };
        let name = child.name();
        if child.role() == "StaticText" && !name.is_empty() {
            return name;
        }
        let nested = first_text(child, scope, depth + 1);
        if !nested.is_empty() {
            return nested;
        }
    }
    String::new()
}

/// `JSON.stringify` of the text cut to 80 UTF-16 units, as the outline prints names.
fn quote(text: &str) -> String {
    const MAX_UNITS: usize = 80;
    let json = |text: &str| Value::from(text).to_string();
    if text.encode_utf16().count() <= MAX_UNITS {
        return json(text);
    }
    let mut units: Vec<u16> = text.encode_utf16().take(MAX_UNITS).collect();
    // The cut can fall inside a surrogate pair. JSON.stringify prints the lone half
    // as an escape, so the same text reads the same on both sides.
    let lone = units
        .pop_if(|unit| (0xD800..0xDC00).contains(unit))
        .map(|unit| format!("\\u{unit:04x}"))
        .unwrap_or_default();
    let mut out = json(&String::from_utf16_lossy(&units));
    out.pop();
    out + &lone + "…\""
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotStats {
    /// Actionable nodes that got a ref.
    pub refs: usize,
    /// Nodes dropped because nothing is painted for them (hidden or covered).
    pub hidden: usize,
    /// Nodes dropped because their box sits outside the viewport.
    pub offscreen: usize,
    /// Refs that exist only because the click scan found them (no ARIA role).
    pub click_only_refs: usize,
    /// Elements the in-page click scan reported, before correlation.
    pub click_scanned: usize,
    /// True when the page was too large and the scan never ran. Every click-only
    /// ref disappears with it, so the outline silently loses `<div onClick>`
    /// targets — an infinite-scroll list growing past the cap did exactly that
    /// mid-session, and nothing said so.
    pub click_scan_skipped: bool,
    /// How many of those correlated to a snapshot node.
    pub click_matched: usize,
    /// True when the filter was discarded because it suppressed everything.
    pub fell_back_to_full: bool,
    /// Child frames whose content was spliced into the tree.
    pub frames: usize,
    /// `<iframe>` elements present in the outline that could NOT be entered.
    ///
    /// Counted from the tree, not from the list of frames that were read: a
    /// cross-origin frame's document is out of the page script's reach and never
    /// appears in that list at all, so counting failures there reported 0 while
    /// a whole frame was missing.
    pub frames_unreachable: usize,
    /// Frames entered but still empty (likely still loading).
    pub frames_empty: usize,
    /// Refs marked *new because they were absent from the previous snapshot.
    pub new_refs: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotResult {
    pub url: String,
    pub title: String,
    pub tree: String,
    pub stats: SnapshotStats,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct PageMeta {
    url: String,
    title: String,
    scroll_x: f64,
    scroll_y: f64,
    inner_width: f64,
    inner_height: f64,
}

/// Everything one run of the page script's `capture` reports.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct CapturePayload {
    meta: PageMeta,
    nodes: Vec<AxNode>,
    frames: Vec<FramePayload>,
    /// Frames the script could not read that have no node in the tree either.
    unlisted_frames: Vec<FramePayload>,
    layout: Option<CaptureResult>,
    click: Option<ClickScanPayload>,
}

/// Tally of actionable nodes the viewport filter kept out of the tree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterTally {
    pub hidden: usize,
    pub below: usize,
    pub above: usize,
    pub side: usize,
    pub nearest_below: f64,
    pub nearest_above: f64,
}

/// Count what the filter excludes, by scanning every AX node directly.
///
/// This deliberately does NOT reuse the tree walk. The walk prunes whole
/// subtrees when a container falls off-screen, so an actionable node buried
/// under one is never visited — counting there under-reports by an order of
/// magnitude and suppresses the scroll hints that make pruning safe.
pub fn tally_filtered<'a>(
    nodes: impl IntoIterator<Item = &'a AxNode>,
    layout: &PageLayout,
) -> FilterTally {
    let mut t = FilterTally {
        hidden: 0,
        below: 0,
        above: 0,
        side: 0,
        nearest_below: f64::INFINITY,
        nearest_above: f64::INFINITY,
    };
    let v = layout.viewport;

    for n in nodes {
        let Some(backend_id) = n.backend_dom_node_id else {
            continue;
        };
        if n.ignored || !ACTIONABLE_ROLES.contains(&n.role().as_str()) {
            continue;
        }
        let Some(lay) = layout.get(backend_id) else {
            continue;
        };

        if !lay.rendered || lay.occluded || lay.zero_size {
            t.hidden += 1;
            continue;
        }
        if lay.in_viewport {
            continue;
        }

        let below = lay.y - (v.y + v.height);
        let above = v.y - (lay.y + lay.height);
        if below > 0.0 {
            t.below += 1;
            t.nearest_below = t.nearest_below.min(below);
        } else if above > 0.0 {
            t.above += 1;
            t.nearest_above = t.nearest_above.min(above);
        } else {
            t.side += 1;
        }
    }

    t
}

pub fn describe_offscreen(t: &FilterTally) -> Vec<String> {
    // floor(x + 0.5) is Math.round.
    let px = |distance: f64| (distance + 0.5).floor().max(0.0);
    let mut out = Vec::new();
    // The distance is to the NEAREST element, not to all of them — say so, or a
    // "scroll down ~25px" hint alongside a count of 138 reads as "25px reveals
    // everything" and the agent scrolls one row at a time.
    if t.below > 0 {
        out.push(format!(
            "- [{} more actionable element(s) below the fold — nearest ~{}px down]",
            t.below,
            px(t.nearest_below)
        ));
    }
    if t.above > 0 {
        out.push(format!(
            "- [{} more actionable element(s) above the fold — nearest ~{}px up]",
            t.above,
            px(t.nearest_above)
        ));
    }
    if t.side > 0 {
        out.push(format!(
            "- [{} more actionable element(s) outside the horizontal viewport]",
            t.side
        ));
    }
    out
}

/// The accessibility trees of one capture: the page's and each entered frame's.
struct Trees {
    main: HashMap<String, AxNode>,
    /// The first node the page reported, where the outline starts.
    root_id: Option<String>,
    frames: Vec<FrameTree>,
    /// What to call each frame that was neither read nor given a line by the walk.
    unlisted_frames: Vec<String>,
    frames_empty: usize,
    click_scan_skipped: bool,
}

struct Built {
    tree: String,
    stats: SnapshotStats,
    ref_targets: HashSet<u64>,
    refs: HashMap<String, RefEntry>,
}

/// The walk recurses once per tree level. Documents nest far shallower than this,
/// and a payload that does not must not be able to exhaust the stack.
const MAX_WALK_DEPTH: usize = 512;

/// One pass over the trees that writes the outline and hands out refs.
struct Outline<'a> {
    trees: &'a Trees,
    /// Owning `<iframe>` → index of the frame it holds.
    frames_by_owner: HashMap<u64, usize>,
    filter: Option<&'a PageLayout>,
    prune: bool,
    same_document: bool,
    previous_ref_targets: &'a HashSet<u64>,
    refs: HashMap<String, RefEntry>,
    counter: usize,
    click_only_refs: usize,
    new_refs: usize,
    seen_ref_targets: HashSet<u64>,
    frames_entered: usize,
    frames_missed: usize,
    spliced_frames: HashSet<usize>,
    lines: Vec<String>,
    /// A node is written once; a payload whose child lists loop cannot run the walk forever.
    visited: HashSet<(Option<usize>, &'a str)>,
}

/// Where a walk stands: what its ancestors already said about the node.
#[derive(Clone, Copy)]
struct Walk<'s> {
    depth: usize,
    parent_name: &'s str,
    ancestor_claimed: bool,
    /// `None` is the page's own tree, `Some(i)` the tree of frame `i`.
    scope: Option<usize>,
    ancestor_control: bool,
    level: usize,
}

impl<'a> Outline<'a> {
    fn next_ref(&mut self) -> String {
        self.counter += 1;
        format!("r{}", self.counter)
    }

    fn walk(&mut self, id: &str, at: Walk<'_>) {
        let trees = self.trees;
        let scope = match at.scope {
            None => &trees.main,
            Some(frame) => match trees.frames.get(frame) {
                Some(frame) => &frame.by_id,
                None => return,
            },
        };
        let Some((key, n)) = scope.get_key_value(id) else {
            return;
        };
        if at.level > MAX_WALK_DEPTH || !self.visited.insert((at.scope, key.as_str())) {
            return;
        }
        let role = n.role();
        let name = n.name();

        // Layout gate. A node with no layout entry is left alone — absence of
        // data must never be read as "invisible", or whole pages would vanish.
        // Counting happens in tally_filtered, not here: this prunes subtrees, so
        // anything below a pruned container is never reached.
        let lay = match (self.filter, n.backend_dom_node_id) {
            (Some(filter), Some(backend_id)) => filter.get(backend_id),
            _ => None,
        };
        // A zero-size box is invisible on its own but its children can overflow
        // it, so it must not take the subtree down with it — only style-hidden,
        // covered and off-screen boxes do.
        if self.prune
            && let Some(lay) = lay
            && !lay.zero_size
            && (!lay.rendered || lay.occluded || !lay.in_viewport)
        {
            return;
        }

        let actionable = ACTIONABLE_ROLES.contains(&role.as_str());
        // A click signal with no interactive ARIA role — the `<div onClick>`
        // case. Only the outermost such node in a chain gets a ref, so a button
        // wrapped in three pointer-cursor divs still yields exactly one.
        let claimable = lay.is_some_and(|lay| lay.clickable)
            && !at.ancestor_claimed
            && !actionable
            && n.backend_dom_node_id.is_some();

        // StaticText is collapsed into its parent's name. Suppress entirely if it
        // duplicates the parent name (Playwright MCP rule); otherwise emit as a
        // single quoted text line without a ref (text is not actionable).
        if role == "StaticText" && !claimable {
            if !at.ancestor_control && !name.is_empty() && name != at.parent_name {
                let indent = "  ".repeat(at.depth);
                self.lines.push(format!("{indent}- text {}", quote(&name)));
            }
            return;
        }

        // Inside a link or button, the label text and its wrapper spans carry no
        // information the ancestor line does not already give — a button prints
        // its own accessible name. Emitting them is most of what the outline
        // wastes on a real page. Anything with its own ref survives, so a nested
        // control or a click-scan hit is never swallowed.
        let inside_control = at.ancestor_control && !claimable && !actionable;

        let is_scroller = lay.is_some_and(|lay| lay.scrollable_by > 0.0);
        let skip_role = SKIP_ROLES.contains(&role.as_str());
        let skip = !is_scroller
            && (inside_control
                || (!claimable
                    && (role.is_empty() || n.ignored || (skip_role && name.is_empty()))));

        if !skip {
            self.line(n, scope, &role, &name, lay, claimable, at.depth);
        }
        let next_depth = at.depth + usize::from(!skip);
        let claimed = at.ancestor_claimed || claimable;

        let below = Walk {
            depth: next_depth,
            parent_name: if skip { at.parent_name } else { &name },
            ancestor_claimed: claimed,
            scope: at.scope,
            ancestor_control: at.ancestor_control || LABEL_ONLY_SUBTREE.contains(&role.as_str()),
            level: at.level + 1,
        };
        for child in &n.child_ids {
            self.walk(child, below);
        }

        // Descend into a child frame. Its accessibility tree is a separate
        // document with its own node ids, so the scope switches here rather
        // than being merged.
        let frame = n
            .backend_dom_node_id
            .and_then(|backend_id| self.frames_by_owner.get(&backend_id).copied());
        if frame.is_none() && role == "Iframe" {
            self.frames_missed += 1;
        }
        if let Some(index) = frame
            && let Some(frame) = trees.frames.get(index)
        {
            self.frames_entered += 1;
            self.spliced_frames.insert(index);
            let inside = Walk {
                parent_name: &name,
                scope: Some(index),
                ancestor_control: false,
                ..below
            };
            self.walk(&frame.root_node_id, inside);
        }
    }

    /// Write one node's outline line, giving it a ref when it can be acted on.
    #[allow(clippy::too_many_arguments)]
    fn line(
        &mut self,
        n: &AxNode,
        scope: &HashMap<String, AxNode>,
        role: &str,
        name: &str,
        lay: Option<&NodeLayout>,
        claimable: bool,
        depth: usize,
    ) {
        let scrollable_by = lay.map_or(0.0, |lay| lay.scrollable_by);
        let is_scroller = scrollable_by > 0.0;

        // A click-scan node has no ARIA role or name by definition, so it would
        // print as a bare `- generic [ref=r4]`. Label it by its own text so the
        // agent can tell what it is about to click.
        let label = if claimable && (role.is_empty() || SKIP_ROLES.contains(&role)) {
            "clickable"
        } else if !role.is_empty() {
            role
        } else if is_scroller {
            "scroller"
        } else {
            "generic"
        };
        let shown_name = if !name.is_empty() {
            name.to_owned()
        } else if claimable {
            first_text(n, scope, 0)
        } else {
            String::new()
        };

        let mut parts = vec![format!("- {label}")];
        if !shown_name.is_empty() {
            parts.push(quote(&shown_name));
        }

        if let Some(value) = n.value.as_ref().and_then(|v| v.value.as_ref())
            && !matches!(value, Value::String(text) if text.is_empty())
        {
            parts.push(format!("value={}", quote(&js_string(value))));
        }

        for p in &n.properties {
            let Some(v) = p.value.value.as_ref() else {
                continue;
            };
            let name = p.name.as_str();
            if matches!(name, "checked" | "selected" | "expanded") && truthy(v) {
                parts.push(format!("{name}={}", js_string(v)));
            }
            if name == "disabled" && truthy(v) {
                parts.push("disabled".to_owned());
            }
            if name == "level" && v.is_number() {
                parts.push(format!("level={}", js_string(v)));
            }
        }

        // A scroll container gets a ref too. browser_scroll moves the window,
        // so a list that appends as its OWN box scrolls could be seen but never
        // advanced — the outline said `[scrollable +132px]` with nothing to
        // address. Same for a canvas: the click scan finds it, but a stroke
        // needs the element itself as the drag surface.
        if let Some(backend_id) = n.backend_dom_node_id
            && (ACTIONABLE_ROLES.contains(&role) || claimable || is_scroller)
        {
            let r = self.next_ref();
            if claimable {
                self.click_only_refs += 1;
            }
            if self.same_document && !self.previous_ref_targets.contains(&backend_id) {
                parts.push("*new".to_owned());
                self.new_refs += 1;
            }
            self.seen_ref_targets.insert(backend_id);
            parts.push(format!("[ref={r}]"));
            let entry = RefEntry {
                backend_node_id: backend_id,
                role: if role.is_empty() { "clickable" } else { role }.to_owned(),
                name: shown_name,
            };
            self.refs.insert(r, entry);
            if claimable {
                parts.push("(click-scan)".to_owned());
            }
        }

        // A scrollable box is worth calling out even when it carries no ref:
        // browser_scroll moves the window, so if the real scroller is this
        // container the agent needs to know it exists to reach what is inside.
        if is_scroller {
            parts.push(format!("[scrollable +{scrollable_by}px]"));
        }

        self.lines
            .push(format!("{}{}", "  ".repeat(depth), parts.join(" ")));
    }
}

fn build(
    trees: &Trees,
    filter: Option<&PageLayout>,
    prune: bool,
    same_document: bool,
    previous_ref_targets: &HashSet<u64>,
) -> Built {
    let mut outline = Outline {
        trees,
        // Keyed by the owning element; the script's numbers are unique across
        // documents, so a frame cannot be spliced under a look-alike in another one.
        frames_by_owner: trees
            .frames
            .iter()
            .enumerate()
            .map(|(index, frame)| (frame.owner_backend_node_id, index))
            .collect(),
        filter,
        prune,
        same_document,
        previous_ref_targets,
        refs: HashMap::new(),
        counter: 0,
        click_only_refs: 0,
        new_refs: 0,
        seen_ref_targets: HashSet::new(),
        frames_entered: 0,
        frames_missed: 0,
        spliced_frames: HashSet::new(),
        lines: Vec::new(),
        visited: HashSet::new(),
    };
    let top = Walk {
        depth: 0,
        parent_name: "",
        ancestor_claimed: false,
        scope: None,
        ancestor_control: false,
        level: 0,
    };

    if let Some(root) = &trees.root_id {
        outline.walk(root, top);
    }

    // Elements the click scan found that the accessibility tree has no node
    // for at all. A `<canvas>` with no accessible content is the common case —
    // charts, signature pads, maps and games are all reachable by mouse and
    // completely absent from the outline, so there was nothing to address.
    // Emitted after the walk with their own refs.
    if let Some(filter) = filter {
        for (backend_id, lay) in filter.iter() {
            if !lay.clickable || outline.seen_ref_targets.contains(&backend_id) {
                continue;
            }
            if prune && (!lay.rendered || lay.occluded || !lay.in_viewport) {
                continue;
            }
            if lay.width < 8.0 || lay.height < 8.0 {
                continue;
            }
            let r = outline.next_ref();
            outline.click_only_refs += 1;
            outline.seen_ref_targets.insert(backend_id);
            outline.lines.push(format!(
                "- clickable [ref={r}] (click-scan, not in the a11y tree)"
            ));
            let entry = RefEntry {
                backend_node_id: backend_id,
                role: "clickable".to_owned(),
                name: String::new(),
            };
            outline.refs.insert(r, entry);
        }
    }

    // A frame whose `<iframe>` carries aria-hidden has NO node in the parent
    // accessibility tree, so the walk never reaches an anchor to nest it
    // under. Emitting it at the end keeps it reachable instead of dropping it
    // without a word: a frame the page hides from assistive tech (trackers,
    // 3DS steps, captchas, status embeds) is exactly what a reverser is
    // looking for.
    for (index, frame) in trees.frames.iter().enumerate() {
        if outline.spliced_frames.contains(&index) {
            continue;
        }
        outline.frames_entered += 1;
        let label = frame.url.as_deref().unwrap_or(&frame.frame_id);
        outline.lines.push(format!(
            "- Iframe (hidden from the a11y tree) {}",
            quote(label)
        ));
        let inside = Walk {
            depth: 1,
            scope: Some(index),
            ..top
        };
        outline.walk(&frame.root_node_id, inside);
    }

    // A cross-origin frame under aria-hidden: the script cannot read its document
    // and the walk has no `Iframe` line to count it by. The TypeScript reached such
    // frames over the DevTools protocol; here they are at least listed and counted
    // as not entered, since these are the frames a reverser is looking for.
    for label in &trees.unlisted_frames {
        outline.frames_missed += 1;
        outline.lines.push(format!(
            "- Iframe (hidden from the a11y tree) {}",
            quote(label)
        ));
    }

    let mut stats = SnapshotStats {
        refs: outline.counter,
        click_only_refs: outline.click_only_refs,
        click_scanned: filter.map_or(0, |filter| filter.click_scanned),
        click_scan_skipped: trees.click_scan_skipped,
        click_matched: filter.map_or(0, |filter| filter.click_matched),
        frames: outline.frames_entered,
        frames_unreachable: outline.frames_missed,
        frames_empty: trees.frames_empty,
        new_refs: outline.new_refs,
        ..SnapshotStats::default()
    };
    let mut lines = outline.lines;
    if let Some(filter) = filter
        && prune
    {
        let tally = tally_filtered(trees.main.values(), filter);
        lines.extend(describe_offscreen(&tally));
        stats.hidden = tally.hidden;
        stats.offscreen = tally.below + tally.above + tally.side;
    }

    Built {
        tree: lines.join("\n"),
        stats,
        ref_targets: outline.seen_ref_targets,
        refs: outline.refs,
    }
}

/// The page script called with one command, as an expression to evaluate.
fn script_call(command: &str, argument: &Value) -> String {
    format!("({PAGE_SCRIPT})({}, {argument})", Value::from(command))
}

/// What the page script returned. It answers with a JSON string; an engine that
/// already decoded it is accepted as well.
fn script_result<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    match value {
        Value::String(text) => serde_json::from_str(&text),
        other => serde_json::from_value(other),
    }
    .map_err(|e| format!("the page script returned something unreadable: {e}"))
}

/// A pause that needs no async runtime, so it works on whatever executor runs the tools.
struct Sleep {
    until: Instant,
    /// Shared with the timer thread once it runs; holds the waker of the latest poll.
    waker: Option<Arc<Mutex<Waker>>>,
}

fn sleep(duration: Duration) -> Sleep {
    Sleep {
        until: Instant::now() + duration,
        waker: None,
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let Some(left) = self.until.checked_duration_since(Instant::now()) else {
            return Poll::Ready(());
        };
        if left.is_zero() {
            return Poll::Ready(());
        }
        match &self.waker {
            Some(waker) => {
                *waker.lock().unwrap_or_else(|p| p.into_inner()) = cx.waker().clone();
            }
            None => {
                let waker = Arc::new(Mutex::new(cx.waker().clone()));
                self.waker = Some(waker.clone());
                std::thread::spawn(move || {
                    std::thread::sleep(left);
                    waker
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .wake_by_ref();
                });
            }
        }
        Poll::Pending
    }
}

/// How long an apparently unsettled page gets before the second look.
const SETTLE_DELAY: Duration = Duration::from_millis(300);

/// Pause between wheel steps, so lazy-loading handlers fire the way they would for a person.
const WHEEL_STEP_PAUSE: Duration = Duration::from_millis(40);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SnapshotOptions {
    /// Keep hidden and off-screen nodes instead of summarising them.
    pub full: bool,
}

/// Page-space centre and box of an element, in CSS pixels of the viewport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct RefPoint {
    pub x: f64,
    pub y: f64,
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// A small action the page script performs on an element.
#[derive(Clone, Debug, PartialEq)]
pub enum RefAction {
    /// Keyboard focus, without clicking.
    Focus,
    /// Replace a text field's value and fire `input` and `change`.
    SetValue(String),
    /// Read a form field's value, or any other element's visible text.
    Text,
    /// Choose the options of a `<select>` whose value or label is listed.
    Select(Vec<String>),
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ActionReply {
    ok: Option<Value>,
    error: Option<String>,
    options: Vec<String>,
}

/// Run one action of the page script on the element `target` names.
async fn run_action(
    browser: &dyn Browser,
    mut target: Value,
    action: &str,
    value: Value,
) -> Result<Value, String> {
    if let Some(argument) = target.as_object_mut() {
        argument.insert("action".to_owned(), action.into());
        argument.insert("value".to_owned(), value);
    }
    let reply: ActionReply = script_result(browser.evaluate(&script_call("act", &target)).await?)?;
    match reply.error {
        // On no match the available options come back in the error, so the
        // caller can retry without another snapshot.
        Some(error) if reply.options.is_empty() => Err(error),
        Some(error) => Err(format!("{error} — options: {}", reply.options.join(" | "))),
        None => Ok(reply.ok.unwrap_or(Value::Null)),
    }
}

/// Page-space centre of the first element matching a CSS selector, after
/// scrolling it into view. This lets selector-based tools reuse the exact same
/// input path as ref-based ones WITHOUT needing a prior browser_snapshot — the
/// fix for pages whose snapshot is too big to be practical (the agent used to
/// fall back to raw JS, which fires no trusted events).
pub async fn point_for_selector(browser: &dyn Browser, selector: &str) -> Result<RefPoint, String> {
    let target = json!({ "selector": selector });
    script_result(run_action(browser, target, "point", Value::Null).await?)
}

// Refs are namespaced per tab: a "r3" from tab A must never resolve against
// tab B's tree, and one tab's snapshot must not drop another's live refs. One
// `Snapshotter` holds one tab's refs, so whoever drives several tabs keeps one each.
#[derive(Default)]
struct State {
    refs: HashMap<String, RefEntry>,
    /// Nodes that carried a ref in the previous snapshot, so the next one can point
    /// at what just appeared.
    ///
    /// In browser-use the equivalent marker is an attention aid. Here it is closer
    /// to evidence: paired with the traffic store, "what showed up right after this
    /// request" is how a response field gets traced to the UI that renders it.
    previous_ref_targets: HashSet<u64>,
    previous_url: String,
    /// Which filtering mode produced the baseline. A viewport-filtered snapshot and
    /// a full one contain different node sets, so comparing across them reports
    /// everything the filter had hidden as newly appeared — the first run of this
    /// marked an off-screen button as new when nothing had changed.
    previous_pruned: bool,
}

/// Takes snapshots of one tab and remembers what its refs point at.
pub struct Snapshotter {
    state: Mutex<State>,
    settle_delay: Duration,
}

impl Default for Snapshotter {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            settle_delay: SETTLE_DELAY,
        }
    }
}

impl Snapshotter {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// What a ref of the current snapshot points at.
    pub fn ref_entry(&self, r: &str) -> Option<RefEntry> {
        self.state().refs.get(r).cloned()
    }

    /// Reverse lookup: which ref from the CURRENT snapshot points at this node, if any.
    pub fn ref_for_backend_node_id(&self, backend_node_id: u64) -> Option<String> {
        self.state()
            .refs
            .iter()
            .find(|(_, entry)| entry.backend_node_id == backend_node_id)
            .map(|(r, _)| r.clone())
    }

    /// Capture the page as an accessibility outline.
    ///
    /// By default only what is actually painted inside the viewport is emitted:
    /// hidden, zero-size, overlay-covered and off-screen subtrees are dropped, and
    /// off-screen actionable nodes are summarised as scroll hints instead. That
    /// keeps the tree small enough that the agent keeps using refs rather than
    /// falling back to raw JS (which skips the human-shaped input path and fires
    /// untrusted events). Pass `full` to get the unfiltered tree back.
    pub async fn take_snapshot(
        &self,
        browser: &dyn Browser,
        opts: SnapshotOptions,
    ) -> Result<SnapshotResult, String> {
        let first = self.capture_snapshot(browser, opts).await?;

        // "Actionable elements exist, none of them are visible" is what an unsettled
        // layout looks like: the boxes have been created but not yet placed, so they
        // all read as sitting below the fold. It is also indistinguishable from a
        // correct snapshot of a page whose controls really are all off-screen, which
        // is why this waits and re-reads rather than trying to decide.
        //
        // A cold load of a heavy page produced exactly this - zero refs, while the
        // very next snapshot found 25. The agent's first look at a page is the one
        // that shapes the rest of the session, so an empty first look is expensive.
        let looks_unsettled =
            !opts.full && first.stats.refs == 0 && first.stats.hidden + first.stats.offscreen > 0;
        if !looks_unsettled {
            return Ok(first);
        }

        // shortcut: a flat pause, because the engine reports no network-idle signal
        // to wait on; wait on that instead once `Browser` can say when a page is idle.
        sleep(self.settle_delay).await;
        let second = self.capture_snapshot(browser, opts).await?;
        Ok(if second.stats.refs > 0 { second } else { first })
    }

    async fn capture_snapshot(
        &self,
        browser: &dyn Browser,
        opts: SnapshotOptions,
    ) -> Result<SnapshotResult, String> {
        let limits = json!({
            "maxShadowDepth": MAX_SHADOW_DEPTH,
            "maxScannedElements": MAX_SCANNED_ELEMENTS,
            "maxFrameDepth": frames::MAX_DEPTH,
            "maxFrames": frames::MAX_FRAMES,
        });
        let raw = browser.evaluate(&script_call("capture", &limits)).await?;
        let capture: CapturePayload = script_result(raw)?;
        let meta = capture.meta;

        let viewport = Viewport {
            x: meta.scroll_x,
            y: meta.scroll_y,
            width: meta.inner_width,
            height: meta.inner_height,
        };

        // `full` only turns off viewport PRUNING — the click scan still runs. The
        // agent reaches for full=true when it cannot find something, so having that
        // switch also drop the click-scan refs would remove exactly the elements it
        // is looking for.
        let click = scan_clickable(capture.click);
        // The script measures in CSS px, the space the viewport is in, so no scale applies.
        let layout = capture
            .layout
            .and_then(|snap| capture_page_layout(snap, viewport, Some(&click.paths), 1.0));
        let collected = collect_frames(capture.frames);
        let trees = Trees {
            root_id: capture.nodes.first().map(|n| n.node_id.clone()),
            main: capture
                .nodes
                .into_iter()
                .map(|n| (n.node_id.clone(), n))
                .collect(),
            frames: collected.frames,
            unlisted_frames: capture
                .unlisted_frames
                .into_iter()
                .map(|frame| {
                    frame
                        .url
                        .filter(|url| !url.is_empty())
                        .unwrap_or(frame.frame_id)
                })
                .collect(),
            frames_empty: collected.empty,
            click_scan_skipped: click.skipped,
        };
        let pruned = !opts.full;

        let (result, ref_ids) = {
            let mut state = self.state();
            // After a navigation every node is new, so the marker would say nothing.
            // Only compare within the same document.
            let same_document = state.previous_url == meta.url
                && !state.previous_ref_targets.is_empty()
                && state.previous_pruned == pruned;
            let previous = &state.previous_ref_targets;

            let mut built = build(&trees, layout.as_ref(), pruned, same_document, previous);

            // Safety valve for a coordinate mismatch (detached document, odd frame
            // setup): the filter suppressed everything AND cannot say why.
            //
            // "Cannot say why" is the load-bearing half. A full-screen modal legitimately
            // covers every actionable element, which also yields zero refs — firing the
            // valve there throws away a correct result and silently disables the filter
            // exactly when the page is most confusing. So only fall back when the tally
            // accounts for nothing.
            if layout.is_some() && pruned && built.stats.refs == 0 {
                let explained = built.stats.hidden > 0 || built.stats.offscreen > 0;
                if !explained {
                    built = build(&trees, layout.as_ref(), false, same_document, previous);
                    // Only a real mismatch if dropping the filter actually reveals controls.
                    // An error page with no controls at all also lands here, and claiming a
                    // coordinate problem there is a false alarm.
                    built.stats.fell_back_to_full = built.stats.refs > 0;
                }
            }

            let ref_ids: serde_json::Map<String, Value> = built
                .refs
                .iter()
                .map(|(r, entry)| (r.clone(), entry.backend_node_id.into()))
                .collect();
            state.refs = built.refs;
            state.previous_ref_targets = built.ref_targets;
            state.previous_url = meta.url.clone();
            state.previous_pruned = pruned;

            let result = SnapshotResult {
                url: meta.url,
                title: meta.title,
                tree: built.tree,
                stats: built.stats,
            };
            (result, Value::Object(ref_ids))
        };

        // The page keeps the elements behind the refs and lets go of the rest, so a
        // later action reaches its element by ref.
        browser.evaluate(&script_call("setRefs", &ref_ids)).await?;
        Ok(result)
    }

    /// The page-script target for a ref, or the error for one no snapshot handed out.
    fn target(&self, r: &str) -> Result<Value, String> {
        if self.state().refs.contains_key(r) {
            Ok(json!({ "ref": r }))
        } else {
            Err(format!("unknown ref \"{r}\" — call browser_snapshot first"))
        }
    }

    /// Page-space centre of a ref, after scrolling it into view. Shared by every
    /// gesture so they inherit the frame-offset handling rather than re-deriving
    /// coordinates that would be wrong inside an iframe.
    pub async fn point_for_ref(&self, browser: &dyn Browser, r: &str) -> Result<RefPoint, String> {
        let target = self.target(r)?;
        script_result(run_action(browser, target, "point", Value::Null).await?)
    }

    /// Run a small action on a ref inside the page and return what it produced.
    pub async fn act_on_ref(
        &self,
        browser: &dyn Browser,
        r: &str,
        action: RefAction,
    ) -> Result<Value, String> {
        let target = self.target(r)?;
        let (name, value) = match action {
            RefAction::Focus => ("focus", Value::Null),
            RefAction::SetValue(text) => ("setValue", text.into()),
            RefAction::Text => ("text", Value::Null),
            RefAction::Select(values) => ("select", values.into()),
        };
        run_action(browser, target, name, value).await
    }

    /// Give a ref keyboard focus without clicking it.
    ///
    /// Clicking to focus is not always equivalent: a click on a dialog's backdrop
    /// dismisses it, and a click on a listbox item selects the wrong row before the
    /// arrow keys ever run.
    pub async fn focus_ref(&self, browser: &dyn Browser, r: &str) -> Result<(), String> {
        self.act_on_ref(browser, r, RefAction::Focus).await?;
        Ok(())
    }

    /// Choose option(s) in a `<select>`.
    ///
    /// A native dropdown has no DOM to click: the option list is drawn by the OS,
    /// so the click and type paths cannot reach it. Setting `selected` and firing
    /// input/change is what Playwright does too, and it is the only way a form
    /// gated on a select can be completed — a real one blocked a card payment on
    /// its mandatory installment field, with no tool able to answer it.
    ///
    /// Returns the labels actually chosen.
    pub async fn select_ref(
        &self,
        browser: &dyn Browser,
        r: &str,
        values: &[String],
    ) -> Result<Vec<String>, String> {
        let picked = self
            .act_on_ref(browser, r, RefAction::Select(values.to_vec()))
            .await?;
        script_result(picked)
    }

    /// Scroll a specific container rather than the window, and return its new
    /// scroll position.
    ///
    /// `browser_scroll` moves the page, which does nothing for a list that scrolls
    /// inside its own box — the outline could show one and the agent had no way to
    /// advance it. Stepped rather than jumped, so lazy-loading handlers fire the way
    /// they would for a person.
    pub async fn scroll_ref_by(
        &self,
        browser: &dyn Browser,
        r: &str,
        delta_y: f64,
    ) -> Result<f64, String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ScrollState {
            can_scroll: bool,
            scroll_top: f64,
        }
        let target = self.target(r)?;
        let scroll_state = async || -> Result<ScrollState, String> {
            script_result(run_action(browser, target.clone(), "scrollState", Value::Null).await?)
        };
        // A distance without an end would step forever.
        if !delta_y.is_finite() {
            return Err("scroll distance must be a finite number of pixels".to_owned());
        }
        if !scroll_state().await?.can_scroll {
            return Err("element does not scroll vertically".to_owned());
        }

        // Real wheel input over the element, not `scrollTop = n`.
        //
        // Assigning scrollTop moves the box but fires NO scroll event here: the
        // event is dispatched on a frame boundary, and an agent-driven window
        // produces none. Infinite scroll and lazy loading hang off that event, so
        // the container moved to its end and nothing ever appended — the position
        // was right and the page had not reacted at all.
        let point = self.point_for_ref(browser, r).await?;
        let sign = if delta_y >= 0.0 { 1.0 } else { -1.0 };
        let mut left = delta_y.abs();
        while left > 0.0 {
            let step = left.min(120.0);
            let wheel = Input::Wheel {
                x: point.x,
                y: point.y,
                dx: 0.0,
                dy: sign * step,
            };
            browser.input(wheel).await?;
            left -= step;
            sleep(WHEEL_STEP_PAUSE).await;
        }

        Ok(scroll_state().await?.scroll_top)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::task::Wake;

    use rever_tools_api::tool::BoxFuture;

    use super::*;

    /// Run a future to completion on this thread; a pending poll parks until woken.
    fn block_on<T>(future: impl Future<Output = T>) -> T {
        struct Unpark(std::thread::Thread);
        impl Wake for Unpark {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut Context::from_waker(&waker)) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::park(),
            }
        }
    }

    /// A browser that answers the page script from recorded payloads.
    #[derive(Default)]
    struct FakeBrowser {
        captures: Mutex<VecDeque<Value>>,
        actions: Mutex<VecDeque<Value>>,
        /// Every script call as `(command, argument)` source text.
        calls: Mutex<Vec<String>>,
        inputs: Mutex<Vec<Input>>,
    }

    impl FakeBrowser {
        fn with_captures(captures: impl IntoIterator<Item = Value>) -> Self {
            Self {
                captures: Mutex::new(captures.into_iter().collect()),
                ..Self::default()
            }
        }

        fn with_actions(self, actions: impl IntoIterator<Item = Value>) -> Self {
            *self.actions.lock().unwrap() = actions.into_iter().collect();
            self
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Browser for FakeBrowser {
        fn evaluate<'a>(&'a self, expression: &'a str) -> BoxFuture<'a, Result<Value, String>> {
            Box::pin(async move {
                let call = expression
                    .strip_prefix(&format!("({PAGE_SCRIPT})"))
                    .ok_or("the expression is not a page script call")?;
                self.calls.lock().unwrap().push(call.to_owned());
                let recorded = if call.starts_with("(\"capture\"") {
                    self.captures.lock().unwrap().pop_front()
                } else if call.starts_with("(\"setRefs\"") {
                    Some(json!(0))
                } else {
                    self.actions.lock().unwrap().pop_front()
                };
                recorded.ok_or_else(|| format!("no answer recorded for {call}"))
            })
        }

        fn input(&self, input: Input) -> BoxFuture<'_, Result<(), String>> {
            self.inputs.lock().unwrap().push(input);
            Box::pin(async { Ok(()) })
        }
    }

    fn snapshotter() -> Snapshotter {
        Snapshotter {
            settle_delay: Duration::ZERO,
            ..Snapshotter::default()
        }
    }

    fn snap(snapshotter: &Snapshotter, browser: &FakeBrowser, full: bool) -> SnapshotResult {
        block_on(snapshotter.take_snapshot(browser, SnapshotOptions { full })).unwrap()
    }

    /// One capture of a single page.
    fn snap_once(page: &Page) -> SnapshotResult {
        let browser = FakeBrowser::with_captures([page.payload()]);
        snap(&snapshotter(), &browser, false)
    }

    /// What `r` points at after one capture of `page`.
    fn snap_entry(page: &Page, r: &str) -> RefEntry {
        let browser = FakeBrowser::with_captures([page.payload()]);
        let snapshotter = snapshotter();
        snap(&snapshotter, &browser, false);
        snapshotter.ref_entry(r).unwrap()
    }

    // Style strings of the recorded pages, by index.
    const STRINGS: [&str; 8] = [
        "visible",
        "hidden",
        "1",
        "0",
        "auto",
        "fixed",
        "rgb(20, 20, 20)",
        "clip",
    ];
    const S_HIDDEN: i64 = 1;
    const S_AUTO: i64 = 4;
    const S_FIXED: i64 = 5;
    const S_OPAQUE: i64 = 6;

    /// A recorded capture, built the way the page script reports one: node 1 is the
    /// document (RootWebArea), node 2 is `<html>` at path '' filling a 1000x3000 page,
    /// and the viewport is 1000x800 at the top.
    struct Page {
        url: String,
        /// Tree 0 is the page; the others are frames as (owner, url, nodes).
        trees: Vec<(u64, String, Vec<Value>)>,
        /// Which tree a node lives in.
        home: HashMap<u64, usize>,
        dom_ids: Vec<u64>,
        dom_parents: Vec<i64>,
        dom_index: HashMap<u64, usize>,
        rows: Vec<Row>,
        row_of: HashMap<u64, usize>,
        click_paths: Vec<&'static str>,
        click_skipped: bool,
        scroll_y: f64,
    }

    /// One layout row: the box of a node, its style strings and its scroll rects.
    struct Row {
        node_index: usize,
        bounds: [f64; 4],
        styles: [i64; 6],
        scroll_rect: Value,
        client_rect: Value,
    }

    impl Page {
        fn new() -> Self {
            let root = json!({
                "nodeId": "1", "backendDOMNodeId": 1,
                "role": { "value": "RootWebArea" }, "name": { "value": "Test page" },
                "childIds": []
            });
            let mut page = Page {
                url: "https://shop.test/".to_owned(),
                trees: vec![(0, String::new(), vec![root])],
                home: HashMap::from([(1, 0)]),
                dom_ids: vec![1],
                dom_parents: vec![-1],
                dom_index: HashMap::from([(1, 0)]),
                rows: Vec::new(),
                row_of: HashMap::new(),
                click_paths: Vec::new(),
                click_skipped: false,
                scroll_y: 0.0,
            };
            page.el(1, 2, "generic", "", [0.0, 0.0, 1000.0, 3000.0]);
            page
        }

        /// An element with a box and no accessibility node (aria-hidden, canvas internals).
        fn boxed(&mut self, parent: u64, id: u64, bounds: [f64; 4]) {
            self.dom_index.insert(id, self.dom_ids.len());
            self.dom_ids.push(id);
            self.dom_parents.push(self.dom_index[&parent] as i64);
            self.row_of.insert(id, self.rows.len());
            self.rows.push(Row {
                node_index: self.dom_index[&id],
                bounds,
                styles: [0, 2, -1, -1, -1, -1],
                scroll_rect: json!([]),
                client_rect: json!([]),
            });
        }

        /// An element: a box plus an accessibility node under `parent`'s.
        fn el(
            &mut self,
            parent: u64,
            id: u64,
            role: &str,
            name: &str,
            bounds: [f64; 4],
        ) -> &mut Value {
            self.boxed(parent, id, bounds);
            let mut node = json!({
                "nodeId": id.to_string(), "backendDOMNodeId": id,
                "role": { "value": role }, "childIds": []
            });
            if !name.is_empty() {
                node["name"] = json!({ "value": name });
            }
            self.attach(parent, id.to_string(), node)
        }

        fn text(&mut self, parent: u64, text: &str) {
            let id = format!("t{}", self.trees.iter().map(|t| t.2.len()).sum::<usize>());
            let node = json!({
                "nodeId": id, "role": { "value": "StaticText" }, "name": { "value": text }
            });
            self.attach(parent, id, node);
        }

        fn attach(&mut self, parent: u64, id: String, node: Value) -> &mut Value {
            let tree = self.home[&parent];
            if let Ok(id) = id.parse() {
                self.home.insert(id, tree);
            }
            let nodes = &mut self.trees[tree].2;
            let parent_id = parent.to_string();
            let parent_node = nodes.iter_mut().find(|n| n["nodeId"] == parent_id).unwrap();
            parent_node["childIds"]
                .as_array_mut()
                .unwrap()
                .push(json!(id));
            nodes.push(node);
            nodes.last_mut().unwrap()
        }

        /// The document of a same-origin frame, read through the `<iframe>` `owner`.
        fn frame(&mut self, owner: u64, root: u64, title: &str, url: &str) {
            let node = json!({
                "nodeId": root.to_string(), "backendDOMNodeId": root,
                "role": { "value": "RootWebArea" }, "name": { "value": title },
                "childIds": []
            });
            self.home.insert(root, self.trees.len());
            self.trees.push((owner, url.to_owned(), vec![node]));
            // The frame's elements hang under the <iframe> in the node table.
            self.dom_index.insert(root, self.dom_index[&owner]);
        }

        fn style(&mut self, id: u64, slot: usize, string: i64) {
            self.rows[self.row_of[&id]].styles[slot] = string;
        }

        /// Make `id` an overflow:auto box with this much content and room.
        fn scrolls(&mut self, id: u64, scroll_height: f64, client_height: f64) {
            self.style(id, 3, S_AUTO);
            let row = &mut self.rows[self.row_of[&id]];
            row.scroll_rect = json!([0, 0, 100, scroll_height]);
            row.client_rect = json!([0, 0, 100, client_height]);
        }

        fn payload(&self) -> Value {
            let column = |f: &dyn Fn(usize) -> Value| -> Vec<Value> {
                (0..self.rows.len()).map(f).collect()
            };
            let frames: Vec<Value> = self.trees[1..]
                .iter()
                .map(|(owner, url, nodes)| {
                    json!({
                        "frameId": format!("f{owner}"), "ownerBackendNodeId": owner,
                        "url": url, "nodes": nodes
                    })
                })
                .collect();
            json!({
                "meta": {
                    "url": self.url, "title": "Test page",
                    "scrollX": 0, "scrollY": self.scroll_y, "innerWidth": 1000, "innerHeight": 800
                },
                "nodes": self.trees[0].2,
                "frames": frames,
                "layout": {
                    "strings": STRINGS,
                    "documents": [{
                        "nodes": {
                            "backendNodeId": self.dom_ids,
                            "parentIndex": self.dom_parents,
                            "nodeType": self.dom_ids.iter().map(|&id| if id == 1 { 9 } else { 1 }).collect::<Vec<_>>()
                        },
                        "layout": {
                            "nodeIndex": column(&|row| json!(self.rows[row].node_index)),
                            "bounds": column(&|row| json!(self.rows[row].bounds)),
                            "styles": column(&|row| json!(self.rows[row].styles)),
                            "paintOrders": column(&|row| json!(row)),
                            "scrollRects": column(&|row| self.rows[row].scroll_rect.clone()),
                            "clientRects": column(&|row| self.rows[row].client_rect.clone())
                        }
                    }]
                },
                "click": { "paths": self.click_paths, "scanned": self.dom_ids.len(), "skipped": self.click_skipped }
            })
        }
    }

    fn lines(text: &[&str]) -> String {
        text.join("\n")
    }

    // --- outline ---

    #[test]
    fn 역할_이름_값_상태를_한_줄씩_쓰고_조작_가능한_노드에_ref를_붙인다() {
        let mut page = Page::new();
        page.el(2, 3, "heading", "Shop", [0.0, 0.0, 500.0, 40.0])["properties"] =
            json!([{ "name": "level", "value": { "value": 1 } }]);
        page.text(3, "Shop");
        page.el(2, 4, "navigation", "Main", [0.0, 40.0, 1000.0, 40.0]);
        page.el(4, 5, "link", "Home", [0.0, 40.0, 100.0, 40.0]);
        page.text(5, "Home");
        page.el(2, 6, "button", "Buy now", [0.0, 100.0, 100.0, 40.0]);
        page.el(6, 7, "generic", "", [0.0, 100.0, 100.0, 40.0]);
        page.text(7, "Buy now");
        page.el(2, 8, "textbox", "Search", [0.0, 150.0, 200.0, 30.0])["value"] =
            json!({ "value": "shoes" });
        page.el(2, 9, "checkbox", "Accept", [0.0, 200.0, 20.0, 20.0])["properties"] =
            json!([{ "name": "checked", "value": { "value": "false" } }]);
        page.el(2, 10, "paragraph", "", [0.0, 240.0, 500.0, 40.0]);
        page.text(10, "  Free shipping over $50 ");
        page.el(2, 11, "button", "Pay", [0.0, 300.0, 100.0, 40.0])["properties"] = json!([
            { "name": "disabled", "value": { "value": true } },
            { "name": "expanded", "value": { "value": false } }
        ]);
        page.el(2, 12, "option", "Large", [0.0, 350.0, 100.0, 20.0])["properties"] =
            json!([{ "name": "selected", "value": { "value": true } }]);

        let result = snap_once(&page);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - heading \"Shop\" level=1",
                "  - navigation \"Main\"",
                "    - link \"Home\" [ref=r1]",
                "  - button \"Buy now\" [ref=r2]",
                "  - textbox \"Search\" value=\"shoes\" [ref=r3]",
                "  - checkbox \"Accept\" checked=false [ref=r4]",
                "  - paragraph",
                "    - text \"Free shipping over $50\"",
                "  - button \"Pay\" disabled [ref=r5]",
                "  - option \"Large\" selected=true [ref=r6]",
            ])
        );
        assert_eq!(result.url, "https://shop.test/");
        assert_eq!(result.title, "Test page");
        assert_eq!(
            result.stats,
            SnapshotStats {
                refs: 6,
                click_scanned: 0,
                ..SnapshotStats::default()
            }
        );
    }

    #[test]
    fn 무시된_노드와_이름_없는_generic은_줄을_쓰지_않고_자식만_올린다() {
        let mut page = Page::new();
        page.el(2, 3, "button", "Ghost", [0.0, 0.0, 100.0, 40.0])["ignored"] = json!(true);
        page.el(3, 4, "button", "Inner", [0.0, 0.0, 50.0, 40.0]);
        page.el(2, 5, "generic", "Named group", [0.0, 50.0, 100.0, 40.0]);
        page.el(2, 6, "", "No role", [0.0, 100.0, 100.0, 40.0]);
        page.text(6, "plain");

        let result = snap_once(&page);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - button \"Inner\" [ref=r1]",
                "  - generic \"Named group\"",
                "  - text \"plain\"",
            ])
        );
    }

    #[test]
    fn 뷰포트_밖과_숨겨진_노드는_빼고_스크롤_힌트로_요약한다() {
        let mut page = Page::new();
        page.el(2, 3, "button", "Top", [0.0, 10.0, 100.0, 40.0]);
        page.el(2, 4, "button", "Below", [0.0, 1000.0, 100.0, 40.0]);
        page.el(2, 5, "button", "Far below", [0.0, 2500.0, 100.0, 40.0]);
        page.el(2, 6, "button", "Side", [1500.0, 100.0, 100.0, 40.0]);
        page.el(2, 7, "button", "Invisible", [0.0, 200.0, 100.0, 40.0]);
        page.style(7, 0, S_HIDDEN);
        // An off-screen container takes its subtree with it; the button is still counted.
        page.el(2, 8, "list", "", [0.0, 1200.0, 500.0, 300.0]);
        page.el(8, 9, "button", "Buried", [0.0, 1210.0, 100.0, 40.0]);

        let result = snap_once(&page);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - button \"Top\" [ref=r1]",
                "- [3 more actionable element(s) below the fold — nearest ~200px down]",
                "- [1 more actionable element(s) outside the horizontal viewport]",
            ])
        );
        assert_eq!(result.stats.refs, 1);
        assert_eq!(result.stats.hidden, 1);
        assert_eq!(result.stats.offscreen, 4);
    }

    #[test]
    fn full이면_걸러내지_않고_전부_쓴다() {
        let mut page = Page::new();
        page.el(2, 3, "button", "Top", [0.0, 10.0, 100.0, 40.0]);
        page.el(2, 4, "button", "Below", [0.0, 1000.0, 100.0, 40.0]);
        page.el(2, 5, "button", "Invisible", [0.0, 200.0, 100.0, 40.0]);
        page.style(5, 0, S_HIDDEN);
        let browser = FakeBrowser::with_captures([page.payload()]);

        let result = snap(&snapshotter(), &browser, true);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - button \"Top\" [ref=r1]",
                "  - button \"Below\" [ref=r2]",
                "  - button \"Invisible\" [ref=r3]",
            ])
        );
        assert_eq!((result.stats.hidden, result.stats.offscreen), (0, 0));
    }

    #[test]
    fn 크기가_0인_래퍼는_자식을_데리고_사라지지_않는다() {
        let mut page = Page::new();
        page.el(2, 3, "navigation", "Menu", [0.0, 0.0, 1000.0, 0.0]);
        page.el(3, 4, "link", "Docs", [0.0, 0.0, 80.0, 30.0]);

        let result = snap_once(&page);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - navigation \"Menu\"",
                "    - link \"Docs\" [ref=r1]",
            ])
        );
    }

    #[test]
    fn 오버레이에_덮인_컨트롤은_빠지고_숨김으로_센다() {
        let mut page = Page::new();
        page.el(2, 3, "button", "Behind", [100.0, 100.0, 100.0, 40.0]);
        page.el(2, 4, "dialog", "Sign in", [0.0, 0.0, 1000.0, 800.0]);
        page.style(4, 4, S_FIXED);
        page.style(4, 5, S_OPAQUE);
        page.el(4, 5, "button", "Continue", [400.0, 400.0, 100.0, 40.0]);

        let result = snap_once(&page);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - dialog \"Sign in\"",
                "    - button \"Continue\" [ref=r1]",
            ])
        );
        assert_eq!(result.stats.hidden, 1);
        assert!(!result.stats.fell_back_to_full);
    }

    #[test]
    fn 필터가_이유_없이_전부_지우면_필터_없이_다시_만든다() {
        // The container reads as hidden, so the walk never reaches the button, while
        // the button's own box is fine and the tally has nothing to report.
        let mut page = Page::new();
        page.el(2, 3, "main", "", [0.0, 0.0, 1000.0, 800.0]);
        page.style(3, 0, S_HIDDEN);
        page.el(
            3,
            4,
            "button",
            "Visible after all",
            [0.0, 10.0, 100.0, 40.0],
        );

        let result = snap_once(&page);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - main",
                "    - button \"Visible after all\" [ref=r1]",
            ])
        );
        assert!(result.stats.fell_back_to_full);
    }

    #[test]
    fn 컨트롤이_아예_없는_페이지는_좌표_불일치로_보고하지_않는다() {
        let mut page = Page::new();
        page.el(2, 3, "heading", "Not found", [0.0, 0.0, 300.0, 40.0]);

        let result = snap_once(&page);

        assert_eq!(result.stats.refs, 0);
        assert!(!result.stats.fell_back_to_full);
    }

    #[test]
    fn 레이아웃이_없으면_걸러내지_않는다() {
        let mut page = Page::new();
        page.el(2, 3, "button", "Anywhere", [0.0, 5000.0, 100.0, 40.0]);
        let mut payload = page.payload();
        payload["layout"] = Value::Null;
        let browser = FakeBrowser::with_captures([payload]);

        let result = snap(&snapshotter(), &browser, false);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - button \"Anywhere\" [ref=r1]"
            ])
        );
        assert_eq!(result.stats.offscreen, 0);
    }

    // --- click scan ---

    #[test]
    fn 역할_없는_클릭_대상은_clickable로_ref를_받고_안쪽_글자로_이름을_단다() {
        let mut page = Page::new();
        page.el(2, 3, "generic", "", [0.0, 0.0, 300.0, 100.0]);
        page.el(3, 4, "generic", "", [0.0, 0.0, 300.0, 50.0]);
        page.text(4, "Card title");
        page.click_paths = vec!["0"];

        let result = snap_once(&page);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - clickable \"Card title\" [ref=r1] (click-scan)",
                "    - text \"Card title\"",
            ])
        );
        assert_eq!(result.stats.click_only_refs, 1);
        assert_eq!(result.stats.click_scanned, 1);
        assert_eq!(result.stats.click_matched, 1);
        let entry = snap_entry(&page, "r1");
        assert_eq!(
            (entry.role.as_str(), entry.name.as_str()),
            ("generic", "Card title")
        );
    }

    #[test]
    fn 클릭_신호가_있어도_역할이_있는_노드는_자기_역할을_쓴다() {
        let mut page = Page::new();
        page.el(2, 3, "button", "Save", [0.0, 0.0, 100.0, 40.0]);
        page.el(2, 4, "image", "Product photo", [0.0, 50.0, 100.0, 100.0]);
        page.click_paths = vec!["0", "1"];

        let result = snap_once(&page);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - button \"Save\" [ref=r1]",
                "  - image \"Product photo\" [ref=r2] (click-scan)",
            ])
        );
        assert_eq!(result.stats.click_only_refs, 1);
    }

    #[test]
    fn 접근성_트리에_없는_클릭_대상은_끝에_따로_쓴다() {
        let mut page = Page::new();
        page.el(2, 3, "button", "Save", [0.0, 0.0, 100.0, 40.0]);
        page.boxed(2, 4, [0.0, 100.0, 400.0, 300.0]); // a canvas hidden from the tree
        page.boxed(2, 5, [0.0, 500.0, 4.0, 4.0]); // too small to be a target
        page.boxed(2, 6, [0.0, 2000.0, 400.0, 300.0]); // off-screen
        page.click_paths = vec!["1", "2", "3"];

        let result = snap_once(&page);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - button \"Save\" [ref=r1]",
                "- clickable [ref=r2] (click-scan, not in the a11y tree)",
            ])
        );
        assert_eq!(result.stats.refs, 2);
        assert_eq!(result.stats.click_only_refs, 1);
        assert_eq!(result.stats.click_matched, 3);
    }

    #[test]
    fn 클릭_스캔을_건너뛰었으면_통계에_남긴다() {
        let mut page = Page::new();
        page.el(2, 3, "button", "Save", [0.0, 0.0, 100.0, 40.0]);
        page.click_skipped = true;
        assert!(snap_once(&page).stats.click_scan_skipped);

        let mut payload = page.payload();
        payload["click"] = Value::Null;
        let browser = FakeBrowser::with_captures([payload]);
        assert!(
            snap(&snapshotter(), &browser, false)
                .stats
                .click_scan_skipped
        );
    }

    // --- scroll containers ---

    #[test]
    fn 스크롤_컨테이너는_역할이_없어도_ref와_남은_픽셀을_받는다() {
        let mut page = Page::new();
        page.el(2, 3, "generic", "", [0.0, 0.0, 300.0, 100.0]);
        page.scrolls(3, 400.0, 100.0);
        page.el(2, 4, "list", "Orders", [0.0, 100.0, 300.0, 100.0]);
        page.scrolls(4, 232.0, 100.0);
        page.el(2, 5, "", "", [0.0, 200.0, 300.0, 100.0]);
        page.scrolls(5, 150.0, 100.0);
        // Overflow below the noise threshold is not a scroller.
        page.el(2, 6, "generic", "", [0.0, 300.0, 300.0, 100.0]);
        page.scrolls(6, 110.0, 100.0);

        let result = snap_once(&page);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - generic [ref=r1] [scrollable +300px]",
                "  - list \"Orders\" [ref=r2] [scrollable +132px]",
                "  - scroller [ref=r3] [scrollable +50px]",
            ])
        );
        assert_eq!(snap_once(&page).stats.click_only_refs, 0);
    }

    // --- frames ---

    #[test]
    fn 같은_출처_프레임은_iframe_아래에_이어_붙이고_못_들어간_프레임은_센다() {
        let mut page = Page::new();
        page.el(2, 3, "Iframe", "Pay", [0.0, 0.0, 400.0, 300.0]);
        page.frame(3, 20, "Pay doc", "https://shop.test/pay");
        page.el(20, 21, "generic", "", [0.0, 0.0, 400.0, 300.0]);
        page.el(21, 22, "generic", "", [0.0, 0.0, 400.0, 300.0]);
        page.el(22, 23, "button", "Pay now", [10.0, 10.0, 100.0, 40.0]);
        // Cross-origin: the script could not read it, so no frame was reported.
        page.el(2, 4, "Iframe", "Ads", [0.0, 300.0, 400.0, 100.0]);
        // aria-hidden <iframe>: a frame with no node to hang under.
        page.boxed(2, 5, [0.0, 400.0, 400.0, 100.0]);
        page.frame(5, 30, "Tracker", "https://shop.test/pixel");
        page.el(30, 31, "generic", "", [0.0, 400.0, 400.0, 100.0]);
        page.el(31, 32, "generic", "", [0.0, 400.0, 400.0, 100.0]);
        page.el(32, 33, "button", "Allow", [10.0, 410.0, 100.0, 40.0]);

        let result = snap_once(&page);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - Iframe \"Pay\"",
                "    - RootWebArea \"Pay doc\"",
                "      - button \"Pay now\" [ref=r1]",
                "  - Iframe \"Ads\"",
                "- Iframe (hidden from the a11y tree) \"https://shop.test/pixel\"",
                "  - RootWebArea \"Tracker\"",
                "    - button \"Allow\" [ref=r2]",
            ])
        );
        assert_eq!(result.stats.frames, 2);
        assert_eq!(result.stats.frames_unreachable, 1);
        assert_eq!(result.stats.frames_empty, 0);
    }

    #[test]
    fn 읽지도_못하고_줄도_없는_프레임은_끝에_적고_못_들어간_것으로_센다() {
        let mut page = Page::new();
        page.el(2, 3, "button", "Save", [0.0, 0.0, 100.0, 40.0]);
        let mut payload = page.payload();
        payload["unlistedFrames"] = json!([
            { "frameId": "f8", "url": "https://ads.test/beacon" },
            { "frameId": "f9", "url": "" }
        ]);
        let browser = FakeBrowser::with_captures([payload]);

        let result = snap(&snapshotter(), &browser, false);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - button \"Save\" [ref=r1]",
                "- Iframe (hidden from the a11y tree) \"https://ads.test/beacon\"",
                "- Iframe (hidden from the a11y tree) \"f9\"",
            ])
        );
        assert_eq!(result.stats.frames, 0);
        assert_eq!(result.stats.frames_unreachable, 2);
    }

    #[test]
    fn 루트뿐인_프레임은_빈_프레임으로_세고_주소가_없으면_id로_부른다() {
        let mut page = Page::new();
        page.boxed(2, 5, [0.0, 0.0, 400.0, 100.0]);
        page.frame(5, 30, "", "");

        let result = snap_once(&page);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "- Iframe (hidden from the a11y tree) \"f5\"",
                "  - RootWebArea",
            ])
        );
        assert_eq!(result.stats.frames, 1);
        assert_eq!(result.stats.frames_empty, 1);
    }

    // --- *new ---

    fn buttons(names: &[(u64, &str)]) -> Page {
        let mut page = Page::new();
        for (row, &(id, name)) in names.iter().enumerate() {
            page.el(2, id, "button", name, [0.0, 50.0 * row as f64, 100.0, 40.0]);
        }
        page
    }

    #[test]
    fn 같은_문서의_다음_스냅샷은_새로_나타난_ref에_new를_붙인다() {
        let before = buttons(&[(10, "Add"), (11, "Remove")]);
        let after = buttons(&[(10, "Add"), (12, "Undo"), (11, "Remove")]);
        let browser = FakeBrowser::with_captures([before.payload(), after.payload()]);
        let snapshotter = snapshotter();

        let first = snap(&snapshotter, &browser, false);
        let second = snap(&snapshotter, &browser, false);

        assert_eq!(first.stats.new_refs, 0);
        assert!(!first.tree.contains("*new"));
        assert_eq!(
            second.tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - button \"Add\" [ref=r1]",
                "  - button \"Undo\" *new [ref=r2]",
                "  - button \"Remove\" [ref=r3]",
            ])
        );
        assert_eq!(second.stats.new_refs, 1);
    }

    #[test]
    fn 주소가_바뀌면_new를_붙이지_않는다() {
        let before = buttons(&[(10, "Add")]);
        let mut after = buttons(&[(10, "Add"), (12, "Undo")]);
        after.url = "https://shop.test/cart".to_owned();
        let browser = FakeBrowser::with_captures([before.payload(), after.payload()]);
        let snapshotter = snapshotter();

        snap(&snapshotter, &browser, false);
        let second = snap(&snapshotter, &browser, false);

        assert_eq!(second.stats.new_refs, 0);
    }

    #[test]
    fn 걸러낸_스냅샷과_전체_스냅샷은_서로_비교하지_않는다() {
        let page = buttons(&[(10, "Add"), (11, "Remove")]);
        let mut more = buttons(&[(10, "Add"), (11, "Remove")]);
        more.el(2, 12, "button", "Far below", [0.0, 2000.0, 100.0, 40.0]);
        let browser = FakeBrowser::with_captures([page.payload(), more.payload(), more.payload()]);
        let snapshotter = snapshotter();

        snap(&snapshotter, &browser, false);
        let full = snap(&snapshotter, &browser, true);
        let full_again = snap(&snapshotter, &browser, true);

        assert_eq!(full.stats.refs, 3);
        assert_eq!(full.stats.new_refs, 0);
        // Same mode twice: nothing appeared in between.
        assert_eq!(full_again.stats.new_refs, 0);
    }

    // --- settle retry ---

    #[test]
    fn 컨트롤이_전부_화면_밖으로_읽히면_기다렸다가_다시_읽는다() {
        let unsettled = buttons(&[(10, "Add")]);
        let mut unsettled_payload = unsettled.payload();
        unsettled_payload["layout"]["documents"][0]["layout"]["bounds"][1] =
            json!([0, 4000, 100, 40]);
        let settled = buttons(&[(10, "Add")]);
        let browser = FakeBrowser::with_captures([unsettled_payload, settled.payload()]);

        let result = snap(&snapshotter(), &browser, false);

        assert_eq!(result.stats.refs, 1);
        let captures = browser
            .calls()
            .iter()
            .filter(|call| call.starts_with("(\"capture\""))
            .count();
        assert_eq!(captures, 2);
    }

    #[test]
    fn 다시_읽어도_ref가_없으면_첫_결과를_돌려준다() {
        let mut page = Page::new();
        page.el(2, 3, "button", "Below", [0.0, 1000.0, 100.0, 40.0]);
        let mut later = Page::new();
        later.el(2, 3, "button", "Below", [0.0, 1000.0, 100.0, 40.0]);
        later.el(2, 4, "button", "Also below", [0.0, 1100.0, 100.0, 40.0]);
        let browser = FakeBrowser::with_captures([page.payload(), later.payload()]);

        let result = snap(&snapshotter(), &browser, false);

        assert_eq!(result.stats.offscreen, 1);
    }

    #[test]
    fn full_스냅샷은_다시_읽지_않는다() {
        let mut page = Page::new();
        page.el(
            2,
            3,
            "heading",
            "Nothing to act on",
            [0.0, 0.0, 300.0, 40.0],
        );
        let browser = FakeBrowser::with_captures([page.payload()]);

        snap(&snapshotter(), &browser, true);

        assert_eq!(browser.calls().len(), 2); // capture + setRefs
    }

    // --- page script calls ---

    #[test]
    fn 캡처에_한도를_넘기고_ref가_가리키는_요소를_페이지에_등록한다() {
        let page = buttons(&[(10, "Add"), (11, "Remove")]);
        let browser = FakeBrowser::with_captures([page.payload()]);
        let snapshotter = snapshotter();

        snap(&snapshotter, &browser, false);

        let calls = browser.calls();
        assert_eq!(
            calls[0],
            "(\"capture\", {\"maxShadowDepth\":5,\"maxScannedElements\":10000,\"maxFrameDepth\":5,\"maxFrames\":24})"
        );
        let registered: Value = serde_json::from_str(
            calls[1]
                .strip_prefix("(\"setRefs\", ")
                .and_then(|rest| rest.strip_suffix(')'))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(registered, json!({ "r1": 10, "r2": 11 }));

        assert_eq!(
            snapshotter.ref_for_backend_node_id(11).as_deref(),
            Some("r2")
        );
        assert_eq!(snapshotter.ref_for_backend_node_id(99), None);
        let entry = snapshotter.ref_entry("r1").unwrap();
        assert_eq!(
            (entry.role.as_str(), entry.name.as_str()),
            ("button", "Add")
        );
    }

    #[test]
    fn 새_스냅샷은_이전_ref를_버린다() {
        let browser = FakeBrowser::with_captures([
            buttons(&[(10, "Add"), (11, "Remove")]).payload(),
            buttons(&[(11, "Remove")]).payload(),
        ]);
        let snapshotter = snapshotter();

        snap(&snapshotter, &browser, false);
        snap(&snapshotter, &browser, false);

        assert_eq!(snapshotter.ref_entry("r1").unwrap().name, "Remove");
        assert!(snapshotter.ref_entry("r2").is_none());
    }

    #[test]
    fn 스크립트가_json_문자열로_답해도_읽는다() {
        let page = buttons(&[(10, "Add")]);
        let browser = FakeBrowser::with_captures([Value::String(page.payload().to_string())]);

        assert_eq!(snap(&snapshotter(), &browser, false).stats.refs, 1);
    }

    #[test]
    fn 읽을_수_없는_응답은_패닉_없이_오류로_돌려준다() {
        for bad in [
            json!("not json"),
            json!(42),
            json!({ "nodes": "nope" }),
            json!({ "nodes": [{ "nodeId": 7 }] }),
            json!({ "layout": { "documents": [{ "nodes": { "backendNodeId": [-1] } }] } }),
        ] {
            let browser = FakeBrowser::with_captures([bad]);
            let error = block_on(snapshotter().take_snapshot(&browser, SnapshotOptions::default()))
                .unwrap_err();
            assert!(
                error.starts_with("the page script returned something unreadable"),
                "{error}"
            );
        }

        // An empty answer is an empty page, not an error.
        let browser = FakeBrowser::with_captures([json!({})]);
        let empty = snap(&snapshotter(), &browser, false);
        assert_eq!(empty.tree, "");
        assert_eq!(empty.stats.refs, 0);
    }

    #[test]
    fn 브라우저_오류는_그대로_전달한다() {
        let browser = FakeBrowser::default();
        let error = block_on(snapshotter().take_snapshot(&browser, SnapshotOptions::default()))
            .unwrap_err();
        assert!(error.starts_with("no answer recorded"), "{error}");
    }

    #[test]
    fn 자식_목록이_순환하거나_지나치게_깊어도_끝난다() {
        let looped = json!({
            "nodes": [
                { "nodeId": "1", "role": { "value": "RootWebArea" }, "childIds": ["2", "2", "9"] },
                { "nodeId": "2", "backendDOMNodeId": 2, "role": { "value": "button" }, "name": { "value": "Loop" }, "childIds": ["1", "2"] }
            ]
        });
        let browser = FakeBrowser::with_captures([looped]);
        assert_eq!(
            snap(&snapshotter(), &browser, false).tree,
            lines(&["- RootWebArea", "  - button \"Loop\" [ref=r1]"])
        );

        let deep: Vec<Value> = (1..=5000)
            .map(|i| {
                json!({
                    "nodeId": i.to_string(), "role": { "value": "group" },
                    "childIds": [(i + 1).to_string()]
                })
            })
            .collect();
        let browser = FakeBrowser::with_captures([json!({ "nodes": deep })]);
        let tree = snap(&snapshotter(), &browser, false).tree;
        assert_eq!(tree.lines().count(), MAX_WALK_DEPTH + 1);
    }

    // --- actions ---

    fn ready() -> (Snapshotter, FakeBrowser) {
        let page = buttons(&[(10, "Add"), (11, "Remove")]);
        let browser = FakeBrowser::with_captures([page.payload()]);
        let snapshotter = snapshotter();
        snap(&snapshotter, &browser, false);
        (snapshotter, browser)
    }

    #[test]
    fn ref의_중심점을_페이지에_물어_돌려준다() {
        let (snapshotter, browser) = ready();
        let browser = browser.with_actions([json!(
            "{\"ok\":{\"x\":50,\"y\":20.5,\"left\":0,\"top\":0.5,\"width\":100,\"height\":40}}"
        )]);

        let point = block_on(snapshotter.point_for_ref(&browser, "r2")).unwrap();

        assert_eq!((point.x, point.y), (50.0, 20.5));
        assert_eq!(
            (point.left, point.top, point.width, point.height),
            (0.0, 0.5, 100.0, 40.0)
        );
        assert_eq!(
            browser.calls().last().unwrap(),
            "(\"act\", {\"ref\":\"r2\",\"action\":\"point\",\"value\":null})"
        );
    }

    #[test]
    fn 모르는_ref는_페이지에_묻지_않고_오류를_낸다() {
        let (snapshotter, browser) = ready();
        let before = browser.calls().len();

        let error = block_on(snapshotter.point_for_ref(&browser, "r9")).unwrap_err();

        assert_eq!(error, "unknown ref \"r9\" — call browser_snapshot first");
        assert_eq!(browser.calls().len(), before);
        assert!(block_on(snapshotter.focus_ref(&browser, "nope")).is_err());
    }

    #[test]
    fn 페이지에서_사라진_ref는_페이지가_알려준_오류를_돌려준다() {
        let (snapshotter, browser) = ready();
        let gone = "ref \"r1\" is no longer on the page — call browser_snapshot again";
        let browser = browser.with_actions([json!({ "error": gone })]);

        assert_eq!(
            block_on(snapshotter.point_for_ref(&browser, "r1")).unwrap_err(),
            gone
        );
    }

    #[test]
    fn 작은_동작은_이름과_값을_스크립트에_넘기고_결과를_돌려준다() {
        let (snapshotter, browser) = ready();
        let browser = browser.with_actions([
            json!({ "ok": true }),
            json!({ "ok": true }),
            json!({ "ok": "Add to cart" }),
        ]);

        block_on(snapshotter.focus_ref(&browser, "r1")).unwrap();
        let set = RefAction::SetValue("say \"hi\"".to_owned());
        assert_eq!(
            block_on(snapshotter.act_on_ref(&browser, "r1", set)),
            Ok(json!(true))
        );
        let text = block_on(snapshotter.act_on_ref(&browser, "r1", RefAction::Text));
        assert_eq!(text, Ok(json!("Add to cart")));

        let calls = browser.calls();
        assert_eq!(
            calls[calls.len() - 3..],
            [
                "(\"act\", {\"ref\":\"r1\",\"action\":\"focus\",\"value\":null})",
                "(\"act\", {\"ref\":\"r1\",\"action\":\"setValue\",\"value\":\"say \\\"hi\\\"\"})",
                "(\"act\", {\"ref\":\"r1\",\"action\":\"text\",\"value\":null})",
            ]
        );
    }

    #[test]
    fn select는_고른_라벨을_돌려주고_못_고르면_선택지를_오류에_싣는다() {
        let (snapshotter, browser) = ready();
        let browser = browser.with_actions([
            json!({ "ok": ["3 months"] }),
            json!({ "error": "no option matched", "options": ["Pay in full", "3 months"] }),
            json!({ "error": "ref is a <button>, not a <select>" }),
        ]);
        let wanted = ["3".to_owned()];

        assert_eq!(
            block_on(snapshotter.select_ref(&browser, "r1", &wanted)),
            Ok(vec!["3 months".to_owned()])
        );
        assert_eq!(
            block_on(snapshotter.select_ref(&browser, "r1", &wanted)).unwrap_err(),
            "no option matched — options: Pay in full | 3 months"
        );
        assert_eq!(
            block_on(snapshotter.select_ref(&browser, "r1", &wanted)).unwrap_err(),
            "ref is a <button>, not a <select>"
        );
    }

    #[test]
    fn 컨테이너_스크롤은_120픽셀씩_휠_입력으로_나눠_보낸다() {
        let (snapshotter, browser) = ready();
        let browser = browser.with_actions([
            json!({ "ok": { "canScroll": true, "scrollTop": 0 } }),
            json!({ "ok": { "x": 150, "y": 60, "left": 0, "top": 10, "width": 300, "height": 100 } }),
            json!({ "ok": { "canScroll": true, "scrollTop": 300 } }),
        ]);

        let top = block_on(snapshotter.scroll_ref_by(&browser, "r1", -300.0)).unwrap();

        assert_eq!(top, 300.0);
        let wheel = |dy| Input::Wheel {
            x: 150.0,
            y: 60.0,
            dx: 0.0,
            dy,
        };
        assert_eq!(
            *browser.inputs.lock().unwrap(),
            [wheel(-120.0), wheel(-120.0), wheel(-60.0)]
        );
    }

    #[test]
    fn 스크롤되지_않는_요소와_끝없는_거리는_거절한다() {
        let (snapshotter, browser) = ready();
        let browser =
            browser.with_actions([json!({ "ok": { "canScroll": false, "scrollTop": 0 } })]);

        assert_eq!(
            block_on(snapshotter.scroll_ref_by(&browser, "r1", 100.0)).unwrap_err(),
            "element does not scroll vertically"
        );
        assert!(block_on(snapshotter.scroll_ref_by(&browser, "r1", f64::INFINITY)).is_err());
        assert!(browser.inputs.lock().unwrap().is_empty());
    }

    #[test]
    fn 선택자로도_스냅샷_없이_중심점을_구한다() {
        let browser = FakeBrowser::default().with_actions([
            json!({ "ok": { "x": 5, "y": 6, "left": 0, "top": 0, "width": 10, "height": 12 } }),
            json!({ "error": "no element matches selector: #missing" }),
        ]);

        let point = block_on(point_for_selector(&browser, "#buy")).unwrap();
        assert_eq!((point.x, point.y), (5.0, 6.0));
        assert_eq!(
            browser.calls()[0],
            "(\"act\", {\"selector\":\"#buy\",\"action\":\"point\",\"value\":null})"
        );
        assert_eq!(
            block_on(point_for_selector(&browser, "#missing")).unwrap_err(),
            "no element matches selector: #missing"
        );
    }

    // --- pure helpers ---

    #[test]
    fn 이름은_utf16_80단위에서_자르고_말줄임표를_붙인다() {
        assert_eq!(quote("Buy \"now\"\n"), "\"Buy \\\"now\\\"\\n\"");
        assert_eq!(quote(&"a".repeat(80)), format!("\"{}\"", "a".repeat(80)));
        assert_eq!(quote(&"a".repeat(81)), format!("\"{}…\"", "a".repeat(80)));
        // 가 is one UTF-16 unit and three bytes: the limit counts units.
        assert_eq!(quote(&"가".repeat(81)), format!("\"{}…\"", "가".repeat(80)));
        // 😀 is two units; 40 of them are exactly 80.
        assert_eq!(quote(&"😀".repeat(40)), format!("\"{}\"", "😀".repeat(40)));
        // Cut through a pair: JSON.stringify prints the lone half as an escape.
        assert_eq!(
            quote(&format!("a{}", "😀".repeat(40))),
            format!("\"a{}\\ud83d…\"", "😀".repeat(39))
        );
    }

    #[test]
    fn 자바스크립트처럼_문자열로_바꾸고_참거짓을_가린다() {
        assert_eq!(js_string(&json!(2)), "2");
        assert_eq!(js_string(&json!(2.0)), "2");
        assert_eq!(js_string(&json!(2.5)), "2.5");
        assert_eq!(js_string(&json!(true)), "true");
        assert_eq!(js_string(&json!("mixed")), "mixed");
        assert!(truthy(&json!("false")));
        assert!(!truthy(&json!("")));
        assert!(!truthy(&json!(0)));
        assert!(!truthy(&json!(false)));
        assert!(!truthy(&Value::Null));
        assert_eq!(js_trim("\u{feff}\u{a0} name\t\n"), "name");
        assert_eq!(js_trim("\u{85}name"), "\u{85}name");
    }

    #[test]
    fn 숫자가_아닌_level과_거짓인_상태는_쓰지_않는다() {
        let mut page = Page::new();
        page.el(2, 3, "heading", "Title", [0.0, 0.0, 300.0, 40.0])["properties"] = json!([
            { "name": "level", "value": { "value": "2" } },
            { "name": "checked", "value": { "value": false } },
            { "name": "selected", "value": {} },
            { "name": "expanded", "value": { "value": true } },
            { "name": "focusable", "value": { "value": true } }
        ]);
        page.el(2, 4, "slider", "Volume", [0.0, 50.0, 300.0, 20.0])["value"] =
            json!({ "value": 40 });
        page.el(2, 5, "textbox", "Empty", [0.0, 80.0, 300.0, 20.0])["value"] =
            json!({ "value": "" });

        assert_eq!(
            snap_once(&page).tree,
            lines(&[
                "- RootWebArea \"Test page\"",
                "  - heading \"Title\" expanded=true",
                "  - slider \"Volume\" value=\"40\" [ref=r1]",
                "  - textbox \"Empty\" [ref=r2]",
            ])
        );
    }

    #[test]
    fn 첫_텍스트는_세_단계_아래까지만_찾는다() {
        let scope = |value: Value| -> HashMap<String, AxNode> {
            let nodes: Vec<AxNode> = serde_json::from_value(value).unwrap();
            nodes.into_iter().map(|n| (n.node_id.clone(), n)).collect()
        };
        let text = |id: &str, name: &str| json!({ "nodeId": id, "role": { "value": "StaticText" }, "name": { "value": name } });
        let near = scope(json!([
            { "nodeId": "a", "childIds": ["missing", "blank", "b"] },
            text("blank", "   "),
            { "nodeId": "b", "childIds": ["c"] },
            text("c", " Deep label ")
        ]));
        assert_eq!(first_text(&near["a"], &near, 0), "Deep label");

        // a → b → c → d → e → text: the text sits one level too deep.
        let far = scope(json!([
            { "nodeId": "a", "childIds": ["b"] },
            { "nodeId": "b", "childIds": ["c"] },
            { "nodeId": "c", "childIds": ["d"] },
            { "nodeId": "d", "childIds": ["e"] },
            { "nodeId": "e", "childIds": ["t"] },
            text("t", "Too deep")
        ]));
        assert_eq!(first_text(&far["a"], &far, 0), "");
        assert_eq!(first_text(&far["b"], &far, 0), "Too deep");
    }

    #[test]
    fn 걸러진_노드를_방향별로_세고_가장_가까운_거리를_남긴다() {
        let mut page = Page::new();
        page.scroll_y = 1000.0;
        page.el(2, 3, "button", "Visible", [0.0, 1100.0, 100.0, 40.0]);
        page.el(2, 4, "button", "Above near", [0.0, 900.0, 100.0, 40.0]);
        page.el(2, 5, "link", "Above far", [0.0, 100.0, 100.0, 40.0]);
        page.el(2, 6, "button", "Below", [0.0, 1824.6, 100.0, 40.0]);
        page.el(2, 7, "button", "Zero", [0.0, 1100.0, 0.0, 0.0]);
        page.el(2, 8, "heading", "Not actionable", [0.0, 100.0, 100.0, 40.0]);
        page.el(2, 9, "button", "Ignored", [0.0, 100.0, 100.0, 40.0])["ignored"] = json!(true);
        let payload: CapturePayload = serde_json::from_value(page.payload()).unwrap();
        let viewport = Viewport {
            x: 0.0,
            y: 1000.0,
            width: 1000.0,
            height: 800.0,
        };
        let layout = capture_page_layout(payload.layout.unwrap(), viewport, None, 1.0).unwrap();

        let tally = tally_filtered(&payload.nodes, &layout);

        assert_eq!(
            (tally.hidden, tally.below, tally.above, tally.side),
            (1, 1, 2, 0)
        );
        assert!((tally.nearest_below - 24.6).abs() < 1e-9);
        assert_eq!(tally.nearest_above, 60.0);
        assert_eq!(
            describe_offscreen(&tally),
            [
                "- [1 more actionable element(s) below the fold — nearest ~25px down]",
                "- [2 more actionable element(s) above the fold — nearest ~60px up]",
            ]
        );
    }

    // --- snapshot-tally.test.ts ---

    const TALLY_VIEWPORT: Viewport = Viewport {
        x: 0.0,
        y: 0.0,
        width: 1000.0,
        height: 800.0,
    };

    fn lay(y: f64) -> NodeLayout {
        NodeLayout {
            x: 0.0,
            y,
            width: 200.0,
            height: 40.0,
            paint_order: 1.0,
            rendered: true,
            zero_size: false,
            scrollable_by: 0.0,
            in_viewport: false,
            occluded: false,
            clickable: false,
            node_index: 0,
            can_occlude: false,
        }
    }

    fn page_layout(entries: &[(u64, NodeLayout)]) -> PageLayout {
        PageLayout::from_entries(entries.iter().copied(), TALLY_VIEWPORT)
    }

    fn ax(node_id: &str, backend_id: u64, role: &str, child_ids: &[&str]) -> AxNode {
        AxNode {
            node_id: node_id.to_owned(),
            backend_dom_node_id: Some(backend_id),
            role: Some(AxValue {
                value: Some(json!(role)),
            }),
            child_ids: child_ids.iter().map(|id| (*id).to_owned()).collect(),
            ..AxNode::default()
        }
    }

    /// Minimal actionable AX node.
    fn link(node_id: &str, backend_id: u64) -> AxNode {
        ax(node_id, backend_id, "link", &[])
    }

    /// Non-actionable container — the shape that used to swallow the tally.
    fn container(node_id: &str, backend_id: u64, child_ids: &[&str]) -> AxNode {
        ax(node_id, backend_id, "LayoutTable", child_ids)
    }

    fn explained(t: &FilterTally) -> bool {
        t.hidden > 0 || t.below + t.above + t.side > 0
    }

    #[test]
    fn 잘려나간_컨테이너_밑의_조작_가능_요소도_전부_집계한다() {
        // The Hacker News shape: one off-screen container holding many off-screen
        // links. The tree walk prunes at the container and never visits the links,
        // so the tally must not depend on traversal.
        let nodes = [
            container("c1", 100, &["l1", "l2", "l3"]),
            link("l1", 1),
            link("l2", 2),
            link("l3", 3),
        ];
        let layout = page_layout(&[
            (
                100,
                NodeLayout {
                    height: 900.0,
                    ..lay(2000.0)
                },
            ),
            (1, lay(2000.0)),
            (2, lay(2100.0)),
            (3, lay(2200.0)),
        ]);

        let t = tally_filtered(&nodes, &layout);

        assert_eq!(t.below, 3);
        assert_eq!(t.nearest_below, 1200.0);
    }

    #[test]
    fn 가장_가까운_화면_밖_요소까지의_스크롤_거리를_보고한다() {
        let nodes = [link("l1", 1), link("l2", 2)];
        let layout = page_layout(&[(1, lay(3000.0)), (2, lay(900.0))]);

        assert_eq!(tally_filtered(&nodes, &layout).nearest_below, 100.0);
    }

    #[test]
    fn 뷰포트_안_요소는_집계하지_않는다() {
        let nodes = [link("l1", 1)];
        let visible = NodeLayout {
            in_viewport: true,
            ..lay(100.0)
        };
        let layout = page_layout(&[(1, visible)]);

        let t = tally_filtered(&nodes, &layout);

        assert_eq!(t.below + t.above + t.side, 0);
        assert_eq!(t.hidden, 0);
    }

    #[test]
    fn 숨김이나_가려진_요소는_hidden으로_분류한다() {
        let nodes = [link("l1", 1), link("l2", 2)];
        let unrendered = NodeLayout {
            rendered: false,
            ..lay(100.0)
        };
        let covered = NodeLayout {
            occluded: true,
            ..lay(100.0)
        };
        let layout = page_layout(&[(1, unrendered), (2, covered)]);

        assert_eq!(tally_filtered(&nodes, &layout).hidden, 2);
    }

    #[test]
    fn 위쪽으로_벗어난_요소는_above로_분류한다() {
        let scrolled = PageLayout::from_entries(
            [(1, lay(100.0))],
            Viewport {
                x: 0.0,
                y: 2000.0,
                width: 1000.0,
                height: 800.0,
            },
        );

        let t = tally_filtered(&[link("l1", 1)], &scrolled);

        assert_eq!(t.above, 1);
        assert_eq!(t.nearest_above, 1860.0);
    }

    #[test]
    fn 조작_불가능한_노드는_집계에서_제외한다() {
        let nodes = [container("c1", 100, &[])];
        let layout = page_layout(&[(100, lay(3000.0))]);

        let t = tally_filtered(&nodes, &layout);

        assert_eq!(t.below + t.above + t.side + t.hidden, 0);
    }

    #[test]
    fn 레이아웃_정보가_없는_노드는_건너뛴다() {
        let t = tally_filtered(&[link("l1", 999)], &page_layout(&[(1, lay(3000.0))]));

        assert_eq!(t.below + t.above + t.side + t.hidden, 0);
    }

    #[test]
    fn 아래쪽_요소_개수와_스크롤_거리를_힌트로_만든다() {
        let hints = describe_offscreen(&FilterTally {
            hidden: 0,
            below: 90,
            above: 0,
            side: 0,
            nearest_below: 1200.0,
            nearest_above: f64::INFINITY,
        });

        assert_eq!(
            hints,
            ["- [90 more actionable element(s) below the fold — nearest ~1200px down]"]
        );
    }

    #[test]
    fn 벗어난_요소가_없으면_힌트를_만들지_않는다() {
        let hints = describe_offscreen(&FilterTally {
            hidden: 3,
            below: 0,
            above: 0,
            side: 0,
            nearest_below: f64::INFINITY,
            nearest_above: f64::INFINITY,
        });

        assert!(hints.is_empty());
    }

    // The overlay case: a full-screen modal legitimately covers every actionable
    // element, so refs hit 0. Falling back to the unfiltered tree there discards
    // a correct result and turns the filter off exactly when it matters most.
    #[test]
    fn 전부_가려져서_ref가_0이면_tally가_이유를_설명하므로_밸브는_발동하지_않아야_한다() {
        let nodes = [link("l1", 1), link("l2", 2)];
        let covered = NodeLayout {
            occluded: true,
            ..lay(100.0)
        };
        let layout = page_layout(&[(1, covered), (2, covered)]);

        let t = tally_filtered(&nodes, &layout);

        assert_eq!(t.hidden, 2);
        assert!(explained(&t));
    }

    #[test]
    fn 설명되지_않는_0건이면_밸브가_발동해야_한다() {
        // No layout entries correlate, so nothing is attributed — the shape a real
        // coordinate mismatch produces.
        let t = tally_filtered(&[link("l1", 999)], &page_layout(&[(1, lay(100.0))]));

        assert!(!explained(&t));
    }

    #[test]
    fn 통계와_결과는_camel_case로_직렬화한다() {
        let page = buttons(&[(10, "Add")]);
        let value = serde_json::to_value(snap_once(&page)).unwrap();

        assert_eq!(value["url"], "https://shop.test/");
        let stats = value["stats"].as_object().unwrap();
        let keys: Vec<&str> = stats.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "refs",
                "hidden",
                "offscreen",
                "clickOnlyRefs",
                "clickScanned",
                "clickScanSkipped",
                "clickMatched",
                "fellBackToFull",
                "frames",
                "framesUnreachable",
                "framesEmpty",
                "newRefs"
            ]
        );
    }

    // --- the page script itself ---

    /// Runs `page_script.js` in node or bun against the stub DOM of
    /// `tests/page_script_harness.cjs`. Skipped when neither runtime is installed.
    fn run_harness() -> Option<Value> {
        let dir = env!("CARGO_MANIFEST_DIR");
        let output = ["node", "bun"].iter().find_map(|runtime| {
            std::process::Command::new(runtime)
                .arg(format!("{dir}/tests/page_script_harness.cjs"))
                .arg(format!("{dir}/src/page_script.js"))
                .output()
                .ok()
        });
        let Some(output) = output else {
            eprintln!("skipped: neither node nor bun is installed");
            return None;
        };
        assert!(
            output.status.success(),
            "the page script failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Some(serde_json::from_slice(&output.stdout).unwrap())
    }

    #[test]
    fn 페이지_스크립트가_읽은_dom으로_같은_윤곽을_만든다() {
        let Some(run) = run_harness() else { return };
        // The script's own answer, a JSON string, goes through the same path as in the app.
        let browser = FakeBrowser::with_captures([run["capture"].clone(), run["capture2"].clone()]);
        let snapshotter = snapshotter();

        let result = snap(&snapshotter, &browser, false);

        assert_eq!(
            result.tree,
            lines(&[
                "- RootWebArea \"Shop\"",
                "  - banner",
                "    - heading \"Shop & Co\" level=1",
                "    - navigation \"Main\"",
                "      - link \"Home\" [ref=r1]",
                "  - button \"Buy now\" [ref=r2]",
                "  - LabelText",
                "    - text \"Search\"",
                "  - textbox \"Search\" value=\"shoes\" [ref=r3]",
                "    - text \"shoes\"",
                "  - textbox \"Password\" value=\"••••\" [ref=r4]",
                "    - text \"••••\"",
                // The label that only repeats its checkbox's name is left out.
                "  - checkbox \"Accept terms\" checked=false [ref=r5]",
                "  - combobox \"Size\" value=\"Medium\" [ref=r6]",
                "    - MenuListPopup",
                "      - option \"Small\" [ref=r7]",
                "      - option \"Medium\" selected=true [ref=r8]",
                "  - button \"Sold out\" disabled [ref=r9]",
                "  - clickable \"Card title\" [ref=r10] (click-scan)",
                "    - text \"Card title\"",
                "  - list [ref=r11] [scrollable +300px]",
                "    - listitem level=1",
                "      - ListMarker \"•\"",
                "      - text \"Order 1\"",
                "  - image \"Logo\"",
                "  - button \"Widget action\" [ref=r12]",
                "  - LabelText",
                "    - checkbox \"Send me news\" checked=true [ref=r13]",
                "    - text \"Send me\"",
                "    - link \"news\" [ref=r14]",
                "  - list",
                "    - listitem level=1",
                "      - ListMarker \"4.\"",
                "      - text \"Pay\"",
                "    - listitem level=1",
                "      - ListMarker \"9.\"",
                "      - text \"Ship\"",
                "    - listitem level=1",
                "      - text \"Done\"",
                "  - textbox \"Notes\" value=\"line one\\n\\nline two\" [ref=r15]",
                "    - text \"line one\"",
                "    - text \"line two\"",
                // Found by its contextmenu handler; the bare canvas after it has no line.
                "  - clickable \"Row menu\" [ref=r16] (click-scan)",
                "    - text \"Row menu\"",
                "  - group \"Billing\"",
                "    - Legend",
                "      - text \"Billing\"",
                "    - textbox \"VAT\" disabled [ref=r17]",
                "  - meter value=\"0.6000000238418579\"",
                "  - generic \"Editor\" value=\"Draft\"",
                "    - text \"Draft\"",
                "  - Iframe \"Pay\"",
                "    - RootWebArea \"Pay doc\"",
                "      - button \"Pay now\" [ref=r18]",
                "  - Iframe \"Ads\"",
                "- clickable [ref=r19] (click-scan, not in the a11y tree)",
                "- Iframe (hidden from the a11y tree) \"https://shop.test/pixel\"",
                "  - RootWebArea \"Tracker\"",
                // Cross-origin and under aria-hidden: listed, not entered.
                "- Iframe (hidden from the a11y tree) \"https://ads.test/beacon\"",
                "- [1 more actionable element(s) below the fold — nearest ~4200px down]",
            ])
        );
        assert_eq!(result.url, "https://shop.test/");
        assert_eq!(
            result.stats,
            SnapshotStats {
                refs: 19,
                hidden: 1,
                offscreen: 1,
                click_only_refs: 3,
                click_scanned: 3,
                click_matched: 3,
                frames: 2,
                frames_unreachable: 2,
                frames_empty: 1,
                ..SnapshotStats::default()
            }
        );

        // The script numbers elements once: a second capture reports the same ids,
        // so nothing reads as new.
        assert_eq!(snap(&snapshotter, &browser, false).stats.new_refs, 0);
    }

    #[test]
    fn 페이지_스크립트가_ref로_요소를_다시_찾아_동작한다() {
        let Some(run) = run_harness() else { return };
        let reply =
            |name: &str| -> Value { serde_json::from_str(run[name].as_str().unwrap()).unwrap() };

        assert_eq!(reply("setRefs"), json!(6));
        // The button sits at (20,100) 100x40 in a 1000x800 viewport: no scroll needed.
        assert_eq!(
            reply("point"),
            json!({ "ok": { "x": 70, "y": 120, "left": 20, "top": 100, "width": 100, "height": 40 } })
        );
        assert_eq!(run["scrolledVisible"], json!(false));
        // Inside the frame at (0,600) with a 2px border: the frame's origin is added back.
        assert_eq!(
            reply("framePoint"),
            json!({ "ok": { "x": 62, "y": 632, "left": 12, "top": 612, "width": 100, "height": 40 } })
        );
        assert_eq!(run["scrolledInFrame"], json!(false));
        // A box below the fold is scrolled to before it is measured.
        assert_eq!(run["scrolledOffscreen"], json!(true));
        assert_eq!(reply("focus"), json!({ "ok": true }));
        assert_eq!(run["focused"], json!("INPUT"));
        assert_eq!(reply("setValue"), json!({ "ok": true }));
        assert_eq!(run["valueAfter"], json!("boots"));
        assert_eq!(run["events"], json!(["input", "change"]));
        assert_eq!(reply("text"), json!({ "ok": "boots" }));
        assert_eq!(reply("buttonText"), json!({ "ok": "Buy now" }));
        assert_eq!(
            reply("setValueOnButton"),
            json!({ "error": "ref is a <button>, not a text field" })
        );
        assert_eq!(reply("select"), json!({ "ok": ["Small"] }));
        assert_eq!(run["selectedAfter"], json!([true, false]));
        assert_eq!(
            reply("selectMissing"),
            json!({ "error": "no option matched", "options": ["Small", "Medium"] })
        );
        assert_eq!(
            reply("selectOnButton"),
            json!({ "error": "ref is a <button>, not a <select>" })
        );
        assert_eq!(
            reply("scrollState"),
            json!({ "ok": { "canScroll": true, "scrollTop": 0 } })
        );
        assert_eq!(
            reply("unknownRef"),
            json!({ "error": "ref \"r99\" is no longer on the page — call browser_snapshot again" })
        );
        assert_eq!(
            reply("selectorPoint"),
            json!({ "ok": { "x": 70, "y": 120, "left": 20, "top": 100, "width": 100, "height": 40 } })
        );
        assert_eq!(
            reply("selectorMissing"),
            json!({ "error": "no element matches selector: #missing" })
        );
        assert_eq!(
            reply("selectorInvalid"),
            json!({ "error": "no element matches selector: div >" })
        );
        assert_eq!(
            reply("detached"),
            json!({ "error": "ref \"r1\" is no longer on the page — call browser_snapshot again" })
        );
        assert_eq!(
            reply("unknownAction"),
            json!({ "error": "unknown action: dance" })
        );
        // The capture is not kept once the refs are registered.
        assert_eq!(run["lastDropped"], json!(true));
        assert_eq!(run["globalHidden"], json!(true));
    }
}
