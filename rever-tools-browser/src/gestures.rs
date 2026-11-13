//! The gestures the tools are built from: click, hover, type, draw, drag, history
//! moves and the settle wait.
//!
//! In the TypeScript these are the tail of snapshot.ts (clickRef, typeRef,
//! dragRef, …) and `waitForSettle` of chrome-cdp.ts, written against the Chrome
//! DevTools Protocol. Here they are composed from what the port already has:
//! `rever_snapshot::Snapshotter` finds the element and its point, and
//! [`HumanInput`] moves, presses and types through `Browser::input`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rever_core::traffic::ListFilter;
use rever_snapshot::snapshot::{RefEntry, RefPoint, Snapshotter, point_for_selector};
use rever_tools_api::tool::{ToolContext, unsupported};
use serde_json::Value;
use tokio::time::Instant;

use crate::cdp_eval::fire;
use crate::human_input::{HumanInput, Mouse, Point};
use crate::{js_str, lock, now_ms, sleep_ms, utf16_len, utf16_prefix};

pub(crate) fn snapshotter(ctx: &ToolContext) -> Arc<Snapshotter> {
    ctx.state::<Snapshotter>()
}

fn human(ctx: &ToolContext) -> Arc<HumanInput> {
    ctx.state::<HumanInput>()
}

/// The element behind a ref, as a page expression. The snapshot script keeps the
/// elements of the current snapshot in `window.__reverSnapshot.refs`; the few
/// things it has no action for (the overlay flash, the drag fallbacks) read them
/// from there.
fn ref_element(r: &str) -> String {
    format!(
        "(window.__reverSnapshot && window.__reverSnapshot.refs.get({}))",
        js_str(r)
    )
}

fn selector_element(selector: &str) -> String {
    format!("document.querySelector({})", js_str(selector))
}

fn entry(ctx: &ToolContext, r: &str) -> Result<RefEntry, String> {
    snapshotter(ctx)
        .ref_entry(r)
        .ok_or_else(|| format!("unknown ref \"{r}\" — call browser_snapshot first"))
}

/// `AI click "Save"`: the action and the start of the element's name.
fn named(action: &str, entry: &RefEntry, max: usize) -> String {
    if entry.name.is_empty() {
        action.to_owned()
    } else {
        format!("{action} \"{}\"", utf16_prefix(&entry.name, max))
    }
}

fn short(selector: &str, max: usize) -> String {
    if utf16_len(selector) > max {
        format!("{}…", utf16_prefix(selector, max))
    } else {
        selector.to_owned()
    }
}

/// The shared start of every pointer gesture: thinking pause → cursor move →
/// flash. The overlay flash appears only once the cursor has arrived.
async fn approach(
    ctx: &ToolContext,
    point: RefPoint,
    element: &str,
    label: &str,
    kind: &str,
) -> Result<(), String> {
    let human = human(ctx);
    human.thinking_pause().await;
    human.mouse_move(&ctx.browser, point.x, point.y).await?;
    fire(
        &ctx.browser,
        format!(
            "(() => {{ const el = {element}; if (el && window.__reverAi) window.__reverAi.flashElement(el, {}, '{kind}') }})()",
            js_str(label)
        ),
    );
    Ok(())
}

async fn click_at(
    ctx: &ToolContext,
    point: RefPoint,
    element: &str,
    label: &str,
    mouse: Mouse<'_>,
) -> Result<(), String> {
    approach(ctx, point, element, label, "click").await?;
    human(ctx)
        .press_release(&ctx.browser, point.x, point.y, mouse)
        .await
}

pub(crate) async fn click_ref(ctx: &ToolContext, r: &str) -> Result<(), String> {
    let label = named("AI click", &entry(ctx, r)?, 32);
    let point = snapshotter(ctx)
        .point_for_ref(ctx.browser.as_ref(), r)
        .await?;
    click_at(ctx, point, &ref_element(r), &label, Mouse::default()).await
}

