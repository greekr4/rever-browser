//! Port of `src/main/mcp/layout.ts` — per-node layout facts (box, visibility,
//! occlusion, scrollability, click signal) that decide what the page outline keeps.
//!
//! The TypeScript read these from Chrome's `DOMSnapshot.captureSnapshot` and a
//! separate in-page click scan. This engine has no DevTools protocol, so the page
//! script evaluated by `snapshot.rs` reports the same payload shapes from inside
//! the page (the click scan is part of that script) and everything here works on
//! those payloads unchanged.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use serde::Deserialize;

/// Layout facts for one DOM node, correlated back to the AX tree by
/// backendNodeId. Sourced from a single capture so the whole page costs one
/// round trip, not one per node.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeLayout {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub paint_order: f64,
    /// false when a style hides it — visibility or opacity. These inherit, so a
    /// false here means the whole subtree is hidden too.
    pub rendered: bool,
    /// true for a box with no width or height. Deliberately separate from
    /// `rendered`: a zero-size wrapper is invisible itself but its children can
    /// overflow it and be perfectly visible. Treating the two the same pruned
    /// whole pages — GitHub and Coupang returned a bare RootWebArea because one
    /// `0,0,1137x0` wrapper sat above everything.
    pub zero_size: bool,
    /// true when the box intersects the current viewport
    pub in_viewport: bool,
    /// true when a later-painted box fully covers it (modal overlays, drawers)
    pub occluded: bool,
    /// true when the in-page scan found a click signal the accessibility tree
    /// misses — a `<div onClick>` with no ARIA role, for example.
    pub clickable: bool,
    /// Index into the document's node arrays, used to test ancestry.
    pub node_index: usize,
    /// true only for a box that could actually hide something: lifted out of flow
    /// AND painted with a non-transparent background.
    ///
    /// Geometry and paint order alone kept mistaking ordinary layout for overlays
    /// and blanking pages - three separate times, on GitHub, on Naver's root, and
    /// on the banner holding Naver's search box. A transparent wrapper covers its
    /// siblings geometrically while hiding nothing.
    pub can_occlude: bool,
    /// Pixels of content past this box's own fold, 0 when it does not scroll.
    ///
    /// `overflow:auto` alone is not enough to report: most such boxes never
    /// overflow. What matters is that content exists beyond the visible band,
    /// because browser_scroll moves the window — if the real scroller is an inner
    /// container, scrolling does nothing and reports no error either.
    pub scrollable_by: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Viewport {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug)]
pub struct PageLayout {
    /// Every laid-out box, including repeats of a backend id.
    entries: Vec<NodeLayout>,
    /// Backend id → its first box in `entries`.
    index: HashMap<u64, usize>,
    /// Backend ids in the order their first box was seen.
    order: Vec<u64>,
    pub viewport: Viewport,
    /// Elements the in-page click scan reported.
    pub click_scanned: usize,
    /// How many of those actually correlated to a snapshot node.
    pub click_matched: usize,
}

impl PageLayout {
    pub fn get(&self, backend_id: u64) -> Option<&NodeLayout> {
        self.index
            .get(&backend_id)
            .and_then(|&at| self.entries.get(at))
    }

    pub fn has(&self, backend_id: u64) -> bool {
        self.index.contains_key(&backend_id)
    }

    /// Nodes in capture order, one box per backend id.
    pub fn iter(&self) -> impl Iterator<Item = (u64, &NodeLayout)> {
        self.order
            .iter()
            .filter_map(|&id| self.get(id).map(|layout| (id, layout)))
    }
}

#[cfg(test)]
impl PageLayout {
    /// A layout holding exactly these boxes, for tests that write one by hand.
    pub(crate) fn from_entries(
        boxes: impl IntoIterator<Item = (u64, NodeLayout)>,
        viewport: Viewport,
    ) -> Self {
        let (order, entries): (Vec<u64>, Vec<NodeLayout>) = boxes.into_iter().unzip();
        Self {
            index: order.iter().enumerate().map(|(at, &id)| (id, at)).collect(),
            entries,
            order,
            viewport,
            click_scanned: 0,
            click_matched: 0,
        }
    }
}

// Computed styles ride along in this order; `layout.styles[i]` is parallel to
// it, so the indices below must move together with what the page script emits.
const STYLE_VISIBILITY: usize = 0;
const STYLE_OPACITY: usize = 1;
const STYLE_OVERFLOW_X: usize = 2;
const STYLE_OVERFLOW_Y: usize = 3;
const STYLE_POSITION: usize = 4;
const STYLE_BACKGROUND: usize = 5;

/// An overlay is lifted out of flow; an ordinary wrapper is not.
const OVERLAY_POSITION: [&str; 3] = ["fixed", "absolute", "sticky"];

/// Any of these on an ancestor means it clips whatever sticks out of its box.
const CLIPPING_OVERFLOW: [&str; 5] = ["hidden", "auto", "scroll", "clip", "overlay"];

/// A box only counts as an occluder if it covers at least this much of the
/// viewport. Keeps the check on real overlays instead of every wrapper div.
const OCCLUDER_MIN_AREA_RATIO: f64 = 0.25;

/// Upper bound on the occluder scan so a huge page can't make it quadratic.
const MAX_OCCLUDERS: usize = 40;

/// JavaScript's `parseFloat`: the longest numeric prefix, NaN when there is none.
fn parse_float(text: &str) -> f64 {
    let text = text.trim_start();
    let bytes = text.as_bytes();
    let digits = |from: usize| {
        bytes.get(from..).map_or(0, |rest| {
            rest.iter().take_while(|b| b.is_ascii_digit()).count()
        })
    };
    let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let whole = digits(end);
    end += whole;
    let mut fraction = 0;
    if bytes.get(end) == Some(&b'.') {
        fraction = digits(end + 1);
        if whole + fraction > 0 {
            end += 1 + fraction;
        }
    }
    if whole + fraction == 0 {
        return f64::NAN;
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(bytes.get(end + 1), Some(b'+' | b'-')));
        let exponent = digits(end + 1 + sign);
        if exponent > 0 {
            end += 1 + sign + exponent;
        }
    }
    text.get(..end)
        .and_then(|number| number.parse().ok())
        .unwrap_or(f64::NAN)
}

