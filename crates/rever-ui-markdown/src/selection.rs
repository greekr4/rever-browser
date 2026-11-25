//! Drag-to-select for markdown text.
//!
//! GPUI has no selection for static text: only input fields can select. The
//! markdown element keeps one itself: every text block records where its
//! [`TextLayout`] landed during paint, the root element turns a mouse drag
//! into a character range across those blocks, an overlay paints the
//! highlight, and a small bubble copies the selected text.
//!
//! One selection exists per app at a time, like any text selection: a drag
//! anywhere replaces the previous one. Code blocks are excluded; each already
//! carries its own copy button.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher as _;

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Element, ElementId, FocusHandle, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, SharedString, TextLayout, Window, px,
};

/// Drags shorter than this stay clicks: links keep working when the pointer
/// only wiggles.
const DRAG_THRESHOLD: f32 = 3.0;
/// Selection color, at the opacity the app's inactive text uses.
fn highlight_hsla() -> gpui::Hsla {
    gpui::hsla(220.0 / 360.0, 0.65, 0.55, 0.25)
}

/// Where a mouse position landed: which text block, and which byte in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    ordinal: usize,
    index: usize,
}

/// One laid-out text block, as recorded during its paint.
#[derive(Clone)]
struct HitEntry {
    ordinal: usize,
    bounds: Bounds<Pixels>,
    layout: TextLayout,
    text: SharedString,
    line_height: Pixels,
}

/// The selection of one markdown element instance.
#[derive(Clone, Copy, Debug)]
struct Active {
    owner: u64,
    /// Where the mouse went down; the anchor once the drag activated.
    down: Hit,
    /// The moving end of the selection.
    head: Hit,
    /// The pixel position of `down`, for the drag threshold.
    down_point: P32,
    /// Whether the drag passed the threshold and selected anything.
    live: bool,
}

/// A Point whose fields are plain f32s, so comparisons and distance math read
/// without unit ceremony.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct P32 {
    x: f32,
    y: f32,
}

impl From<Point<Pixels>> for P32 {
    fn from(p: Point<Pixels>) -> Self {
        Self {
            x: f32::from(p.x),
            y: f32::from(p.y),
        }
    }
}

impl P32 {
    fn distance(self, other: Self) -> f32 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}

/// App-global selection state. `RefCell` because element paint and mouse
/// handlers both reach it while `App` is only `&mut` borrowed once.
#[derive(Default)]
pub(crate) struct SelectionState {
    hits: RefCell<HashMap<u64, Vec<HitEntry>>>,
    active: RefCell<Option<Active>>,
    /// Each owner's keyboard focus: a drag moves focus to the markdown so a
    /// contextual Cmd-C binding reaches it.
    focus: RefCell<HashMap<u64, FocusHandle>>,
}

// GPUI draws on one thread; a thread-local sidesteps both `Send` bounds and
// registration plumbing a real app global would need.
thread_local! {
    static STATE: SelectionState = SelectionState::default();
}

/// Run `f` with the selection state. One borrow at a time keeps the RefCells
/// inside conflict-free.
fn state<R>(f: impl FnOnce(&SelectionState) -> R) -> R {
    STATE.with(f)
}

/// The keyboard focus of `owner`, created on first use.
pub(crate) fn focus_handle(cx: &App, owner: u64) -> FocusHandle {
    if let Some(handle) = state(|s| s.focus.borrow().get(&owner).cloned()) {
        return handle;
    }
    let handle = cx.focus_handle();
    state(|s| s.focus.borrow_mut().insert(owner, handle.clone()));
    handle
}

/// The instance key of a markdown root id.
pub(crate) fn key(id: &ElementId) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(format!("{id:?}").as_bytes());
    hasher.finish()
}

/// Drops the previous frame's hit entries for `owner`. The marker element at
/// the top of the markdown tree calls this before any block paints.
pub(crate) fn clear_owner(owner: u64) {
    state(|s| s.hits.borrow_mut().remove(&owner));
}

/// A text block's layout, recorded from its paint.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record(
    owner: u64,
    ordinal: usize,
    bounds: Bounds<Pixels>,
    layout: TextLayout,
    text: SharedString,
    line_height: Pixels,
) {
    state(|s| {
        s.hits
            .borrow_mut()
            .entry(owner)
            .or_default()
            .push(HitEntry {
                ordinal,
                bounds,
                layout,
                text,
                line_height,
            })
    });
}

/// Which block and byte `position` lands on inside `owner`.
pub(crate) fn hit_test(owner: u64, position: Point<Pixels>) -> Option<Hit> {
    state(|s| {
        let hits = s.hits.borrow();
        let entries = hits.get(&owner)?;
        let mut best: Option<(f32, usize)> = None;
        for (at, entry) in entries.iter().enumerate() {
            if !entry.bounds.contains(&position) {
                continue;
            }
            let distance = distance_to(entry.bounds, position);
            if best.is_none_or(|(so_far, _)| distance < so_far) {
                best = Some((distance, at));
            }
        }
        let at = best.map(|(_, at)| at)?;
        let entry = &entries[at];
        let index = entry
            .layout
            .index_for_position(position)
            .unwrap_or(entry.text.len());
        Some(Hit {
            ordinal: entry.ordinal,
            index: index.min(entry.text.len()),
        })
    })
}