/// Right-click, middle-click or double-click a ref.
pub(crate) async fn click_ref_with(
    ctx: &ToolContext,
    r: &str,
    mouse: Mouse<'_>,
) -> Result<(), String> {
    let entry = entry(ctx, r)?;
    // Refused before the page is scrolled and the cursor moved for a press that
    // cannot be sent.
    if !mouse.is_plain() {
        return Err(unsupported(
            "a press with another mouse button or a click count",
        ));
    }
    let kind = if mouse.click_count == Some(2.0) {
        "double-click".to_owned()
    } else {
        format!("{}-click", mouse.button.unwrap_or("left"))
    };
    let label = named(&format!("AI {kind}"), &entry, 28);
    let point = snapshotter(ctx)
        .point_for_ref(ctx.browser.as_ref(), r)
        .await?;
    click_at(ctx, point, &ref_element(r), &label, mouse).await
}

/// Same sequence as a click WITHOUT the press/release — the cursor just arrives
/// and stays, so :hover styles / mouseover-driven menus stay open.
pub(crate) async fn hover_ref(ctx: &ToolContext, r: &str) -> Result<(), String> {
    let label = named("AI hover", &entry(ctx, r)?, 32);
    let point = snapshotter(ctx)
        .point_for_ref(ctx.browser.as_ref(), r)
        .await?;
    approach(ctx, point, &ref_element(r), &label, "hover").await
}

/// Same human-shaped sequence as a click, then focus + type (+ optional Enter).
pub(crate) async fn type_ref(
    ctx: &ToolContext,
    r: &str,
    text: &str,
    submit: bool,
) -> Result<(), String> {
    let label = named("AI type →", &entry(ctx, r)?, 24);
    let snapshotter = snapshotter(ctx);
    let point = snapshotter.point_for_ref(ctx.browser.as_ref(), r).await?;
    approach(ctx, point, &ref_element(r), &label, "type").await?;
    let human = human(ctx);
    human
        .press_release(&ctx.browser, point.x, point.y, Mouse::default())
        .await?;
    // Focus the target element. Required so the key events land in it.
    snapshotter.focus_ref(ctx.browser.as_ref(), r).await?;
    human.type_text(&ctx.browser, text, submit).await
}

pub(crate) async fn hover_selector(ctx: &ToolContext, selector: &str) -> Result<(), String> {
    let point = point_for_selector(ctx.browser.as_ref(), selector).await?;
    let label = format!("AI hover \"{}\"", short(selector, 32));
    approach(ctx, point, &selector_element(selector), &label, "hover").await
}

pub(crate) async fn click_selector(ctx: &ToolContext, selector: &str) -> Result<(), String> {
    let point = point_for_selector(ctx.browser.as_ref(), selector).await?;
    let label = format!("AI click \"{}\"", short(selector, 32));
    let element = selector_element(selector);
    click_at(ctx, point, &element, &label, Mouse::default()).await
}

pub(crate) async fn type_selector(
    ctx: &ToolContext,
    selector: &str,
    text: &str,
    submit: bool,
) -> Result<(), String> {
    let point = point_for_selector(ctx.browser.as_ref(), selector).await?;
    let label = format!("AI type → \"{}\"", short(selector, 24));
    let element = selector_element(selector);
    approach(ctx, point, &element, &label, "type").await?;
    let human = human(ctx);
    human
        .press_release(&ctx.browser, point.x, point.y, Mouse::default())
        .await?;
    let focus = format!("(() => {{ const el = {element}; if (el && el.focus) el.focus() }})()");
    ctx.browser.evaluate(&focus).await?;
    human.type_text(&ctx.browser, text, submit).await
}