/// Reads the alpha channel of a computed colour; anything unparsed counts as opaque.
fn is_opaque_background(color: &str) -> bool {
    if color.is_empty() || color == "transparent" {
        return false;
    }
    // The first `rgb(…)` or `rgba(…)` with something between the parentheses.
    let inner = color.match_indices("rgb").find_map(|(at, _)| {
        let rest = color.get(at + 3..)?;
        let rest = rest.strip_prefix('a').unwrap_or(rest).strip_prefix('(')?;
        let inner = rest.get(..rest.find(')')?)?;
        (!inner.is_empty()).then_some(inner)
    });
    let Some(inner) = inner else { return true };
    let parts: Vec<f64> = inner.split(',').map(parse_float).collect();
    parts.get(3).is_none_or(|&alpha| alpha > 0.0)
}

/// Ancestry walks are bounded; real documents nest far shallower than this.
const MAX_ANCESTRY_WALK: usize = 64;

/// Below this, the overflow is rounding noise rather than hidden content.
const MIN_SCROLLABLE_PX: f64 = 16.0;

/// Only these actually scroll; `hidden` clips and `visible` spills.
const SCROLLING_OVERFLOW: [&str; 3] = ["auto", "scroll", "overlay"];

/// The flat node table of one document, as parallel arrays.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SnapshotNodes {
    pub backend_node_id: Option<Vec<u64>>,
    pub parent_index: Option<Vec<i64>>,
    pub node_type: Option<Vec<i64>>,
}

/// One row per laid-out node, as parallel arrays.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SnapshotLayout {
    pub node_index: Vec<i64>,
    pub styles: Vec<Vec<i64>>,
    pub bounds: Vec<Vec<f64>>,
    pub paint_orders: Option<Vec<f64>>,
    pub scroll_rects: Option<Vec<Vec<f64>>>,
    pub client_rects: Option<Vec<Vec<f64>>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct DocumentSnapshot {
    pub nodes: SnapshotNodes,
    pub layout: SnapshotLayout,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CaptureResult {
    pub documents: Vec<DocumentSnapshot>,
    pub strings: Vec<String>,
}

fn intersects(a: &NodeLayout, v: &Viewport) -> bool {
    a.x + a.width > v.x && a.x < v.x + v.width && a.y + a.height > v.y && a.y < v.y + v.height
}

fn intersect_rect(a: Viewport, b: Viewport) -> Viewport {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    Viewport {
        x,
        y,
        width: ((a.x + a.width).min(b.x + b.width) - x).max(0.0),
        height: ((a.y + a.height).min(b.y + b.height) - y).max(0.0),
    }
}

/// The style string of layout row `row` at `slot`; empty when the row does not carry it.
fn style_at<'a>(
    layout: &SnapshotLayout,
    strings: &'a [String],
    row: usize,
    slot: usize,
) -> &'a str {
    layout
        .styles
        .get(row)
        .and_then(|style| style.get(slot))
        .and_then(|&at| usize::try_from(at).ok())
        .and_then(|at| strings.get(at))
        .map_or("", String::as_str)
}

fn rect_of(rects: &[Vec<f64>], row: usize) -> Option<[f64; 4]> {
    match rects.get(row)?.as_slice() {
        &[x, y, width, height, ..] => Some([x, y, width, height]),
        _ => None,
    }
}

/// Effective visible region for each laid-out node: the viewport intersected
/// with every clipping ancestor's box.
///
/// Without this a button scrolled out of an `overflow:auto` container still
/// counts as visible, because its own rect is inside the page viewport — the
/// container is on screen, the button is just clipped out of it. A fixture run
/// showed exactly that: `C1-BOTTOM needs inner scroll` got a ref while being
/// invisible, and the filter reported nothing hidden at all. Reporting an
/// element the agent cannot see is worse than missing one; it plans against a
/// screen state that does not exist.
fn build_clip_rects(
    doc: &DocumentSnapshot,
    strings: &[String],
    viewport: Viewport,
) -> HashMap<usize, Viewport> {
    let mut clip_by_node_index = HashMap::new();
    let Some(parent_index) = &doc.nodes.parent_index else {
        return clip_by_node_index;
    };
    let layout = &doc.layout;

    // Only laid-out nodes have a box, so clipping ancestors are looked up here.
    let mut own_clip = HashMap::new();
    for (row, &node_index) in layout.node_index.iter().enumerate() {
        let Some([x, y, width, height]) = rect_of(&layout.bounds, row) else {
            continue;
        };
        let ox = style_at(layout, strings, row, STYLE_OVERFLOW_X);
        let oy = style_at(layout, strings, row, STYLE_OVERFLOW_Y);
        if !CLIPPING_OVERFLOW.contains(&ox) && !CLIPPING_OVERFLOW.contains(&oy) {
            continue;
        }
        if let Ok(node_index) = usize::try_from(node_index) {
            let own = Viewport {
                x,
                y,
                width,
                height,
            };
            own_clip.insert(node_index, own);
        }
    }
    if own_clip.is_empty() {
        return clip_by_node_index;
    }

    for &node_index in &layout.node_index {
        let Ok(node_index) = usize::try_from(node_index) else {
            continue;
        };
        // Climb to the nearest resolved ancestor, then come back down. A loop instead
        // of recursion, and bounded by the table size, so a parent table that points
        // back at itself cannot hang or overflow the stack.
        let mut chain = Vec::new();
        let mut inherited = viewport;
        let mut current = node_index;
        loop {
            if let Some(&known) = clip_by_node_index.get(&current) {
                inherited = known;
                break;
            }
            chain.push(current);
            let parent = parent_index
                .get(current)
                .and_then(|&parent| usize::try_from(parent).ok())
                .filter(|&parent| parent != current && chain.len() <= parent_index.len());
            match parent {
                Some(parent) => current = parent,
                None => break,
            }
        }
        for at in chain.into_iter().rev() {
            if let Some(&own) = own_clip.get(&at) {
                inherited = intersect_rect(inherited, own);
            }
            clip_by_node_index.insert(at, inherited);
        }
    }
    clip_by_node_index
}

