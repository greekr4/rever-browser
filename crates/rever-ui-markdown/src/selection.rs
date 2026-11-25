//! Drag-to-select for markdown text.
//!
//! GPUI has no selection for static text: only input fields can select. The
//! markdown element keeps one itself: every text block records where its
//! [`TextLayout`] landed during paint, the root element turns a mouse drag
//! into a character range across those blocks, and an overlay paints the
//! highlight. Cmd-C copies it.
//!
//! One selection exists per app at a time, like any text selection: a drag
//! anywhere replaces the previous one.

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

/// Where a mouse position landed: which text block, and which byte in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
    /// What is visible of the block: the scroll boxes it paints inside. A code
    /// line lays out past its box and only its start shows.
    clip: Bounds<Pixels>,
}

/// The selection of one markdown element instance.
#[derive(Clone, Copy, Debug)]
struct Active {
    owner: u64,
    /// The glyph the press landed on: its first and last byte boundary. Both
    /// are the same hit when the press fell in blank space.
    pressed: (Hit, Hit),
    /// The moving end of the selection.
    head: Hit,
    /// The pixel position of the press, for the drag threshold.
    down_point: P32,
    /// Whether the drag passed the threshold and selected anything.
    live: bool,
}

/// The fixed end of the selection: the glyph the press landed on stays
/// selected whichever way the drag went.
fn anchor(active: &Active) -> Hit {
    let (start, end) = active.pressed;
    if active.head <= start { end } else { start }
}

/// A Point whose fields are plain f32s, so distance math reads without unit
/// ceremony.
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

/// The selection state. `RefCell`s because element paint and mouse handlers
/// both reach it while `App` is only borrowed once.
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
/// the top of the markdown tree calls this before any block records.
pub(crate) fn clear_owner(owner: u64) {
    state(|s| s.hits.borrow_mut().remove(&owner));
}

/// A text block's layout, recorded from its paint.
pub(crate) fn record(
    owner: u64,
    ordinal: usize,
    bounds: Bounds<Pixels>,
    layout: TextLayout,
    text: SharedString,
    clip: Bounds<Pixels>,
) {
    state(|s| {
        s.hits
            .borrow_mut()
            .entry(owner)
            .or_default()
            .push(HitEntry {
                ordinal,
                bounds,
                line_height: layout.line_height(),
                layout,
                text,
                clip,
            })
    });
}

/// The block and byte `position` lands on inside `owner`. The flag is false
/// when the position is in the blank space before or after a line's text:
/// no glyph sits there.
fn locate(owner: u64, position: Point<Pixels>) -> Option<(Hit, bool)> {
    state(|s| {
        let hits = s.hits.borrow();
        let entries = hits.get(&owner)?;
        let entry = entries
            .iter()
            .find(|entry| entry.bounds.contains(&position) && entry.clip.contains(&position))?;
        let hit = |index: usize| Hit {
            ordinal: entry.ordinal,
            index: index.min(entry.text.len()),
        };
        match entry.layout.index_for_position(position) {
            Ok(index) => Some((closest_boundary(entry, position, index), true)),
            Err(index) => Some((hit(index), false)),
        }
    })
}

/// Which block and byte `position` lands on inside `owner`.
pub(crate) fn hit_test(owner: u64, position: Point<Pixels>) -> Option<Hit> {
    locate(owner, position).map(|(hit, _)| hit)
}

/// `index` is where the glyph under `position` starts. The boundary after
/// that glyph is the closer one once the pointer is past its middle.
fn closest_boundary(entry: &HitEntry, position: Point<Pixels>, index: usize) -> Hit {
    let next = next_boundary(&entry.text, index, entry.text.len());
    let closer = match (
        entry.layout.position_for_index(index),
        entry.layout.position_for_index(next),
    ) {
        (Some(a), Some(b)) if a.y == b.y => {
            f32::from(b.x - position.x).abs() < f32::from(position.x - a.x).abs()
        }
        _ => false,
    };
    Hit {
        ordinal: entry.ordinal,
        index: if closer { next } else { index },
    }
}

/// A drag starts here. Below the threshold nothing is selected yet, so a
/// plain click still reaches links and the copy button. A press on no text
/// at all clears whatever was selected.
pub(crate) fn mouse_down(owner: u64, event: &MouseDownEvent) -> bool {
    let Some((hit, on_glyph)) = locate(owner, event.position) else {
        clear(owner);
        return false;
    };
    let pressed = if on_glyph {
        (hit, glyph_end(owner, hit))
    } else {
        (hit, hit)
    };
    state(|s| {
        *s.active.borrow_mut() = Some(Active {
            owner,
            pressed,
            head: hit,
            down_point: event.position.into(),
            live: false,
        })
    });
    true
}