/// Press, move and release inside one element — a canvas stroke, a signature
/// pad, a colour picker. `drag_ref` goes between two elements and cannot express
/// a gesture that starts and ends on the same surface.
///
/// Offsets are fractions of the element's box, so they survive a resize.
pub(crate) async fn draw_on_ref(
    ctx: &ToolContext,
    r: &str,
    from: Point,
    to: Point,
) -> Result<(), String> {
    let bounds = snapshotter(ctx)
        .point_for_ref(ctx.browser.as_ref(), r)
        .await?;
    // `Math.min(1, Math.max(0, f))`; a NaN fraction stays NaN there as here.
    let pt = |f: Point| Point {
        x: bounds.left + bounds.width * f.x.clamp(0.0, 1.0),
        y: bounds.top + bounds.height * f.y.clamp(0.0, 1.0),
    };
    let human = human(ctx);
    human.thinking_pause().await;
    human.drag(&ctx.browser, pt(from), pt(to)).await
}

/// A cheap fingerprint of the drop target, used to tell whether a drag that
/// dispatched cleanly actually changed anything. A gesture no library listened
/// to leaves this identical.
async fn drop_signature(ctx: &ToolContext, r: &str) -> Result<Value, String> {
    let expression = format!(
        "(() => {{ const el = {}; return el ? el.className + \"|\" + el.childElementCount + \"|\" + (el.textContent || \"\").slice(0, 200) : null }})()",
        ref_element(r)
    );
    ctx.browser.evaluate(&expression).await
}

/// Dispatch a full HTML5 drag sequence with a shared DataTransfer.
///
/// Last resort, and deliberately explicit about it: the handlers run and the
/// DataTransfer carries whatever dragstart put in it, but the events are not
/// trusted input, so a page that checks isTrusted will not be fooled.
async fn synthesise_drag(ctx: &ToolContext, from_ref: &str, to_ref: &str) -> Result<(), String> {
    let expression = format!(
        "(() => {{
  const from = {};
  const drop = {};
  if (!from || !drop) return 'gone';
  const dt = new DataTransfer();
  const at = (el) => {{
    const r = el.getBoundingClientRect();
    return {{ clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 }};
  }};
  const fire = (el, type, pos) => {{
    const ev = new DragEvent(type, {{ bubbles: true, cancelable: true, dataTransfer: dt, ...pos }});
    el.dispatchEvent(ev);
    return ev;
  }};
  fire(from, 'dragstart', at(from));
  fire(drop, 'dragenter', at(drop));
  fire(drop, 'dragover', at(drop));
  fire(drop, 'drop', at(drop));
  fire(from, 'dragend', at(drop));
  return 'ok';
}})()",
        ref_element(from_ref),
        ref_element(to_ref)
    );
    if ctx.browser.evaluate(&expression).await? == "gone" {
        return Err(
            "a dragged ref is no longer on the page — call browser_snapshot again".to_owned(),
        );
    }
    Ok(())
}

/// Drag one ref onto another. Returns which mechanism actually ran, because
/// the two reach different libraries and a drag that changed nothing is
/// otherwise indistinguishable from one that worked.
pub(crate) async fn drag_ref(
    ctx: &ToolContext,
    from_ref: &str,
    to_ref: &str,
) -> Result<&'static str, String> {
    let snapshotter = snapshotter(ctx);
    let point = async |r: &str| -> Result<Point, String> {
        let point = snapshotter.point_for_ref(ctx.browser.as_ref(), r).await?;
        Ok(Point {
            x: point.x,
            y: point.y,
        })
    };
    // Two passes. The first may scroll either element into view, which moves the
    // other one — measuring source then target once gives a source coordinate
    // that the target's scroll has already invalidated. By the second pass both
    // are on screen, so neither scrolls and the pair is consistent.
    point(from_ref).await?;
    point(to_ref).await?;
    let from = point(from_ref).await?;
    let to = point(to_ref).await?;
    let human = human(ctx);
    human.thinking_pause().await;
    // Try the browser's own drag machinery first: a draggable element takes over
    // on mousedown and mouse events after that are invisible to the page. Fall
    // back to the held-button gesture, which is what pointer-based sortables
    // (dnd-kit, SortableJS in pointer mode) listen for.
    if human.native_drag(&ctx.browser, from, to).await? {
        return Ok("native");
    }

    let before = drop_signature(ctx, to_ref).await?;
    human.drag(&ctx.browser, from, to).await?;
    if drop_signature(ctx, to_ref).await? != before {
        return Ok("pointer");
    }

    // Neither real-input path reached the page. That happens with HTML5
    // draggable elements, where the browser takes the gesture over on mousedown
    // and the mouse events that follow are invisible to the document. Synthesise
    // the drag events as a last resort — the DataTransfer is real and the
    // handlers run, but the events carry isTrusted=false, so the caller is told
    // which path ran rather than being left to assume the drag was genuine.
    synthesise_drag(ctx, from_ref, to_ref).await?;
    Ok("synthetic")
}