fn contains(outer: &NodeLayout, inner: &NodeLayout) -> bool {
    outer.x <= inner.x
        && outer.y <= inner.y
        && outer.x + outer.width >= inner.x + inner.width
        && outer.y + outer.height >= inner.y + inner.height
}

/// Whether one node is the other's ancestor (or the same node).
fn is_related(a: usize, b: usize, parent_index: &[i64]) -> bool {
    for (from, to) in [(a, b), (b, a)] {
        let mut current = from;
        for _ in 0..MAX_ANCESTRY_WALK {
            if current == to {
                return true;
            }
            let next = parent_index
                .get(current)
                .and_then(|&next| usize::try_from(next).ok());
            match next {
                Some(next) if next != current => current = next,
                _ => break,
            }
        }
    }
    false
}

/// Mark every rendered box that a later-painted, viewport-sized box fully
/// covers. Parents paint before their children, so a box that both contains
/// another box AND paints after it is a genuine overlay rather than an
/// ancestor — that is what makes this cheap heuristic safe.
fn mark_occluded(entries: &mut [NodeLayout], viewport: Viewport, parent_index: &[i64]) {
    let min_area = viewport.width * viewport.height * OCCLUDER_MIN_AREA_RATIO;
    let mut occluders: Vec<NodeLayout> = entries
        .iter()
        .filter(|e| e.rendered && !e.zero_size && e.can_occlude && e.width * e.height >= min_area)
        .copied()
        .collect();
    occluders.sort_by(|a, b| b.paint_order.total_cmp(&a.paint_order));
    occluders.truncate(MAX_OCCLUDERS);

    if occluders.is_empty() {
        return;
    }

    for e in entries.iter_mut() {
        if !e.rendered || e.zero_size {
            continue;
        }
        for o in &occluders {
            // Sorted by paint order descending, so once we drop below e's own paint
            // order nothing later in the list can cover it either.
            if o.paint_order <= e.paint_order {
                break;
            }
            // Strictly larger, not merely containing. Nested full-page wrappers share
            // identical bounds, so the inner one both "contains" its parent and
            // paints after it — which read as an overlay and blanked entire pages
            // (GitHub and Coupang returned a bare RootWebArea). A real overlay is
            // bigger than what it hides.
            if o.width * o.height <= e.width * e.height || !contains(o, e) {
                continue;
            }
            // Geometry alone cannot tell an overlay from ordinary nesting: a wrapper
            // the height of the document contains the viewport-sized root box and
            // paints after it, which read as the page being covered and blanked
            // Naver entirely. An overlay is a box from a DIFFERENT branch of the
            // tree, so anything on the target's own ancestor line is not one.
            if is_related(o.node_index, e.node_index, parent_index) {
                continue;
            }
            e.occluded = true;
            break;
        }
    }
}

/// Turn a raw capture payload into per-node layout facts. Pure so the index
/// correlation and the occlusion heuristic can be tested without a live browser.
pub fn build_page_layout(
    snap: &CaptureResult,
    viewport: Viewport,
    clickable_paths: Option<&HashSet<String>>,
) -> Option<PageLayout> {
    // Paths are only meaningful for the main document: the in-page scan does not
    // enter iframes, so an iframe document's paths would collide with the main
    // document's.
    let mut clickable_backend_ids: HashSet<u64> = HashSet::new();
    if let (Some(paths), Some(main)) = (clickable_paths, snap.documents.first())
        && !paths.is_empty()
    {
        let by_path = build_element_paths(main);
        clickable_backend_ids.extend(paths.iter().filter_map(|path| by_path.get(path)));
    }

    let mut click_matched = 0;
    let mut index = HashMap::new();
    let mut order = Vec::new();
    let mut entries = Vec::new();
    // Ancestry is only compared within the main document, which is where
    // full-page wrappers and real overlays both live.
    let main_parent_index = snap
        .documents
        .first()
        .and_then(|doc| doc.nodes.parent_index.as_deref())
        .unwrap_or(&[]);

    for doc in &snap.documents {
        let Some(backend_ids) = &doc.nodes.backend_node_id else {
            continue;
        };
        let layout = &doc.layout;
        let clip_by_node_index = build_clip_rects(doc, &snap.strings, viewport);
        let style = |row, slot| style_at(layout, &snap.strings, row, slot);

        for (row, &node_index) in layout.node_index.iter().enumerate() {
            let Ok(node_index) = usize::try_from(node_index) else {
                continue;
            };
            let Some(&backend_id) = backend_ids.get(node_index) else {
                continue;
            };
            let Some([x, y, width, height]) = rect_of(&layout.bounds, row) else {
                continue;
            };

            let visibility = style(row, STYLE_VISIBILITY);
            let ox = style(row, STYLE_OVERFLOW_X);
            let oy = style(row, STYLE_OVERFLOW_Y);

            let rendered = visibility != "hidden"
                && visibility != "collapse"
                && style(row, STYLE_OPACITY) != "0";
            let zero_size = width <= 0.0 || height <= 0.0;

            // Overflowing content is not the same as a scrollable box: an
            // `overflow: visible` element reports a taller scroll rect while simply
            // spilling out, and marking those buried the real scrollers under 38
            // false positives on one page. The style has to allow scrolling.
            let scrolls = SCROLLING_OVERFLOW.contains(&ox) || SCROLLING_OVERFLOW.contains(&oy);
            let rects = |rects: &Option<Vec<Vec<f64>>>| rect_of(rects.as_deref()?, row);
            let scrollable_by = match (rects(&layout.scroll_rects), rects(&layout.client_rects)) {
                // floor(x + 0.5) is Math.round.
                (Some(scroll), Some(client)) if scrolls => {
                    (scroll[3] - client[3] + 0.5).floor().max(0.0)
                }
                _ => 0.0,
            };

            let mut entry = NodeLayout {
                x,
                y,
                width,
                height,
                paint_order: layout
                    .paint_orders
                    .as_ref()
                    .and_then(|orders| orders.get(row))
                    .copied()
                    .unwrap_or(0.0),
                rendered,
                zero_size,
                in_viewport: false,
                occluded: false,
                clickable: clickable_backend_ids.contains(&backend_id),
                node_index,
                can_occlude: OVERLAY_POSITION.contains(&style(row, STYLE_POSITION))
                    && is_opaque_background(style(row, STYLE_BACKGROUND)),
                scrollable_by: if scrollable_by >= MIN_SCROLLABLE_PX {
                    scrollable_by
                } else {
                    0.0
                },
            };
            if entry.clickable {
                click_matched += 1;
            }
            // Visible means inside the viewport AND inside every clipping ancestor.
            entry.in_viewport = intersects(
                &entry,
                clip_by_node_index.get(&node_index).unwrap_or(&viewport),
            );

            // A backendNodeId can appear more than once across documents; the first
            // laid-out box wins, which is the one the user actually sees.
            if let Entry::Vacant(first) = index.entry(backend_id) {
                first.insert(entries.len());
                order.push(backend_id);
            }
            entries.push(entry);
        }
    }

    if index.is_empty() {
        return None;
    }

    mark_occluded(&mut entries, viewport, main_parent_index);
    Some(PageLayout {
        entries,
        index,
        order,
        viewport,
        click_scanned: clickable_paths.map_or(0, HashSet::len),
        click_matched,
    })
}