fn distance_to(bounds: Bounds<Pixels>, position: Point<Pixels>) -> f32 {
    let dx = if position.x < bounds.left() {
        f32::from(bounds.left() - position.x)
    } else if position.x > bounds.right() {
        f32::from(position.x - bounds.right())
    } else {
        0.0
    };
    let dy = if position.y < bounds.top() {
        f32::from(bounds.top() - position.y)
    } else if position.y > bounds.bottom() {
        f32::from(position.y - bounds.bottom())
    } else {
        0.0
    };
    dx * dx + dy * dy
}

/// A drag starts here. Below the threshold nothing is selected yet, so a
/// plain click still reaches links and the copy button. A press on no text
/// at all clears whatever was selected.
pub(crate) fn mouse_down(owner: u64, event: &MouseDownEvent) -> bool {
    let Some(hit) = hit_test(owner, event.position) else {
        clear(owner);
        return false;
    };
    state(|s| {
        *s.active.borrow_mut() = Some(Active {
            owner,
            down: hit,
            head: hit,
            down_point: event.position.into(),
            live: false,
        })
    });
    true
}

/// The drag moves: past the threshold the selection goes live and the head
/// follows. `true` when something on screen changed.
pub(crate) fn mouse_move(owner: u64, event: &MouseMoveEvent) -> bool {
    // Decide under a shared borrow first: the hit test re-enters the state
    // and cannot run while a mutable borrow is held.
    let should = state(|s| {
        s.active.borrow().as_ref().is_some_and(|a| {
            a.owner == owner
                && event.dragging()
                && (a.live || P32::from(event.position).distance(a.down_point) >= DRAG_THRESHOLD)
        })
    });
    if !should {
        return false;
    }
    let hit = hit_test(owner, event.position);
    state(|s| {
        let mut active = s.active.borrow_mut();
        let Some(active) = active.as_mut() else {
            return false;
        };
        if active.owner != owner {
            return false;
        }
        active.live = true;
        if let Some(hit) = hit {
            active.head = hit;
        }
        true
    })
}

/// The drag ends. A selection that never went live was a click: forget it.
/// `true` when a live selection finished.
pub(crate) fn mouse_up(_owner: u64, _event: &MouseUpEvent) -> bool {
    state(|s| s.active.borrow().as_ref().is_some_and(|a| a.live))
}

/// The selected byte range of `owner`'s block `ordinal`, if it is selected.
fn block_range(active: &Active, owner: u64, ordinal: usize) -> Option<(usize, usize)> {
    if active.owner != owner {
        return None;
    }
    let (lo, hi) = order(active.down, active.head);
    if !active.live || lo == hi {
        return None;
    }
    (lo.ordinal <= ordinal && ordinal <= hi.ordinal).then(|| {
        let start = if lo.ordinal == ordinal { lo.index } else { 0 };
        let end = if hi.ordinal == ordinal {
            hi.index
        } else {
            usize::MAX
        };
        (start, end)
    })
}

fn order(a: Hit, b: Hit) -> (Hit, Hit) {
    if (a.ordinal, a.index) <= (b.ordinal, b.index) {
        (a, b)
    } else {
        (b, a)
    }
}

/// The highlight rectangles of the active selection in `owner`, in window
/// space, and where the copy bubble sits relative to `origin`.
pub(crate) fn highlight(owner: u64) -> Vec<Bounds<Pixels>> {
    let active = state(|s| s.active.borrow().clone());
    let Some(active) = active else {
        return Vec::new();
    };
    let entries = state(|s| s.hits.borrow().get(&owner).cloned());
    let Some(entries) = entries else {
        return Vec::new();
    };
    let mut rects = Vec::new();
    for entry in entries {
        let Some((start, end)) = block_range(&active, owner, entry.ordinal) else {
            continue;
        };
        let end = end.min(entry.text.len());
        if start >= end {
            continue;
        }
        rects.extend(line_rects(&entry, start, end));
    }
    rects
}

/// One entry's selection as whole-line rectangles: sample the caret positions
/// of every char boundary in the range and group them by row.
fn line_rects(entry: &HitEntry, start: usize, end: usize) -> Vec<Bounds<Pixels>> {
    let text = &entry.text[..];
    // Rows sit on the block's line grid: same wrapped line means same grid
    // slot, so sub-pixel y wobble between runs cannot split a row.
    let grid = f32::from(entry.line_height).max(1.0);
    let mut rows: Vec<(i32, f32, f32, f32)> = Vec::new(); // slot, y, min_x, max_x
    let mut at = start;
    while at <= end {
        if let Some(point) = entry.layout.position_for_index(at) {
            let (x, y) = (f32::from(point.x), f32::from(point.y));
            let slot = (y / grid).round() as i32;
            let row = rows.last_mut();
            match row {
                Some((row_slot, row_y, min_x, max_x)) if *row_slot == slot => {
                    *min_x = min_x.min(x);
                    *max_x = max_x.max(x);
                }
                _ => rows.push((slot, y, x, x)),
            }
        }
        if at >= end {
            break;
        }
        at = next_boundary(text, at, end);
    }
    rows.into_iter()
        .map(|(_slot, y, min_x, max_x)| Bounds {
            origin: Point {
                x: px(min_x),
                y: px(y),
            },
            size: gpui::size(px((max_x - min_x).max(2.0)), entry.line_height),
        })
        .collect()
}