/// Attach files to a file input.
///
/// A file input cannot be typed into — the browser owns its value, and clicking
/// it opens a native picker no tool can answer. The TypeScript set the files
/// with `DOM.setFileInputFiles`; `Browser` has no way to hand files to an
/// input, so after the checks that need no page this reports it as unsupported.
pub(crate) async fn upload_to_ref(
    ctx: &ToolContext,
    r: &str,
    paths: &[&str],
) -> Result<usize, String> {
    entry(ctx, r)?;
    let missing: Vec<&str> = paths
        .iter()
        .copied()
        .filter(|path| !Path::new(path).exists())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "file(s) not found: {} — pass absolute paths",
            missing.join(", ")
        ));
    }
    Err(unsupported("attaching files to a file input"))
}

/// The page's address, or nothing when it cannot be read.
pub(crate) async fn page_url(ctx: &ToolContext) -> String {
    match ctx.browser.evaluate("location.href").await {
        Ok(Value::String(url)) => url,
        _ => String::new(),
    }
}

/// Move back or forward in the tab's own history.
///
/// Navigating to the previous URL is not the same thing: it pushes a new entry,
/// so the page sees a fresh load rather than a restore, and anything that keys
/// off history length or popstate behaves differently.
///
/// The TypeScript read the entry list (`Page.getNavigationHistory`) to refuse a
/// move past either end and to name the destination. A page only knows how long
/// its history is, not where in it it stands, so the one case refused here is a
/// history of a single entry; the caller reads the address once the move settled.
pub(crate) async fn navigate_history(ctx: &ToolContext, direction: &str) -> Result<(), String> {
    let length = ctx.browser.evaluate("history.length").await.ok();
    if length.and_then(|length| length.as_f64()) == Some(1.0) {
        return Err(format!("nothing to go {direction} to — at entry 1 of 1"));
    }
    match direction {
        "back" => ctx.browser.back().await,
        _ => ctx.browser.forward().await,
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Settle {
    pub idle_ms: u64,
    pub timeout_ms: u64,
    pub min_wait_ms: u64,
}

impl Default for Settle {
    fn default() -> Self {
        Self {
            idle_ms: 500,
            timeout_ms: 1500,
            min_wait_ms: 250,
        }
    }
}

/// A request older than this that never finished is a stream left open, not
/// something the page is still loading.
const IN_FLIGHT_LOOKBACK_MS: u64 = 10_000;

/// Wait until the tab's network has been quiet for `idle_ms` or `timeout_ms`
/// elapses. Cheap alternative to Playwright's waitForLoadState.
///
/// The TypeScript counted in-flight requests from debugger events. Here the
/// count is read off the traffic store: a recent request without a completion
/// time is still in flight.
pub(crate) async fn wait_for_settle(ctx: &ToolContext, opts: Settle) {
    sleep_ms(opts.min_wait_ms as f64).await;
    let filter = ListFilter {
        since: Some(now_ms().saturating_sub(IN_FLIGHT_LOOKBACK_MS)),
        limit: Some(500),
        ..ListFilter::default()
    };
    let in_flight = || {
        let traffic = lock(&ctx.traffic);
        let recent = traffic.list(&filter);
        recent.iter().any(|request| request.completed_at.is_none())
    };
    let start = Instant::now();
    let mut quiet_since = (!in_flight()).then(Instant::now);
    while start.elapsed() < Duration::from_millis(opts.timeout_ms) {
        if in_flight() {
            quiet_since = None;
        } else {
            let quiet = *quiet_since.get_or_insert_with(Instant::now);
            if quiet.elapsed() >= Duration::from_millis(opts.idle_ms) {
                return;
            }
        }
        sleep_ms(60.0).await;
    }
}

#[cfg(test)]
mod tests {
    use rever_core::traffic::RequestPatch;
    use rever_snapshot::snapshot::SnapshotOptions;
    use rever_tools_api::tool::Input;
    use serde_json::json;

    use super::*;
    use crate::test_support::{FakeBrowser, context, settle_overlay};

    /// Take the snapshot that hands the button `r1`, and forget the calls it made.
    async fn snapshot(browser: &FakeBrowser, ctx: &ToolContext) {
        snapshotter(ctx)
            .take_snapshot(ctx.browser.as_ref(), SnapshotOptions::default())
            .await
            .unwrap();
        browser.calls.lock().unwrap().clear();
    }

    fn presses(inputs: &[Input]) -> Vec<Input> {
        inputs
            .iter()
            .filter(|input| !matches!(input, Input::MouseMove { .. }))
            .cloned()
            .collect()
    }

    const POINT_R1: &str = "script:(\"act\", {\"ref\":\"r1\",\"action\":\"point\",\"value\":null})";

    #[tokio::test(start_paused = true)]
    async fn 클릭은_요소로_커서를_옮긴_뒤_그_가운데를_누른다() {
        let (browser, ctx) = context(FakeBrowser::new());
        snapshot(&browser, &ctx).await;

        click_ref(&ctx, "r1").await.unwrap();

        assert_eq!(browser.calls(), [POINT_R1]);
        let inputs = browser.inputs();
        assert!(matches!(inputs[0], Input::MouseMove { .. }));
        assert_eq!(
            presses(&inputs),
            [
                Input::MouseDown { x: 100.0, y: 50.0 },
                Input::MouseUp { x: 100.0, y: 50.0 }
            ]
        );
        settle_overlay().await;
        assert!(browser.overlay().contains(
            &"(() => { const el = (window.__reverSnapshot && window.__reverSnapshot.refs.get(\"r1\")); if (el && window.__reverAi) window.__reverAi.flashElement(el, \"AI click \\\"Save\\\"\", 'click') })()".to_owned()
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn 모르는_ref는_페이지를_건드리지_않고_거절한다() {
        let (browser, ctx) = context(FakeBrowser::new());
        let unknown = Err("unknown ref \"r9\" — call browser_snapshot first".to_owned());
        assert_eq!(click_ref(&ctx, "r9").await, unknown);
        assert_eq!(hover_ref(&ctx, "r9").await, unknown);
        assert_eq!(type_ref(&ctx, "r9", "x", false).await, unknown);
        assert_eq!(click_ref_with(&ctx, "r9", Mouse::default()).await, unknown);
        assert!(browser.calls().is_empty() && browser.inputs().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn 페이지에서_사라진_ref는_페이지가_알려준_오류를_돌려준다() {
        let gone = "ref \"r1\" is no longer on the page — call browser_snapshot again";
        let (browser, ctx) = context(FakeBrowser::new().with_actions([json!({ "error": gone })]));
        snapshot(&browser, &ctx).await;
        assert_eq!(click_ref(&ctx, "r1").await, Err(gone.to_owned()));
        assert!(browser.inputs().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn 호버는_누르지_않고_커서만_옮긴다() {
        let (browser, ctx) = context(FakeBrowser::new());
        snapshot(&browser, &ctx).await;
        hover_ref(&ctx, "r1").await.unwrap();
        let inputs = browser.inputs();
        assert!(!inputs.is_empty() && presses(&inputs).is_empty());
        settle_overlay().await;
        assert!(
            browser
                .overlay()
                .iter()
                .any(|call| call.contains("\"AI hover \\\"Save\\\"\", 'hover'"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 입력은_클릭하고_포커스를_준_뒤_글자를_친다() {
        let (browser, ctx) = context(FakeBrowser::new());
        snapshot(&browser, &ctx).await;
        type_ref(&ctx, "r1", "hi", true).await.unwrap();
        assert_eq!(
            browser.calls(),
            [
                POINT_R1,
                "script:(\"act\", {\"ref\":\"r1\",\"action\":\"focus\",\"value\":null})"
            ]
        );
        assert_eq!(
            presses(&browser.inputs()),
            [
                Input::MouseDown { x: 100.0, y: 50.0 },
                Input::MouseUp { x: 100.0, y: 50.0 },
                Input::Text {
                    text: "h".to_owned()
                },
                Input::Text {
                    text: "i".to_owned()
                },
                Input::Key {
                    key: "Enter".to_owned()
                },
            ]
        );
        settle_overlay().await;
        assert!(
            browser
                .overlay()
                .iter()
                .any(|call| call.contains("\"AI type → \\\"Save\\\"\", 'type'"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 다른_버튼의_클릭은_커서를_옮기기_전에_거절한다() {
        let (browser, ctx) = context(FakeBrowser::new());
        snapshot(&browser, &ctx).await;
        let right = Mouse {
            button: Some("right"),
            click_count: None,
        };
        assert_eq!(
            click_ref_with(&ctx, "r1", right).await,
            Err(unsupported(
                "a press with another mouse button or a click count"
            ))
        );
        assert!(browser.calls().is_empty() && browser.inputs().is_empty());

        // The default button and count is the plain click, with its own label.
        click_ref_with(&ctx, "r1", Mouse::default()).await.unwrap();
        assert_eq!(presses(&browser.inputs()).len(), 2);
        settle_overlay().await;
        assert!(
            browser
                .overlay()
                .iter()
                .any(|call| call.contains("\"AI left-click \\\"Save\\\"\", 'click'"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 선택자로도_같은_순서로_클릭하고_긴_선택자는_라벨에서_줄인다() {
        let (browser, ctx) = context(FakeBrowser::new());
        let selector = "div.results > ul.list > li.item:nth-child(3) > a";
        click_selector(&ctx, selector).await.unwrap();
        assert_eq!(
            browser.calls(),
            [format!(
                "script:(\"act\", {{\"selector\":\"{selector}\",\"action\":\"point\",\"value\":null}})"
            )]
        );
        assert_eq!(presses(&browser.inputs()).len(), 2);
        settle_overlay().await;
        assert!(browser.overlay().iter().any(|call| {
            call.contains("\"AI click \\\"div.results > ul.list > li.item:…\\\"\", 'click'")
        }));

        let (browser, ctx) = context(FakeBrowser::new());
        hover_selector(&ctx, "#q").await.unwrap();
        assert!(presses(&browser.inputs()).is_empty());
        settle_overlay().await;
        assert!(
            browser
                .overlay()
                .iter()
                .any(|call| call.contains("\"AI hover \\\"#q\\\"\", 'hover'"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 맞는_요소가_없는_선택자는_페이지의_오류를_돌려준다() {
        let (browser, ctx) = context(
            FakeBrowser::new()
                .with_actions([json!({ "error": "no element matches selector: #x" })]),
        );
        assert_eq!(
            click_selector(&ctx, "#x").await,
            Err("no element matches selector: #x".to_owned())
        );
        assert!(browser.inputs().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn 선택자_입력은_요소에_포커스를_주고_친다() {
        let (browser, ctx) = context(FakeBrowser::new());
        type_selector(&ctx, "#q", "a", false).await.unwrap();
        assert_eq!(
            browser.calls()[1],
            "evaluate:(() => { const el = document.querySelector(\"#q\"); if (el && el.focus) el.focus() })()"
        );
        assert_eq!(
            presses(&browser.inputs()).last(),
            Some(&Input::Text {
                text: "a".to_owned()
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 그리기는_요소_상자의_비율을_좌표로_바꾸고_범위_밖은_가장자리로_당긴다() {
        let (browser, ctx) = context(FakeBrowser::new());
        snapshot(&browser, &ctx).await;
        // The box the fake reports: left 80, top 40, 40 wide, 20 high.
        let (from, to) = (Point { x: 0.25, y: 0.5 }, Point { x: 2.0, y: -1.0 });
        draw_on_ref(&ctx, "r1", from, to).await.unwrap();
        assert_eq!(
            presses(&browser.inputs()),
            [
                Input::MouseDown { x: 90.0, y: 50.0 },
                Input::MouseUp { x: 120.0, y: 40.0 }
            ]
        );
    }

    fn two_buttons() -> FakeBrowser {
        let browser = FakeBrowser::new();
        *browser.capture.lock().unwrap() = Some(json!({
            "meta": { "url": "https://a.test/", "title": "A" },
            "nodes": [
                { "nodeId": "1", "role": { "value": "RootWebArea" }, "childIds": ["2", "3"] },
                { "nodeId": "2", "role": { "value": "button" }, "name": { "value": "Card" }, "backendDOMNodeId": 10 },
                { "nodeId": "3", "role": { "value": "button" }, "name": { "value": "Bin" }, "backendDOMNodeId": 11 }
            ]
        }));
        browser
    }

    #[tokio::test(start_paused = true)]
    async fn 드래그는_대상이_바뀌면_포인터_드래그로_끝난다() {
        let (browser, ctx) = context(two_buttons());
        snapshot(&browser, &ctx).await;
        *browser.evaluations.lock().unwrap() =
            [Ok(json!("bin|0|")), Ok(json!("bin|1|Card"))].into();

        assert_eq!(drag_ref(&ctx, "r1", "r2").await, Ok("pointer"));

        let calls = browser.calls();
        // Both elements are measured twice before anything moves.
        let points: Vec<&String> = calls.iter().filter(|c| c.starts_with("script:")).collect();
        assert_eq!(points.len(), 4);
        assert!(points[0].contains("\"r1\"") && points[1].contains("\"r2\""));
        assert!(points[2].contains("\"r1\"") && points[3].contains("\"r2\""));
        assert!(calls[4].contains("window.__reverSnapshot.refs.get(\"r2\")"));
        assert!(calls[4].contains("el.childElementCount"));
        assert_eq!(calls.len(), 6);
        assert_eq!(presses(&browser.inputs()).len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn 대상이_그대로면_드래그_이벤트를_만들어_보내고_그렇게_알린다() {
        let (browser, ctx) = context(two_buttons());
        snapshot(&browser, &ctx).await;
        *browser.evaluations.lock().unwrap() =
            [Ok(json!("bin|0|")), Ok(json!("bin|0|")), Ok(json!("ok"))].into();

        assert_eq!(drag_ref(&ctx, "r1", "r2").await, Ok("synthetic"));
        let calls = browser.calls();
        let last = calls.last().unwrap();
        assert!(last.contains(
            "const from = (window.__reverSnapshot && window.__reverSnapshot.refs.get(\"r1\"));"
        ));
        assert!(last.contains("fire(drop, 'drop', at(drop));"));

        // The elements went away between the gesture and the fallback.
        *browser.evaluations.lock().unwrap() =
            [Ok(json!(null)), Ok(json!(null)), Ok(json!("gone"))].into();
        assert_eq!(
            drag_ref(&ctx, "r1", "r2").await,
            Err("a dragged ref is no longer on the page — call browser_snapshot again".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 파일_첨부는_ref와_파일을_확인한_뒤_지원하지_않는다고_알린다() {
        let (browser, ctx) = context(FakeBrowser::new());
        assert_eq!(
            upload_to_ref(&ctx, "r1", &[]).await,
            Err("unknown ref \"r1\" — call browser_snapshot first".to_owned())
        );
        snapshot(&browser, &ctx).await;

        let here = std::env::current_exe().unwrap();
        let here = here.to_str().unwrap();
        assert_eq!(
            upload_to_ref(&ctx, "r1", &["/no/such/a.png", here, "/no/such/b.png"]).await,
            Err(
                "file(s) not found: /no/such/a.png, /no/such/b.png — pass absolute paths"
                    .to_owned()
            )
        );
        assert_eq!(
            upload_to_ref(&ctx, "r1", &[here]).await,
            Err(unsupported("attaching files to a file input"))
        );
        assert!(browser.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn 히스토리_이동은_항목이_하나뿐이면_거절하고_아니면_방향대로_움직인다() {
        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!(1))]));
        assert_eq!(
            navigate_history(&ctx, "back").await,
            Err("nothing to go back to — at entry 1 of 1".to_owned())
        );
        assert_eq!(browser.calls(), ["evaluate:history.length"]);

        let (browser, ctx) = context(FakeBrowser::answering([
            Ok(json!(3)),
            // An unreadable length does not stop the move.
            Err("blocked".to_owned()),
        ]));
        navigate_history(&ctx, "back").await.unwrap();
        navigate_history(&ctx, "forward").await.unwrap();
        assert_eq!(
            browser.calls(),
            [
                "evaluate:history.length",
                "back",
                "evaluate:history.length",
                "forward"
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 주소는_페이지에_물어_읽고_못_읽으면_빈_문자열이다() {
        let (_, ctx) = context(FakeBrowser::answering([
            Ok(json!("https://a.test/x")),
            Ok(json!(null)),
            Err("gone".to_owned()),
        ]));
        assert_eq!(page_url(&ctx).await, "https://a.test/x");
        assert_eq!(page_url(&ctx).await, "");
        assert_eq!(page_url(&ctx).await, "");
    }

    fn request(ctx: &ToolContext, id: &str, started_at: u64, completed_at: Option<u64>) {
        let patch = RequestPatch {
            request_id: id.to_owned(),
            url: Some("https://a.test/api".to_owned()),
            started_at: Some(started_at),
            completed_at,
            ..RequestPatch::default()
        };
        lock(&ctx.traffic).upsert(patch, started_at);
    }

    #[tokio::test(start_paused = true)]
    async fn 네트워크가_조용하면_최소_대기와_유휴_시간만_기다린다() {
        let (_, ctx) = context(FakeBrowser::new());
        // Finished, and one left open long ago: neither counts as in flight.
        request(&ctx, "done", now_ms(), Some(now_ms()));
        request(&ctx, "stream", now_ms() - 60_000, None);

        let started = Instant::now();
        wait_for_settle(&ctx, Settle::default()).await;
        // 250ms, then quiet for 500ms seen at a 60ms poll.
        assert_eq!(started.elapsed().as_millis(), 250 + 540);

        let started = Instant::now();
        let opts = Settle {
            idle_ms: 300,
            timeout_ms: 3000,
            min_wait_ms: 250,
        };
        wait_for_settle(&ctx, opts).await;
        assert_eq!(started.elapsed().as_millis(), 250 + 300);
    }

    #[tokio::test(start_paused = true)]
    async fn 끝나지_않은_요청이_있으면_제한_시간까지만_기다린다() {
        let (_, ctx) = context(FakeBrowser::new());
        request(&ctx, "pending", now_ms(), None);
        let started = Instant::now();
        wait_for_settle(&ctx, Settle::default()).await;
        // 250ms, then 25 polls of 60ms reach the 1500ms limit.
        assert_eq!(started.elapsed().as_millis(), 250 + 1500);
    }
}