const ELEMENT_NODE: i64 = 1;
const DOCUMENT_NODE: i64 = 9;

/// Map every element in a document snapshot to its element-child index path from
/// `<html>` — `"1.3.0"` is documentElement.children[1].children[3].children[0].
///
/// Geometry was the obvious way to correlate in-page findings with snapshot
/// nodes and it failed outright: a fixture run in the app reported `10
/// candidate(s), 0 correlated`. The cause was never pinned down — a later probe
/// against plain headless Chrome matched 10 of 10 on the same fixture, so the
/// two coordinate spaces do agree under at least some conditions, and something
/// about the embedded webview (scale factor, zoom, or an emulated viewport) is
/// the likely difference.
///
/// Paths sidestep the question entirely: they are exact regardless of scale,
/// origin or rounding, and cost nothing extra since the node tree already rides
/// along in the payload we fetch.
pub fn build_element_paths(doc: &DocumentSnapshot) -> HashMap<String, u64> {
    let mut by_path = HashMap::new();
    let (Some(parent_index), Some(node_type), Some(backend_ids)) = (
        &doc.nodes.parent_index,
        &doc.nodes.node_type,
        &doc.nodes.backend_node_id,
    ) else {
        return by_path;
    };

    let mut children_of: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut root = None;
    for (i, &kind) in node_type.iter().enumerate() {
        if kind != ELEMENT_NODE {
            continue;
        }
        let Some(parent) = parent_index.get(i).and_then(|&p| usize::try_from(p).ok()) else {
            continue;
        };
        if node_type.get(parent) == Some(&DOCUMENT_NODE) && root.is_none() {
            root = Some(i);
        }
        children_of.entry(parent).or_default().push(i);
    }
    let Some(root) = root else { return by_path };

    let mut stack = vec![(root, String::new())];
    while let Some((at, path)) = stack.pop() {
        if !path.is_empty()
            && let Some(&backend_id) = backend_ids.get(at)
        {
            by_path.insert(path.clone(), backend_id);
        }
        let kids = children_of.get(&at).map_or(&[][..], Vec::as_slice);
        for (k, &kid) in kids.iter().enumerate().rev() {
            let kid_path = if path.is_empty() {
                k.to_string()
            } else {
                format!("{path}.{k}")
            };
            stack.push((kid, kid_path));
        }
    }
    by_path
}

/// Above this element count the in-page click scan is skipped as too costly.
pub const MAX_SCANNED_ELEMENTS: usize = 10_000;

/// Shadow roots nested deeper than this are not worth the traversal.
pub const MAX_SHADOW_DEPTH: usize = 5;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClickScan {
    /// Element-child index paths from `<html>`, e.g. "1.3.0".
    pub paths: HashSet<String>,
    pub scanned: usize,
    /// true when the page was too large and the scan was skipped entirely
    pub skipped: bool,
}

/// What the page script reports for the click scan.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ClickScanPayload {
    pub paths: Vec<String>,
    pub scanned: f64,
    pub skipped: bool,
}

/// The click scan as the snapshot uses it.
///
/// The accessibility tree only marks elements that carry an interactive role,
/// so a `<div onClick>` with no ARIA is invisible to it — and an element the
/// agent cannot click is an API call it can never trigger.
///
/// The scan runs inside the page script and reports paths, never touching the
/// DOM. Marking elements with a data attribute would be simpler, but this app
/// also ships stealth measures: writing attributes onto every clickable
/// element fires the target site's MutationObservers, which is exactly what
/// bot detection watches for.
///
/// Known gap: React 17+ delegates events to the root container, so no listener
/// sits on individual React elements. The cursor heuristic in the script is
/// what actually catches those.
pub fn scan_clickable(payload: Option<ClickScanPayload>) -> ClickScan {
    // No answer counts as a skipped scan: the outline has no click-only refs either way.
    let Some(payload) = payload else {
        return ClickScan {
            skipped: true,
            ..ClickScan::default()
        };
    };
    ClickScan {
        // The length of the reported list, repeats included, as the TypeScript counted it.
        scanned: payload.paths.len(),
        paths: payload.paths.into_iter().collect(),
        skipped: payload.skipped,
    }
}

/// Bounds, visibility styles and paint order for every laid-out node of a
/// capture. Returns `None` when the page has no layout data — callers must
/// treat that as "no filtering possible" rather than as an empty page.
pub fn capture_page_layout(
    mut snap: CaptureResult,
    viewport: Viewport,
    clickable_paths: Option<&HashSet<String>>,
    // device-px → CSS-px divisor; 1 leaves the capture untouched.
    scale: f64,
) -> Option<PageLayout> {
    if scale != 1.0 {
        normalize_snapshot_scale(&mut snap, scale);
    }
    build_page_layout(&snap, viewport, clickable_paths)
}