fn next_boundary(text: &str, at: usize, end: usize) -> usize {
    let mut next = at + 1;
    while next < end && !text.is_char_boundary(next) {
        next += 1;
    }
    next
}

/// The selected text of `owner`, blocks joined the way they were laid out.
pub(crate) fn selected_text(owner: u64) -> Option<String> {
    let active = state(|s| s.active.borrow().clone())?;
    if !active.live {
        return None;
    }
    let entries = state(|s| s.hits.borrow().get(&owner).cloned())?;
    let mut out = String::new();
    let mut any = false;
    for entry in entries {
        let Some((start, end)) = block_range(&active, owner, entry.ordinal) else {
            continue;
        };
        let end = end.min(entry.text.len());
        if start >= end {
            continue;
        }
        if any {
            out.push_str("\n\n");
        }
        out.push_str(&entry.text[start..end]);
        any = true;
    }
    any.then_some(out)
}

/// Copy the selection to the clipboard and clear it.
pub(crate) fn copy_and_clear(cx: &mut App, owner: u64) -> bool {
    let copied = selected_text(owner).inspect(|text| {
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
    });
    clear(owner);
    copied.is_some()
}

/// Forget the selection of `owner`.
pub(crate) fn clear(owner: u64) {
    state(|s| {
        let mut active = s.active.borrow_mut();
        if active.as_ref().is_some_and(|a| a.owner == owner) {
            *active = None;
        }
    });
}

/// Whether `owner` has a finished selection worth a copy bubble.
pub(crate) fn has_selection(owner: u64) -> bool {
    state(|s| {
        s.active
            .borrow()
            .as_ref()
            .is_some_and(|a| a.owner == owner && a.live)
    })
}

/// The overlay color.
pub(crate) fn highlight_color() -> gpui::Hsla {
    highlight_hsla()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(ordinal: usize, index: usize) -> Hit {
        Hit { ordinal, index }
    }

    #[test]
    fn p32_거리는_피타고라스를_따른다() {
        let a = P32 { x: 0.0, y: 0.0 };
        let b = P32 { x: 3.0, y: 4.0 };
        assert_eq!(a.distance(b), 5.0);
    }

    #[test]
    fn 역방향_드래그는_순서를_바꿔_정규화한다() {
        let (lo, hi) = order(hit(2, 10), hit(1, 4));
        assert_eq!((lo.ordinal, lo.index), (1, 4));
        assert_eq!((hi.ordinal, hi.index), (2, 10));
    }

    #[test]
    fn 다음_문자_경계는_유니코드를_건너뛴다() {
        let text = "a한b";
        // '한' is three bytes: boundaries at 0, 1, 4, 5.
        assert_eq!(next_boundary(text, 0, 5), 1);
        assert_eq!(next_boundary(text, 1, 5), 4);
        assert_eq!(next_boundary(text, 4, 5), 5);
    }
}

/// Wraps one text block: delegates everything to the inner element, and on
/// prepaint records where the block's [`TextLayout`] landed so the markdown
/// root can hit-test mouse positions against it.
pub(crate) struct SelectableText {
    owner: u64,
    ordinal: usize,
    line_height: Pixels,
    layout: TextLayout,
    text: SharedString,
    inner: AnyElement,
}

impl SelectableText {
    /// `layout` must be cloned from the [`TextLayout`] the inner
    /// [`gpui::StyledText`] will be prepainted with: the clone shares its
    /// state, so it fills up when the inner element measures.
    pub(crate) fn new(
        owner: u64,
        ordinal: usize,
        line_height: Pixels,
        layout: TextLayout,
        text: SharedString,
        inner: AnyElement,
    ) -> Self {
        Self {
            owner,
            ordinal,
            line_height,
            layout,
            text,
            inner,
        }
    }
}

impl Element for SelectableText {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.inner.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.inner.prepaint(window, cx);
        let _ = cx;
        record(
            self.owner,
            self.ordinal,
            bounds,
            self.layout.clone(),
            self.text.clone(),
            self.line_height,
        );
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.inner.paint(window, cx);
    }
}

impl IntoElement for SelectableText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[cfg(test)]
pub(crate) fn debug_state(owner: u64) -> String {
    state(|s| {
        let hits = s.hits.borrow();
        format!(
            "hits for owner: {} entries; active: {:?}",
            hits.get(&owner).map_or(0, |v| v.len()),
            s.active
                .borrow()
                .as_ref()
                .map(|a| (a.owner, a.live, a.down, a.head))
        )
    })
}

#[cfg(test)]
pub(crate) fn focus_handle_pub(owner: &u64) -> FocusHandle {
    state(|s| s.focus.borrow().get(owner).cloned()).expect("focus handle exists")
}