/// The boundary after the glyph that `hit` starts.
fn glyph_end(owner: u64, hit: Hit) -> Hit {
    let end = state(|s| {
        let hits = s.hits.borrow();
        let entry = hits
            .get(&owner)?
            .iter()
            .find(|entry| entry.ordinal == hit.ordinal)?;
        Some(next_boundary(&entry.text, hit.index, entry.text.len()))
    });
    Hit {
        index: end.unwrap_or(hit.index),
        ..hit
    }
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
    if active.owner != owner || !active.live {
        return None;
    }
    let (lo, hi) = (
        anchor(active).min(active.head),
        anchor(active).max(active.head),
    );
    if lo == hi {
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

/// The highlight rectangles of the active selection in `owner`, in window
/// space.
pub(crate) fn highlight(owner: u64) -> Vec<Bounds<Pixels>> {
    let active = state(|s| *s.active.borrow());
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
        // The highlight overlay paints outside the block's scroll boxes: it
        // clips to what they show.
        rects.extend(
            line_rects(&entry, start, end)
                .into_iter()
                .map(|rect| rect.intersect(&entry.clip))
                .filter(|rect| rect.size.width > px(0.0) && rect.size.height > px(0.0)),
        );
    }
    rects
}

/// One block's selection as one rectangle per visual line. A rectangle runs
/// from the caret before a line's first selected glyph to the caret after its
/// last.
fn line_rects(entry: &HitEntry, start: usize, end: usize) -> Vec<Bounds<Pixels>> {
    let mut rects: Vec<Bounds<Pixels>> = Vec::new();
    let mut from = start;
    while from < end {
        let to = next_boundary(&entry.text, from, end);
        if let (Some(a), Some(b)) = (
            entry.layout.position_for_index(from),
            entry.layout.position_for_index(to),
        ) {
            // The caret at a wrap boundary sits at the end of the line above.
            // So a step that changes line opens the next one at the block's
            // left edge.
            let left = if a.y == b.y { a.x } else { entry.bounds.left() };
            match rects.last_mut().filter(|rect| rect.top() == b.y) {
                Some(rect) => rect.size.width = b.x - rect.origin.x,
                None if b.x > left => rects.push(Bounds {
                    origin: Point { x: left, y: b.y },
                    size: gpui::size(b.x - left, entry.line_height),
                }),
                None => {}
            }
        }
        from = to;
    }
    rects
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
    let active = state(|s| *s.active.borrow())?;
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

/// The highlight color.
pub(crate) fn highlight_color() -> gpui::Hsla {
    gpui::hsla(220.0 / 360.0, 0.65, 0.55, 0.25)
}

/// Wraps one text block: delegates everything to the inner element, and on
/// prepaint records where the block's [`TextLayout`] landed so the markdown
/// root can hit-test mouse positions against it.
pub(crate) struct SelectableText {
    owner: u64,
    ordinal: usize,
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
        layout: TextLayout,
        text: SharedString,
        inner: AnyElement,
    ) -> Self {
        Self {
            owner,
            ordinal,
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
        record(
            self.owner,
            self.ordinal,
            bounds,
            self.layout.clone(),
            self.text.clone(),
            window.content_mask().bounds,
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

/// Caret positions of every char boundary in `owner`'s first text block:
/// tests drive drags with them instead of guessing glyph widths.
#[cfg(test)]
pub(crate) fn carets(owner: u64) -> Vec<Point<Pixels>> {
    state(|s| {
        let hits = s.hits.borrow();
        let entry = hits.get(&owner)?.first()?;
        Some(
            (0..=entry.text.len())
                .filter(|at| entry.text.is_char_boundary(*at))
                .filter_map(|at| entry.layout.position_for_index(at))
                .collect(),
        )
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(ordinal: usize, index: usize) -> Hit {
        Hit { ordinal, index }
    }

    /// A selection whose press landed on the glyph spanning bytes 9 to 12.
    fn active(head: Hit) -> Active {
        Active {
            owner: 1,
            pressed: (hit(0, 9), hit(0, 12)),
            head,
            down_point: P32::default(),
            live: true,
        }
    }

    #[test]
    fn 왼쪽으로_드래그하면_누른_글자의_끝이_고정점이다() {
        assert_eq!(anchor(&active(hit(0, 0))), hit(0, 12));
    }

    #[test]
    fn 오른쪽으로_드래그하면_누른_글자의_시작이_고정점이다() {
        assert_eq!(anchor(&active(hit(0, 30))), hit(0, 9));
        assert_eq!(anchor(&active(hit(1, 0))), hit(0, 9));
    }

    #[test]
    fn p32_거리는_피타고라스를_따른다() {
        let a = P32 { x: 0.0, y: 0.0 };
        let b = P32 { x: 3.0, y: 4.0 };
        assert_eq!(a.distance(b), 5.0);
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