/// A capture taken in device pixels has to be brought into CSS px, the space
/// the rest of the pipeline works in. Divide every rect array by the scale
/// factor so the boxes line up with the CSS viewport regardless of the
/// display's scale.
pub fn normalize_snapshot_scale(snap: &mut CaptureResult, scale: f64) {
    for doc in &mut snap.documents {
        let layout = &mut doc.layout;
        let rect_arrays = [
            Some(&mut layout.bounds),
            layout.scroll_rects.as_mut(),
            layout.client_rects.as_mut(),
        ];
        for rect in rect_arrays.into_iter().flatten().flatten() {
            for value in rect {
                *value /= scale;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const VIEWPORT: Viewport = Viewport {
        x: 0.0,
        y: 0.0,
        width: 1000.0,
        height: 800.0,
    };

    /// Index into the shared string table used by every fixture below.
    fn strings() -> Value {
        json!([
            "visible",
            "hidden",
            "1",
            "0",
            "collapse",
            "fixed",
            "rgb(20, 20, 20)"
        ])
    }
    const S_FIXED: i64 = 5;
    const S_OPAQUE: i64 = 6;
    const S_VISIBLE: i64 = 0;
    const S_HIDDEN: i64 = 1;
    const S_OPACITY_1: i64 = 2;
    const S_OPACITY_0: i64 = 3;

    #[derive(Default)]
    struct NodeSpec {
        backend_node_id: u64,
        bounds: [f64; 4],
        paint_order: Option<f64>,
        visibility: Option<i64>,
        opacity: Option<i64>,
        /// marks the box as a real overlay: positioned and opaque
        overlay: bool,
    }

    fn spec(backend_node_id: u64, bounds: [f64; 4]) -> NodeSpec {
        NodeSpec {
            backend_node_id,
            bounds,
            ..NodeSpec::default()
        }
    }

    fn capture(value: Value) -> CaptureResult {
        serde_json::from_value(value).unwrap()
    }

    /// Build a captureSnapshot-shaped payload from a compact node list.
    fn snapshot(specs: &[NodeSpec]) -> CaptureResult {
        let column = |f: &dyn Fn(usize, &NodeSpec) -> Value| -> Vec<Value> {
            specs.iter().enumerate().map(|(i, s)| f(i, s)).collect()
        };
        capture(json!({
            "strings": strings(),
            "documents": [{
                "nodes": { "backendNodeId": column(&|_, s| json!(s.backend_node_id)) },
                "layout": {
                    "nodeIndex": column(&|i, _| json!(i)),
                    "bounds": column(&|_, s| json!(s.bounds)),
                    "styles": column(&|_, s| json!([
                        s.visibility.unwrap_or(S_VISIBLE),
                        s.opacity.unwrap_or(S_OPACITY_1),
                        -1,
                        -1,
                        if s.overlay { S_FIXED } else { -1 },
                        if s.overlay { S_OPAQUE } else { -1 }
                    ])),
                    "paintOrders": column(&|i, s| json!(s.paint_order.unwrap_or(i as f64)))
                }
            }]
        }))
    }

    fn layout_of(specs: &[NodeSpec]) -> PageLayout {
        build_page_layout(&snapshot(specs), VIEWPORT, None).unwrap()
    }

    fn paths(list: &[&str]) -> HashSet<String> {
        list.iter().map(|path| (*path).to_owned()).collect()
    }

    // --- buildPageLayout: index correlation ---

    #[test]
    fn 맵의_키를_node_index가_가리키는_backend_node_id로_잡는다() {
        // nodeIndex is deliberately out of order: layout row 0 describes node 2.
        let snap = capture(json!({
            "strings": strings(),
            "documents": [{
                "nodes": { "backendNodeId": [10, 20, 30] },
                "layout": {
                    "nodeIndex": [2, 0],
                    "bounds": [[5, 5, 50, 50], [100, 100, 20, 20]],
                    "styles": [[S_VISIBLE, S_OPACITY_1], [S_VISIBLE, S_OPACITY_1]],
                    "paintOrders": [1, 2]
                }
            }]
        }));

        let layout = build_page_layout(&snap, VIEWPORT, None).unwrap();

        assert_eq!(layout.get(30).unwrap().x, 5.0);
        assert_eq!(layout.get(10).unwrap().x, 100.0);
        assert!(!layout.has(20));
    }

    #[test]
    fn 레이아웃_항목이_하나도_없으면_none을_반환한다() {
        let empty = capture(json!({
            "strings": strings(),
            "documents": [{
                "nodes": { "backendNodeId": [] },
                "layout": { "nodeIndex": [], "bounds": [], "styles": [] }
            }]
        }));

        assert!(build_page_layout(&empty, VIEWPORT, None).is_none());
    }

    // --- rendered 판정 ---

    #[test]
    fn visibility_hidden이면_rendered_false로_표시한다() {
        let layout = layout_of(&[NodeSpec {
            visibility: Some(S_HIDDEN),
            ..spec(1, [0.0, 0.0, 100.0, 100.0])
        }]);

        assert!(!layout.get(1).unwrap().rendered);
    }

    #[test]
    fn opacity_0이면_rendered_false로_표시한다() {
        let layout = layout_of(&[NodeSpec {
            opacity: Some(S_OPACITY_0),
            ..spec(1, [0.0, 0.0, 100.0, 100.0])
        }]);

        assert!(!layout.get(1).unwrap().rendered);
    }

    #[test]
    fn 크기가_0인_박스는_zero_size_true로_표시한다() {
        // Separate from `rendered` on purpose: a zero-size wrapper is invisible
        // itself, but its children can overflow it and be visible.
        let layout = layout_of(&[spec(1, [10.0, 10.0, 0.0, 0.0])]);

        assert!(layout.get(1).unwrap().zero_size);
        assert!(layout.get(1).unwrap().rendered);
    }

    #[test]
    fn 보이는_박스는_rendered_true로_표시한다() {
        let layout = layout_of(&[spec(1, [10.0, 10.0, 100.0, 40.0])]);

        assert!(layout.get(1).unwrap().rendered);
    }

    // --- 뷰포트 교차 ---

    #[test]
    fn 뷰포트_안_박스는_in_viewport_true다() {
        let layout = layout_of(&[spec(1, [10.0, 10.0, 100.0, 40.0])]);

        assert!(layout.get(1).unwrap().in_viewport);
    }

    #[test]
    fn 스크롤_아래쪽_박스는_in_viewport_false다() {
        let layout = layout_of(&[spec(1, [10.0, 5000.0, 100.0, 40.0])]);

        assert!(!layout.get(1).unwrap().in_viewport);
    }

    #[test]
    fn 스크롤된_뷰포트를_문서_좌표_기준으로_판정한다() {
        let scrolled = Viewport {
            x: 0.0,
            y: 4000.0,
            width: 1000.0,
            height: 800.0,
        };
        let layout = build_page_layout(
            &snapshot(&[
                spec(1, [10.0, 4100.0, 100.0, 40.0]),
                spec(2, [10.0, 10.0, 100.0, 40.0]),
            ]),
            scrolled,
            None,
        )
        .unwrap();

        assert!(layout.get(1).unwrap().in_viewport);
        assert!(!layout.get(2).unwrap().in_viewport);
    }

    #[test]
    fn 경계에_걸친_박스는_in_viewport_true다() {
        let layout = layout_of(&[spec(1, [10.0, 780.0, 100.0, 100.0])]);

        assert!(layout.get(1).unwrap().in_viewport);
    }

    // --- 페인트 순서 가려짐 판정 ---

    fn painted(id: u64, bounds: [f64; 4], paint_order: f64, overlay: bool) -> NodeSpec {
        NodeSpec {
            paint_order: Some(paint_order),
            overlay,
            ..spec(id, bounds)
        }
    }

    #[test]
    fn 나중에_그려진_전체_화면_오버레이가_덮은_박스를_occluded로_표시한다() {
        let layout = layout_of(&[
            painted(1, [100.0, 100.0, 200.0, 50.0], 1.0, false),
            painted(2, [0.0, 0.0, 1000.0, 800.0], 9.0, true),
        ]);

        assert!(layout.get(1).unwrap().occluded);
    }

    #[test]
    fn 오버레이보다_나중에_그려진_모달_내용은_occluded가_아니다() {
        let layout = layout_of(&[
            painted(1, [0.0, 0.0, 1000.0, 800.0], 5.0, true), // overlay
            painted(2, [300.0, 300.0, 200.0, 60.0], 9.0, false), // modal button
        ]);

        assert!(!layout.get(2).unwrap().occluded);
    }

    #[test]
    fn 먼저_그려진_조상_컨테이너는_자식을_가리지_않는다() {
        let layout = layout_of(&[
            painted(1, [0.0, 0.0, 1000.0, 800.0], 1.0, false), // ancestor
            painted(2, [100.0, 100.0, 200.0, 50.0], 4.0, false), // child
        ]);

        assert!(!layout.get(2).unwrap().occluded);
    }

    #[test]
    fn 뷰포트의_25퍼센트_미만인_작은_박스는_가리개로_치지_않는다() {
        let layout = layout_of(&[
            painted(1, [100.0, 100.0, 50.0, 20.0], 1.0, false),
            painted(2, [90.0, 90.0, 200.0, 100.0], 9.0, true),
        ]);

        assert!(!layout.get(1).unwrap().occluded);
    }

    #[test]
    fn 오버레이가_완전히_감싸지_않으면_occluded가_아니다() {
        let layout = layout_of(&[
            painted(1, [900.0, 100.0, 200.0, 50.0], 1.0, false),
            painted(2, [0.0, 0.0, 1000.0, 800.0], 9.0, true),
        ]);

        assert!(!layout.get(1).unwrap().occluded);
    }

    /// <html>                     idx 1  path ''
    ///   <body>                   idx 2  path '0'
    ///     <div id=a>             idx 3  path '0.0'
    ///       <span id=a1>         idx 4  path '0.0.0'
    ///     <div id=b>             idx 5  path '0.1'
    /// Node 0 is the document itself (nodeType 9).
    fn tree_value(bounds: Value) -> Value {
        json!({
            "strings": strings(),
            "documents": [{
                "nodes": {
                    "backendNodeId": [900, 901, 902, 903, 904, 905],
                    "parentIndex": [-1, 0, 1, 2, 3, 2],
                    "nodeType": [9, 1, 1, 1, 1, 1]
                },
                "layout": {
                    "nodeIndex": [3, 4, 5],
                    "bounds": bounds,
                    "styles": [
                        [S_VISIBLE, S_OPACITY_1],
                        [S_VISIBLE, S_OPACITY_1],
                        [S_VISIBLE, S_OPACITY_1]
                    ],
                    "paintOrders": [1, 2, 3]
                }
            }]
        })
    }

    fn tree() -> CaptureResult {
        capture(tree_value(json!([
            [0, 0, 300, 100],
            [10, 10, 80, 20],
            [0, 200, 300, 100]
        ])))
    }

    // --- buildElementPaths ---

    #[test]
    fn 요소_자식_인덱스_경로를_backend_node_id에_매핑한다() {
        let by_path = build_element_paths(&tree().documents[0]);

        assert_eq!(by_path.get("0"), Some(&902)); // body
        assert_eq!(by_path.get("0.0"), Some(&903)); // div#a
        assert_eq!(by_path.get("0.0.0"), Some(&904)); // span#a1
        assert_eq!(by_path.get("0.1"), Some(&905)); // div#b
    }

    #[test]
    fn document_element_자신은_경로를_갖지_않는다() {
        assert!(!build_element_paths(&tree().documents[0]).contains_key(""));
    }

    #[test]
    fn 노드_트리_정보가_없으면_빈_맵을_반환한다() {
        let bare = DocumentSnapshot {
            nodes: SnapshotNodes {
                backend_node_id: Some(vec![1]),
                ..SnapshotNodes::default()
            },
            layout: tree().documents[0].layout.clone(),
        };
        assert_eq!(build_element_paths(&bare).len(), 0);
    }

    // --- 클릭 스캔 경로 매칭 ---

    #[test]
    fn 경로가_일치하는_노드에만_clickable_true를_붙인다() {
        let layout = build_page_layout(&tree(), VIEWPORT, Some(&paths(&["0.0"]))).unwrap();

        assert!(layout.get(903).unwrap().clickable);
        assert!(!layout.get(904).unwrap().clickable);
        assert!(!layout.get(905).unwrap().clickable);
        assert_eq!(layout.click_matched, 1);
    }

    #[test]
    fn 좌표가_어긋나도_경로_매칭은_영향받지_않는다() {
        // Bounds here are deliberately nothing like a viewport-relative rect; the
        // geometry-keyed version scored 0 correlated on exactly this mismatch.
        let scaled = capture(tree_value(json!([
            [0, 0, 600, 200],
            [20, 20, 160, 40],
            [0, 400, 600, 200]
        ])));

        let layout = build_page_layout(&scaled, VIEWPORT, Some(&paths(&["0.0.0"]))).unwrap();

        assert!(layout.get(904).unwrap().clickable);
        assert_eq!(layout.click_matched, 1);
    }

    #[test]
    fn 스캔_결과를_주지_않으면_clickable은_전부_false다() {
        let layout = build_page_layout(&tree(), VIEWPORT, None).unwrap();

        assert!(!layout.get(903).unwrap().clickable);
        assert_eq!(layout.click_matched, 0);
    }

    #[test]
    fn 없는_경로는_무시하고_매칭_수에_세지_않는다() {
        let layout = build_page_layout(&tree(), VIEWPORT, Some(&paths(&["9.9.9"]))).unwrap();

        assert_eq!(layout.click_matched, 0);
    }

    // --- 위임 루트 억제 (React 형태) ---
    // #d-root carries the single delegated listener; #d2 inside it has the
    // pointer cursor. Preferring the OUTER candidate made the delegation root
    // claim the ref and swallow #d2 — on a real React app that collapses the
    // whole application into one ref. The in-page scan now drops any candidate
    // that contains another candidate, so only the inner one survives.

    #[test]
    fn 자식_후보를_가진_컨테이너는_경로_목록에서_빠진다() {
        // Emulates what the in-page dedup emits: only '0.0.0' (the inner span),
        // never '0.0' (the delegation root that contains it).
        let layout = build_page_layout(&tree(), VIEWPORT, Some(&paths(&["0.0.0"]))).unwrap();

        assert!(layout.get(904).unwrap().clickable); // inner
        assert!(!layout.get(903).unwrap().clickable); // containing root
        assert_eq!(layout.click_matched, 1);
    }

    #[test]
    fn 자식_후보가_없는_컨테이너는_그대로_살아남는다() {
        // C1's nested spans never become candidates (cursor inherits), so the
        // outer element is the only hit and must keep its ref.
        let layout = build_page_layout(&tree(), VIEWPORT, Some(&paths(&["0.0"]))).unwrap();

        assert!(layout.get(903).unwrap().clickable);
        assert!(!layout.get(904).unwrap().clickable);
    }

    /// A page-sized body, a small overflow:auto box inside it, and two children of
    /// that box: one inside its visible band, one scrolled past it. The clipped
    /// child's own rect is still within the page viewport — which is exactly why
    /// comparing against the viewport alone reported it as visible.
    fn clip_tree(styles: Value) -> CaptureResult {
        capture(json!({
            "strings": ["visible", "hidden", "1", "0", "auto"],
            "documents": [{
                "nodes": {
                    "backendNodeId": [900, 901, 902, 903, 904],
                    "parentIndex": [-1, 0, 1, 2, 2],
                    "nodeType": [9, 1, 1, 1, 1]
                },
                "layout": {
                    "nodeIndex": [1, 2, 3, 4],
                    "bounds": [
                        [0, 0, 1000, 700], // html
                        [0, 100, 400, 90], // scroller, overflow-y:auto
                        [10, 110, 200, 30], // child inside the visible band
                        [10, 330, 200, 30] // child scrolled past the container's fold
                    ],
                    "styles": styles,
                    "paintOrders": [1, 2, 3, 4]
                }
            }]
        }))
    }

    fn clipping() -> CaptureResult {
        clip_tree(json!([
            [0, 2, -1, -1],
            [0, 2, -1, 4], // overflow-y: auto
            [0, 2, -1, -1],
            [0, 2, -1, -1]
        ]))
    }

    // --- 조상 클립 박스 ---

    #[test]
    fn 스크롤_컨테이너_밖으로_잘린_자식은_in_viewport_false다() {
        let layout = build_page_layout(&clipping(), VIEWPORT, None).unwrap();

        assert!(layout.get(903).unwrap().in_viewport); // inside the band
        assert!(!layout.get(904).unwrap().in_viewport); // clipped out
    }

    #[test]
    fn 컨테이너_자신은_뷰포트_안이면_그대로_보인다() {
        let layout = build_page_layout(&clipping(), VIEWPORT, None).unwrap();

        assert!(layout.get(902).unwrap().in_viewport);
    }

    #[test]
    fn 클리핑_조상이_없으면_판정이_달라지지_않는다() {
        // same geometry, but the container no longer clips
        let no_clip = clip_tree(json!([
            [0, 2, -1, -1],
            [0, 2, -1, -1],
            [0, 2, -1, -1],
            [0, 2, -1, -1]
        ]));

        let layout = build_page_layout(&no_clip, VIEWPORT, None).unwrap();

        assert!(layout.get(904).unwrap().in_viewport);
    }

    // --- 스크롤 가능 컨테이너 ---

    const S_AUTO: i64 = 4;

    fn with_rects(overflow_y: i64, scroll_h: f64, client_h: f64) -> CaptureResult {
        capture(json!({
            "strings": ["visible", "hidden", "1", "0", "auto"],
            "documents": [{
                "nodes": { "backendNodeId": [900, 901], "parentIndex": [-1, 0], "nodeType": [9, 1] },
                "layout": {
                    "nodeIndex": [1],
                    "bounds": [[0, 0, 400, 90]],
                    "styles": [[0, 2, -1, overflow_y]],
                    "paintOrders": [1],
                    "scrollRects": [[0, 0, 400, scroll_h]],
                    "clientRects": [[0, 0, 400, client_h]]
                }
            }]
        }))
    }

    #[test]
    fn 넘치는_auto_박스는_남은_픽셀을_보고한다() {
        let layout = build_page_layout(&with_rects(S_AUTO, 270.0, 90.0), VIEWPORT, None).unwrap();

        assert_eq!(layout.get(901).unwrap().scrollable_by, 180.0);
    }

    #[test]
    fn overflow_visible이면_내용이_넘쳐도_스크롤로_치지_않는다() {
        // The distinction that removed 38 false positives from one page: content
        // spilling out of a visible box is not something the agent can scroll.
        let layout = build_page_layout(&with_rects(-1, 270.0, 90.0), VIEWPORT, None).unwrap();

        assert_eq!(layout.get(901).unwrap().scrollable_by, 0.0);
    }

    #[test]
    fn 넘치지_않는_auto_박스는_표시하지_않는다() {
        let layout = build_page_layout(&with_rects(S_AUTO, 92.0, 90.0), VIEWPORT, None).unwrap();

        assert_eq!(layout.get(901).unwrap().scrollable_by, 0.0);
    }

    // --- 가려짐 판정의 조상 관계 예외 ---

    /// node 1 = <html>, viewport-sized, painted first
    /// node 2 = a wrapper the height of the document, a CHILD of html
    /// node 3 = an overlay the height of the document, a SIBLING branch
    fn ancestry(overlay_parent: i64) -> CaptureResult {
        capture(json!({
            "strings": strings(),
            "documents": [{
                "nodes": {
                    "backendNodeId": [900, 901, 902, 903],
                    "parentIndex": [-1, 0, 1, overlay_parent],
                    "nodeType": [9, 1, 1, 1]
                },
                "layout": {
                    "nodeIndex": [1, 3],
                    "bounds": [[0, 0, 1000, 800], [0, 0, 1000, 5000]],
                    "styles": [
                        [S_VISIBLE, S_OPACITY_1, -1, -1, -1, -1],
                        [S_VISIBLE, S_OPACITY_1, -1, -1, S_FIXED, S_OPAQUE]
                    ],
                    "paintOrders": [1, 9]
                }
            }]
        }))
    }

    #[test]
    fn 같은_가지의_전체_페이지_래퍼는_오버레이로_치지_않는다() {
        // Naver blanked entirely on this shape: a wrapper as tall as the document
        // contains the viewport-sized root box and paints after it.
        let layout = build_page_layout(&ancestry(1), VIEWPORT, None).unwrap();

        assert!(!layout.get(901).unwrap().occluded);
    }

    #[test]
    fn 다른_가지의_큰_박스는_여전히_가리는_것으로_본다() {
        // Same geometry, but the big box hangs off the document node instead of
        // sitting on the target's own ancestor line — that is a real overlay.
        let layout = build_page_layout(&ancestry(0), VIEWPORT, None).unwrap();

        assert!(layout.get(901).unwrap().occluded);
    }

    // --- Not in layout.test.ts: helpers the TypeScript left untested ---

    #[test]
    fn 배경색의_알파가_0이면_불투명으로_치지_않는다() {
        assert!(!is_opaque_background(""));
        assert!(!is_opaque_background("transparent"));
        assert!(!is_opaque_background("rgba(0, 0, 0, 0)"));
        assert!(is_opaque_background("rgba(0, 0, 0, 0.4)"));
        assert!(is_opaque_background("rgb(20, 20, 20)"));
        // Anything the pattern does not read counts as opaque.
        assert!(is_opaque_background("oklch(0.5 0.1 20)"));
        assert!(is_opaque_background("rgb()"));
        // parseFloat reads the numeric prefix; an alpha that is not a number is NaN.
        assert!(is_opaque_background("rgba(0, 0, 0, 50%)"));
        assert!(!is_opaque_background("rgba(0, 0, 0, none)"));
    }

    #[test]
    fn parse_float는_숫자로_시작하는_앞부분만_읽는다() {
        assert_eq!(parse_float(" 0.5"), 0.5);
        assert_eq!(parse_float(".25px"), 0.25);
        assert_eq!(parse_float("-3e2x"), -300.0);
        assert_eq!(parse_float("7."), 7.0);
        assert_eq!(parse_float("1e"), 1.0);
        assert!(parse_float("abc").is_nan());
        assert!(parse_float(".").is_nan());
        assert!(parse_float("").is_nan());
    }

    #[test]
    fn 배율을_주면_모든_사각형을_css_픽셀로_나눈다() {
        let mut snap = with_rects(S_AUTO, 540.0, 180.0);
        normalize_snapshot_scale(&mut snap, 2.0);
        let layout = &snap.documents[0].layout;

        assert_eq!(layout.bounds[0], [0.0, 0.0, 200.0, 45.0]);
        assert_eq!(layout.scroll_rects.as_ref().unwrap()[0][3], 270.0);
        assert_eq!(layout.client_rects.as_ref().unwrap()[0][3], 90.0);

        let scaled = capture_page_layout(with_rects(S_AUTO, 540.0, 180.0), VIEWPORT, None, 2.0);
        assert_eq!(scaled.unwrap().get(901).unwrap().scrollable_by, 180.0);
    }

    #[test]
    fn 스캔_응답이_없으면_건너뛴_것으로_본다() {
        assert!(scan_clickable(None).skipped);

        let scan = scan_clickable(Some(ClickScanPayload {
            paths: vec!["0.1".to_owned(), "0.1".to_owned(), "0.2".to_owned()],
            scanned: 40.0,
            skipped: false,
        }));
        assert_eq!(scan.paths, paths(&["0.1", "0.2"]));
        assert_eq!(scan.scanned, 3);
        assert!(!scan.skipped);
    }

    #[test]
    fn 부모_표가_순환해도_멈추지_않고_끝난다() {
        // Nodes 1 and 2 name each other as parent; node 3 points outside the table.
        let snap = capture(json!({
            "strings": ["auto"],
            "documents": [{
                "nodes": {
                    "backendNodeId": [900, 901, 902, 903],
                    "parentIndex": [-1, 2, 1, 99],
                    "nodeType": [9, 1, 1, 1]
                },
                "layout": {
                    "nodeIndex": [1, 2, 3, -4, 70],
                    "bounds": [[0, 0, 10, 10], [0, 0, 10, 10], [0, 0, 10, 10], [0, 0, 1, 1], [0, 0]],
                    "styles": [[-1, -1, 0, 0], [-1, -1, 0, 0], [5, 9, -2]]
                }
            }]
        }));

        let layout = build_page_layout(&snap, VIEWPORT, Some(&paths(&["0"]))).unwrap();

        assert!(layout.get(901).unwrap().in_viewport);
        assert!(layout.get(903).unwrap().in_viewport);
        assert_eq!(layout.iter().count(), 3);
    }
}
