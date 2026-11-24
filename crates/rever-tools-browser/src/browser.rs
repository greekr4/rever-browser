//! Port of `src/main/mcp/tools/browser.ts` — the tools that drive the tab:
//! navigate, snapshot, click, hover, type, scroll, drag, keys, evaluate, extract.
//!
//! Every tool is registered with its schema and checks. What the TypeScript did
//! through Electron or the Chrome DevTools Protocol with no `Browser`
//! counterpart reports itself as unsupported: addressing a tab by index, the tab
//! list, the clipboard, native dialogs, file inputs, the viewport mode, and
//! image results.

use std::sync::Mutex;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rever_analysis::triggered_requests::{ActionWindows, MAX_TRIGGERED, format_triggered_requests};
use rever_scope::scope::scope_block_for_url;
use rever_snapshot::snapshot::{SnapshotOptions, SnapshotStats};
use rever_tools_api::tool::{BoxFuture, TabInfo, Tool, ToolContext, ToolDef, ToolRegistry};
use rever_tools_api::utils::{Content, ResultStore, ToolResult, err, ok_image};
use serde_json::{Value, json};

use crate::cdp_eval::{eval_in_page, visualize};
use crate::gestures::{
    Settle, click_ref, click_ref_with, click_selector, drag_ref, draw_on_ref, hover_ref,
    hover_selector, navigate_history, page_url, snapshotter, type_ref, type_selector,
    upload_to_ref, wait_for_settle,
};
use crate::human_input::{HumanInput, Mouse, Point, named_keys};
use crate::markdown;
use crate::{
    js_number, js_round, js_space, js_str, lock, no_tab, now_ms, pretty, schema, tab_schema,
    utf16_len, utf16_prefix,
};

pub fn register(registry: &mut ToolRegistry) {
    registry.register(BrowserNavigate);
    registry.register(BrowserSnapshot);
    registry.register(BrowserClick);
    registry.register(BrowserHover);
    registry.register(BrowserType);
    registry.register(BrowserScrollElement);
    registry.register(BrowserDraw);
    registry.register(BrowserPaste);
    registry.register(BrowserTabs);
    registry.register(BrowserHandleDialog);
    registry.register(BrowserFileUpload);
    registry.register(BrowserHistory);
    registry.register(BrowserClickModified);
    registry.register(BrowserDrag);
    registry.register(BrowserPressKey);
    registry.register(BrowserSelectOption);
    registry.register(BrowserClickSelector);
    registry.register(BrowserHoverSelector);
    registry.register(BrowserTypeSelector);
    registry.register(BrowserScroll);
    registry.register(SetViewport);
    registry.register(BrowserEvaluate);
    registry.register(DomExtract);
    registry.register(BrowserScreenshot);
}

// One-line preview of an eval result / error for the in-page code HUD.
fn preview(text: &str) -> String {
    let mut flat = String::new();
    let mut in_space = false;
    for ch in text.chars() {
        if js_space(ch) {
            in_space = true;
            continue;
        }
        if in_space && !flat.is_empty() {
            flat.push(' ');
        }
        in_space = false;
        flat.push(ch);
    }
    if utf16_len(&flat) > 120 {
        format!("{}…", utf16_prefix(&flat, 120))
    } else {
        flat
    }
}

// Requests started within this window before snapshot_after is entered are
// still credited to the action — covers a fetch that fires on the same tick
// the action resolves, just before we read the clock.
// shortcut: correlates by a time window, not true causal linkage. Recurring
// background endpoints are filtered out; a one-off unrelated request in the
// same window still shows. Upgrade to initiator-stack correlation if that bites.
const ACTION_WINDOW_MS: u64 = 500;

/// Wait for the page to settle, then take a fresh snapshot. Used by
/// click/type/navigate to bundle the post-action page state into the same
/// tool response — saves the agent a separate browser_snapshot round-trip
/// and makes the previous snapshot's refs explicitly stale.
pub(crate) async fn snapshot_after(ctx: &ToolContext, action_result: &str) -> String {
    // Capture before the settle wait so requests the action kicks off *during*
    // settling are included; the lookback absorbs any that raced just ahead.
    let since = now_ms().saturating_sub(ACTION_WINDOW_MS);
    let outcome: Result<String, String> = async {
        wait_for_settle(ctx, Settle::default()).await;
        let snap = snapshotter(ctx)
            .take_snapshot(ctx.browser.as_ref(), SnapshotOptions::default())
            .await?;
        let triggered = {
            let traffic = lock(&ctx.traffic);
            let windows = ctx.state::<Mutex<ActionWindows>>();
            let mut windows = lock(&windows);
            // The store is shared by every tab, but `Browser` does not say which
            // one it drives, so requests are not narrowed to a tab.
            let triggered = windows.collect_triggered_requests(&traffic, since, None, now_ms());
            format_triggered_requests(&triggered, since, MAX_TRIGGERED)
        };
        Ok(format!(
            "{action_result}\n\n--- snapshot (refs from previous snapshot are now stale) ---\nurl: {}\ntitle: {}\n\n{}{}{triggered}",
            snap.url,
            snap.title,
            snap.tree,
            filter_note(&snap.stats)
        ))
    }
    .await;
    outcome.unwrap_or_else(|e| format!("{action_result}\n\n[auto-snapshot failed: {e}]"))
}

/// Trailing note telling the agent what the viewport filter left out.
fn filter_note(stats: &SnapshotStats) -> String {
    let mut parts: Vec<String> = Vec::new();
    if stats.hidden > 0 || stats.offscreen > 0 {
        parts.push(format!(
            "viewport filter: {} hidden/covered, {} off-screen actionable element(s) omitted — scroll to reach them, or pass full=true for the whole page",
            stats.hidden, stats.offscreen
        ));
    }
    // Reported even at zero: a silent scan is indistinguishable from a page that
    // genuinely has no role-less click targets, and the scanned/matched split is
    // what separates "scan found nothing" from "geometry failed to correlate".
    if stats.click_scan_skipped {
        parts.push(
            "click scan SKIPPED: the page has too many elements, so role-less click targets (a <div> with an onClick and no ARIA role) are missing from this outline entirely"
                .to_owned(),
        );
    }
    if stats.click_scanned > 0 || stats.click_only_refs > 0 {
        parts.push(format!(
            "click scan: {} candidate(s), {} correlated, {} new ref(s) tagged (click-scan)",
            stats.click_scanned, stats.click_matched, stats.click_only_refs
        ));
    }
    if stats.frames > 0 || stats.frames_unreachable > 0 {
        let mut bits = vec![format!("frames: {} entered", stats.frames)];
        if stats.frames_unreachable > 0 {
            bits.push(format!(
                "{} iframe(s) could not be entered (cross-site, runs in its own renderer — not yet supported)",
                stats.frames_unreachable
            ));
        }
        if stats.frames_empty > 0 {
            bits.push(format!(
                "{} entered but still empty (frame had not finished loading)",
                stats.frames_empty
            ));
        }
        if stats.frames > 0 {
            bits.push(
                "click scan does not reach inside frames, so framed elements need an ARIA role"
                    .to_owned(),
            );
        }
        parts.push(bits.join("; "));
    }
    if stats.new_refs > 0 {
        parts.push(format!(
            "{} ref(s) marked *new — absent from the previous snapshot of this page",
            stats.new_refs
        ));
    }
    if stats.fell_back_to_full {
        parts.push(
            "viewport filter DISABLED for this snapshot: it suppressed every element with nothing to attribute it to (likely a coordinate mismatch)"
                .to_owned(),
        );
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("\n\n[{}]", parts.join("]\n["))
    }
}

/// `okBudgeted`: the text, or its head with a pointer to the rest parked for
/// `tool_result_more`.
fn budgeted(ctx: &ToolContext, text: String) -> String {
    let store = ctx.state::<Mutex<ResultStore>>();
    let result = lock(&store).ok_budgeted(text, None);
    match result.content.into_iter().next() {
        Some(Content::Text { text }) => text,
        None => String::new(),
    }
}

/// A required argument: `{ type, description }`, the order zod lists them in.
fn req(kind: &str, description: &str) -> Value {
    json!({ "type": kind, "description": description })
}

/// An optional argument: zod lists the description first.
fn opt(kind: &str, description: &str) -> Value {
    json!({ "description": description, "type": kind })
}

fn strings(description: &str) -> Value {
    json!({ "type": "array", "items": { "type": "string" }, "description": description })
}

const TAB: &str = "Tab index from browser_tabs (default: the selected tab)";
const REF_TAB: &str = "Tab the ref belongs to (browser_tabs index; default: the selected tab)";

tool!(
    BrowserNavigate,
    ToolDef::new(
        "browser_navigate",
        "Navigate the active browser tab to the given URL. Waits for the load and returns a fresh accessibility snapshot — no separate browser_snapshot needed. Pass `tab` to drive a specific background tab (index from browser_tabs) without selecting it.",
        schema(
            json!({
                "url": req("string", "Absolute URL (must include scheme, e.g. https://...)"),
                "tab": tab_schema(TAB),
            }),
            &["url"]
        )
    ),
    |ctx, args| {
        let url = args.str("url");
        if let Some(block) = scope_block_for_url(Some(url)) {
            return Err(block);
        }
        no_tab(args)?;
        ctx.browser.navigate(url).await?;
        // The TypeScript swept as soon as the new document was parsed. `Browser`
        // has no such moment to hook, so the sweep is fired once the navigation
        // has been handed over; it is cosmetic either way.
        visualize(&ctx.browser, "navSweep", &[]);
        Ok(snapshot_after(ctx, &format!("navigated to {url}")).await)
    }
);

tool!(
    BrowserSnapshot,
    ToolDef::new(
        "browser_snapshot",
        "Capture the current page as a compact accessibility-tree snapshot. Returns url, title, and a YAML-like outline where actionable nodes carry [ref=rN] handles. Use these refs in browser_click and browser_type. This is the primary way to \"see\" the page — far cheaper than dumping HTML or screenshots. By default only what is painted inside the viewport is returned; hidden, overlay-covered and off-screen nodes are omitted and summarised as scroll hints. Scroll (browser_scroll) to reach them, or pass full=true when you genuinely need the whole document. Pass `tab` to snapshot a specific tab (index from browser_tabs) — refs are per-tab, so acting on them (browser_click/browser_type) needs the same `tab`. Pass markdown=true to also append the page body as Markdown under the tree: the cheap way to read what it says.",
        schema(
            json!({
                "full": opt(
                    "boolean",
                    "Return the entire document instead of the viewport-filtered tree (much larger; default false)"
                ),
                "markdown": opt(
                    "boolean",
                    "Also return the page body as Markdown text under the tree (reads the active tab; default false)"
                ),
                "tab": tab_schema(TAB),
            }),
            &[]
        )
    ),
    |ctx, args| {
        no_tab(args)?;
        let opts = SnapshotOptions {
            full: args.opt_bool("full").unwrap_or(false),
        };
        let snap = snapshotter(ctx)
            .take_snapshot(ctx.browser.as_ref(), opts)
            .await?;
        let mut text = format!(
            "url: {}\ntitle: {}\n\n{}{}",
            snap.url,
            snap.title,
            snap.tree,
            filter_note(&snap.stats)
        );
        if args.opt_bool("markdown").unwrap_or(false) {
            text.push_str(&markdown::markdown_section(ctx.browser.as_ref()).await);
        }
        Ok(budgeted(ctx, text))
    }
);

tool!(
    BrowserClick,
    ToolDef::new(
        "browser_click",
        "Click the element identified by ref (from the latest browser_snapshot). Scrolls into view, performs a human-like mouse move, then clicks. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards. Refs from the previous snapshot are now stale. Pass `tab` (browser_tabs index) when the ref came from a background tab's snapshot — refs are per-tab.",
        schema(
            json!({
                "ref": req("string", "Element ref from browser_snapshot, e.g. \"r12\""),
                "tab": tab_schema(REF_TAB),
            }),
            &["ref"]
        )
    ),
    |ctx, args| {
        no_tab(args)?;
        let r = args.str("ref");
        click_ref(ctx, r).await?;
        Ok(snapshot_after(ctx, &format!("clicked {r}")).await)
    }
);

tool!(
    BrowserHover,
    ToolDef::new(
        "browser_hover",
        "Hover over the element identified by ref (from the latest browser_snapshot). Scrolls into view and performs a human-like mouse move WITHOUT clicking — use it to open hover-triggered dropdown menus, tooltips and mega-navs. Returns a fresh snapshot so the hover-revealed content is visible. Refs from the previous snapshot are now stale. Pass `tab` (browser_tabs index) when the ref came from a background tab's snapshot — refs are per-tab.",
        schema(
            json!({
                "ref": req("string", "Element ref from browser_snapshot, e.g. \"r12\""),
                "tab": tab_schema(REF_TAB),
            }),
            &["ref"]
        )
    ),
    |ctx, args| {
        no_tab(args)?;
        let r = args.str("ref");
        hover_ref(ctx, r).await?;
        Ok(snapshot_after(ctx, &format!("hovered {r}")).await)
    }
);

tool!(
    BrowserType,
    ToolDef::new(
        "browser_type",
        "Move the cursor to the input/textarea identified by ref, click to focus, then type the text with realistic per-keystroke timing. Press Enter with submit=true (default false). Returns a fresh snapshot — DO NOT call browser_snapshot afterwards. Refs from the previous snapshot are now stale. Pass `tab` (browser_tabs index) when the ref came from a background tab's snapshot — refs are per-tab.",
        schema(
            json!({
                "ref": req("string", "Element ref from browser_snapshot, e.g. \"r7\""),
                "text": req("string", "Text to put into the field"),
                "submit": opt(
                    "boolean",
                    "If true, dispatches an Enter keydown after typing (default false)"
                ),
                "tab": tab_schema(REF_TAB),
            }),
            &["ref", "text"]
        )
    ),
    |ctx, args| {
        no_tab(args)?;
        let (r, submit) = (args.str("ref"), args.opt_bool("submit").unwrap_or(false));
        type_ref(ctx, r, args.str("text"), submit).await?;
        let suffix = if submit { " + submit" } else { "" };
        Ok(snapshot_after(ctx, &format!("typed into {r}{suffix}")).await)
    }
);

tool!(
    BrowserScrollElement,
    ToolDef::new(
        "browser_scroll_element",
        "Scroll a container identified by ref, rather than the window. browser_scroll moves the page, which does nothing for a list, drawer or chat log that scrolls inside its own box — those show as [scrollable +Npx] in the snapshot. Stepped, so lazy-loading handlers fire. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.",
        schema(
            json!({
                "ref": req("string", "Ref of the scroll container, e.g. \"r15\""),
                "deltaY": req("number", "Pixels to scroll; negative scrolls up"),
            }),
            &["ref", "deltaY"]
        )
    ),
    |ctx, args| {
        let r = args.str("ref");
        let delta_y = args.opt_number("deltaY").unwrap_or(0.0);
        let top = snapshotter(ctx)
            .scroll_ref_by(ctx.browser.as_ref(), r, delta_y)
            .await?;
        let action = format!("scrolled {r} to {}px", js_number(js_round(top)));
        Ok(snapshot_after(ctx, &action).await)
    }
);

fn point_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "properties": { "x": { "type": "number" }, "y": { "type": "number" } },
        "required": ["x", "y"],
        "description": description,
    })
}

fn point_arg(args: &crate::Args, key: &str) -> Point {
    let axis = |axis: &str| args.0[key][axis].as_f64().unwrap_or(0.0);
    Point {
        x: axis("x"),
        y: axis("y"),
    }
}

tool!(
    BrowserDraw,
    ToolDef::new(
        "browser_draw",
        "Press, move and release inside ONE element — a canvas stroke, a signature pad, a colour picker, a range track. browser_drag goes between two elements and cannot express a gesture that starts and ends on the same surface. Points are fractions of the element box (0..1), so 0.1,0.5 to 0.9,0.5 draws a horizontal line across the middle. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.",
        schema(
            json!({
                "ref": req("string", "Ref of the surface to draw on, e.g. \"r24\""),
                "from": point_schema(
                    "Start point as fractions of the element box, e.g. {\"x\":0.1,\"y\":0.5}"
                ),
                "to": point_schema("End point as fractions of the element box"),
            }),
            &["ref", "from", "to"]
        )
    ),
    |ctx, args| {
        let r = args.str("ref");
        draw_on_ref(ctx, r, point_arg(args, "from"), point_arg(args, "to")).await?;
        Ok(snapshot_after(ctx, &format!("drew on {r}")).await)
    }
);

tool!(
    BrowserPaste,
    ToolDef::new(
        "browser_paste",
        "Put text on the clipboard and paste it into the element identified by ref. A page that reads clipboardData on a paste event sees nothing when the text is typed instead — the two are different events. Overwrites the real clipboard. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.",
        schema(
            json!({
                "ref": req("string", "Ref of the field to paste into, e.g. \"r22\""),
                "text": req("string", "Text to place on the clipboard and paste"),
            }),
            &["ref", "text"]
        )
    ),
    |ctx, args| {
        let (r, text) = (args.str("ref"), args.str("text"));
        ctx.browser.write_clipboard(text).await?;
        snapshotter(ctx).focus_ref(ctx.browser.as_ref(), r).await?;
        // The paste itself is a key with the platform's accelerator held.
        let accelerator = if cfg!(target_os = "macos") {
            "Meta"
        } else {
            "Control"
        };
        ctx.state::<HumanInput>()
            .press_key(&ctx.browser, "v", &[accelerator], None)
            .await?;
        let pasted = format!("pasted {} char(s) into {r}", utf16_len(text));
        Ok(snapshot_after(ctx, &pasted).await)
    }
);

tool!(
    BrowserTabs,
    ToolDef::new(
        "browser_tabs",
        "List the open browser tabs, switch to one, or open a new background tab. A window.open popup becomes a tab, and its content is unreachable until the tools are pointed at it — every other browser tool acts on the selected tab only. Select by index from the list. open loads a URL in a new tab WITHOUT switching to it, so several same-site pages can load in parallel; read each with list_endpoints({tab}) / list_requests({tab}).",
        schema(
            json!({
                "action": {
                    "type": "string",
                    "enum": ["list", "select", "open"],
                    "description": "What to do",
                },
                "index": opt("number", "Tab index to select, from the list output"),
                "url": opt("string", "URL to load for action \"open\""),
            }),
            &["action"]
        )
    ),
    |ctx, args| {
        let rows = |tabs: &[TabInfo]| {
            let rows: Vec<String> = tabs
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let mark = if t.active { " *" } else { "  " };
                    let title = if t.title.is_empty() {
                        "(untitled)"
                    } else {
                        &t.title
                    };
                    format!("{i}{mark} {title} — {}", t.url)
                })
                .collect();
            rows.join("\n")
        };
        let action = args.str("action");
        if action == "open" {
            let url = args.opt_str("url").unwrap_or_default();
            if url.is_empty() {
                return Err("url required for action \"open\"".to_owned());
            }
            if let Some(blocked) = scope_block_for_url(Some(url)) {
                return Err(blocked);
            }
            let index = ctx.browser.open_tab(url).await?;
            let after = ctx.browser.tabs().await?;
            return Ok(format!(
                "opened background tab {index}: {url}\n{}\n\n* = selected. Read this tab with list_endpoints({{tab: {index}}}).",
                rows(&after)
            ));
        }
        let tabs = ctx.browser.tabs().await?;
        if action == "list" {
            if tabs.is_empty() {
                return Ok("no attached tabs".to_owned());
            }
            return Ok(format!("{}\n\n* = selected", rows(&tabs)));
        }
        let index = args.0.get("index").and_then(Value::as_f64);
        let Some(index) =
            index.filter(|i| i.fract() == 0.0 && *i >= 0.0 && (*i as usize) < tabs.len())
        else {
            return Err(format!("index must be 0..{}", tabs.len() as i64 - 1));
        };
        let index = index as usize;
        ctx.browser.select_tab(index).await?;
        Ok(snapshot_after(ctx, &format!("selected tab {index}: {}", tabs[index].url)).await)
    }
);

tool!(
    BrowserHandleDialog,
    ToolDef::new(
        "browser_handle_dialog",
        "Choose the answer for a page alert()/confirm()/prompt(). Dialogs that open while your tools run are answered automatically (OK / the prompt default) and listed at the end of that tool's result. Call this BEFORE the click to reach a \"Cancel\" branch or type a prompt value. One-shot: consumed by the first dialog.",
        schema(
            json!({
                "accept": req(
                    "boolean",
                    "true for OK, false for Cancel (a cancelled prompt returns null)"
                ),
                "promptText": opt(
                    "string",
                    "Value a prompt() should return. Ignored when accept is false."
                ),
            }),
            &["accept"]
        )
    ),
    |ctx, args| {
        let accept = args.opt_bool("accept").unwrap_or(false);
        let prompt_text = args.opt_str("promptText");
        ctx.browser
            .arm_dialog_answer(accept, prompt_text.map(str::to_owned))
            .await?;
        let what = match (accept, prompt_text) {
            (true, Some(text)) => format!("OK with \"{text}\""),
            (true, None) => "OK".to_owned(),
            (false, _) => "Cancel".to_owned(),
        };
        Ok(format!("next dialog will answer {what} — now trigger it"))
    }
);

tool!(
    BrowserFileUpload,
    ToolDef::new(
        "browser_file_upload",
        "Attach files to a file input identified by ref. A file input cannot be typed into — the browser owns its value — and clicking it opens a native picker no tool can answer, so this is the only way to fill one. The ref may point at the input or at its rendered button. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.",
        schema(
            json!({
                "ref": req("string", "Ref of the file input, or of its button, e.g. \"r7\""),
                "paths": strings(
                    "Absolute paths of the files to attach. Pass several only for a multiple input."
                ),
            }),
            &["ref", "paths"]
        )
    ),
    |ctx, args| {
        let r = args.str("ref");
        let paths = args.opt_strings("paths").unwrap_or_default();
        let attached = upload_to_ref(ctx, r, &paths).await?;
        Ok(snapshot_after(ctx, &format!("attached {attached} file(s) to {r}")).await)
    }
);

tool!(
    BrowserHistory,
    ToolDef::new(
        "browser_history",
        "Move back or forward in the tab's own history. browser_navigate to the previous URL is NOT the same thing: it pushes a new entry, so the page sees a fresh load rather than a restore and anything keyed off popstate or history length behaves differently. Errors when there is nothing to go to. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.",
        schema(
            json!({
                "direction": {
                    "type": "string",
                    "enum": ["back", "forward"],
                    "description": "Which way to move",
                },
            }),
            &["direction"]
        )
    ),
    |ctx, args| {
        let direction = args.str("direction");
        navigate_history(ctx, direction).await?;
        let settle = Settle {
            idle_ms: 300,
            timeout_ms: 3000,
            ..Settle::default()
        };
        wait_for_settle(ctx, settle).await;
        // The address the move arrived at; the TypeScript named the history entry
        // it was heading for.
        let url = page_url(ctx).await;
        Ok(snapshot_after(ctx, &format!("went {direction} to {url}")).await)
    }
);

tool!(
    BrowserClickModified,
    ToolDef::new(
        "browser_click_modified",
        "Click a ref with a non-default button or click count: right-click to open a context menu, or double-click. browser_click always sends a single left click, and two browser_click calls are seconds apart so the page never pairs them into a double-click. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.",
        schema(
            json!({
                "ref": req("string", "Element ref from browser_snapshot, e.g. \"r12\""),
                "button": {
                    "description": "Mouse button (default left)",
                    "type": "string",
                    "enum": ["left", "right", "middle"],
                },
                "clickCount": opt("number", "2 for a double-click (default 1)"),
            }),
            &["ref"]
        )
    ),
    |ctx, args| {
        let r = args.str("ref");
        let mouse = Mouse {
            button: args.opt_str("button"),
            click_count: args.opt_number("clickCount"),
        };
        click_ref_with(ctx, r, mouse).await?;
        let what = if mouse.click_count == Some(2.0) {
            "double-clicked".to_owned()
        } else {
            format!("{}-clicked", mouse.button.unwrap_or("left"))
        };
        Ok(snapshot_after(ctx, &format!("{what} {r}")).await)
    }
);

tool!(
    BrowserDrag,
    ToolDef::new(
        "browser_drag",
        "Drag one ref onto another. Tries the browser's own drag machinery first (HTML5 draggable), then falls back to a held-button pointer gesture, which is what dnd-kit / SortableJS listen for. A click on the source followed by a click on the target is two unrelated clicks and neither sees a drag. The result says which mechanism ran — CHECK THE PAGE ACTUALLY CHANGED, a drag that reached nothing looks identical to one that worked. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.",
        schema(
            json!({
                "from": req("string", "Ref of the element to drag, e.g. \"r6\""),
                "to": req("string", "Ref of the drop target, e.g. \"r9\""),
            }),
            &["from", "to"]
        )
    ),
    |ctx, args| {
        let (from, to) = (args.str("from"), args.str("to"));
        let how = drag_ref(ctx, from, to).await?;
        Ok(snapshot_after(ctx, &format!("dragged {from} onto {to} ({how} drag)")).await)
    }
);

tool!(
    BrowserPressKey,
    ToolDef::new(
        "browser_press_key",
        &format!(
            "Send a single key to the focused element — the only way to reach a keyboard-driven widget. Escape closes a dialog, ArrowDown moves a listbox or a slider, Tab moves focus, and Control+A then Delete clears a field (browser_type appends rather than replacing). Pass ref to focus that element first WITHOUT clicking it, which matters when a click would dismiss or mis-select. Accepts one character, or: {}. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.",
            named_keys().join(", ")
        ),
        schema(
            json!({
                "key": req(
                    "string",
                    "Key name (e.g. \"Escape\", \"ArrowDown\") or a single character"
                ),
                "ref": opt(
                    "string",
                    "Focus this element first, without clicking it. Omit to send to whatever has focus."
                ),
                "modifiers": {
                    "description": "Held while the key is pressed, e.g. [\"Control\"] for Control+A",
                    "type": "array",
                    "items": { "type": "string", "enum": ["Alt", "Control", "Meta", "Shift"] },
                },
                "repeat": opt("number", "Press this many times (default 1)"),
            }),
            &["key"]
        )
    ),
    |ctx, args| {
        let key = args.str("key");
        let modifiers = args.opt_strings("modifiers").unwrap_or_default();
        let repeat = args.opt_number("repeat");
        if let Some(r) = args.opt_str("ref").filter(|r| !r.is_empty()) {
            snapshotter(ctx).focus_ref(ctx.browser.as_ref(), r).await?;
        }
        ctx.state::<HumanInput>()
            .press_key(&ctx.browser, key, &modifiers, repeat)
            .await?;
        let times = match repeat {
            Some(repeat) if repeat > 1.0 => format!(" x{}", js_number(repeat)),
            _ => String::new(),
        };
        let mut combo = modifiers;
        combo.push(key);
        Ok(snapshot_after(ctx, &format!("pressed {}{times}", combo.join("+"))).await)
    }
);

tool!(
    BrowserSelectOption,
    ToolDef::new(
        "browser_select_option",
        "Choose option(s) in a <select> identified by ref (from the latest browser_snapshot). A native dropdown is drawn by the OS, so browser_click and browser_type cannot reach its options — this is the only way to answer one. Match by option value or by visible label; if nothing matches, the error lists the available options. Returns a fresh snapshot — DO NOT call browser_snapshot afterwards.",
        schema(
            json!({
                "ref": req(
                    "string",
                    "Element ref of the <select> from browser_snapshot, e.g. \"r20\""
                ),
                "values": strings(
                    "Option values or visible labels to select. Pass one for a normal select, several for a multiple one."
                ),
            }),
            &["ref", "values"]
        )
    ),
    |ctx, args| {
        let r = args.str("ref");
        let values: Vec<String> = args
            .opt_strings("values")
            .unwrap_or_default()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let picked = snapshotter(ctx)
            .select_ref(ctx.browser.as_ref(), r, &values)
            .await?;
        Ok(snapshot_after(ctx, &format!("selected {} in {r}", picked.join(", "))).await)
    }
);

tool!(
    BrowserClickSelector,
    ToolDef::new(
        "browser_click_selector",
        "Click the first element matching a CSS selector, using the SAME human-shaped cursor move + click as browser_click (visualised on the page, fires trusted events). Use this to interact when you located an element via dom_extract / browser_evaluate and have no snapshot ref — NEVER poke the DOM with raw JS (.click()/dispatchEvent) to interact, as that skips the cursor animation and trips bot detection. Returns a fresh snapshot.",
        schema(
            json!({
                "selector": req(
                    "string",
                    "CSS selector for the target, e.g. \"#q\" or \"button.search-btn\""
                ),
            }),
            &["selector"]
        )
    ),
    |ctx, args| {
        let selector = args.str("selector");
        click_selector(ctx, selector).await?;
        Ok(snapshot_after(ctx, &format!("clicked selector {selector}")).await)
    }
);

tool!(
    BrowserHoverSelector,
    ToolDef::new(
        "browser_hover_selector",
        "Hover over the first element matching a CSS selector, using the SAME human-shaped cursor move as browser_hover (no click). Use when you have a selector but no snapshot ref — NEVER fake hover with raw JS dispatchEvent, as that skips the cursor animation and fires untrusted events. Returns a fresh snapshot.",
        schema(
            json!({
                "selector": req(
                    "string",
                    "CSS selector for the target, e.g. \"nav .menu-item\""
                ),
            }),
            &["selector"]
        )
    ),
    |ctx, args| {
        let selector = args.str("selector");
        hover_selector(ctx, selector).await?;
        Ok(snapshot_after(ctx, &format!("hovered selector {selector}")).await)
    }
);

tool!(
    BrowserTypeSelector,
    ToolDef::new(
        "browser_type_selector",
        "Type into the first element matching a CSS selector, using the SAME human-shaped focus + per-keystroke timing as browser_type (visualised, fires real trusted key events that survive bot detection). Use when you have a selector but no snapshot ref. Set submit=true to press Enter after typing. Returns a fresh snapshot — do NOT set the value with raw JS.",
        schema(
            json!({
                "selector": req("string", "CSS selector for the input/textarea, e.g. \"#q\""),
                "text": req("string", "Text to type into the field"),
                "submit": opt("boolean", "If true, presses Enter after typing (default false)"),
            }),
            &["selector", "text"]
        )
    ),
    |ctx, args| {
        let selector = args.str("selector");
        let submit = args.opt_bool("submit").unwrap_or(false);
        type_selector(ctx, selector, args.str("text"), submit).await?;
        let suffix = if submit { " + submit" } else { "" };
        Ok(snapshot_after(ctx, &format!("typed into selector {selector}{suffix}")).await)
    }
);

tool!(
    BrowserScroll,
    ToolDef::new(
        "browser_scroll",
        "Scroll the page. Provide either absolute y or relative deltaY (in pixels). Useful for infinite-scroll sites.",
        schema(
            json!({
                "y": opt("number", "Absolute scroll position in px from top"),
                "deltaY": opt("number", "Relative scroll delta in px (positive = down)"),
            }),
            &[]
        )
    ),
    |ctx, args| {
        let delta_y = args.opt_number("deltaY").unwrap_or(0.0);
        let scroll_y = ctx
            .state::<HumanInput>()
            .scroll(&ctx.browser, delta_y, args.opt_number("y"))
            .await?;
        Ok(format!("scrolled, scrollY={}", js_number(scroll_y)))
    }
);

tool!(
    SetViewport,
    ToolDef::new(
        "set_viewport",
        "Switch the active browser tab between desktop and mobile mode (changes user-agent + device metrics) and reloads the page. Use mobile mode to access mobile-only sites or APIs.",
        schema(
            json!({
                "mode": {
                    "type": "string",
                    "enum": ["desktop", "mobile"],
                    "description": "Target viewport mode",
                },
            }),
            &["mode"]
        )
    ),
    |ctx, args| {
        let mode = args.str("mode");
        ctx.browser.set_viewport_mode(mode == "mobile").await?;
        Ok(format!("viewport set to {mode}"))
    }
);

tool!(
    BrowserEvaluate,
    ToolDef::new(
        "browser_evaluate",
        "Run a JavaScript expression in the active page and return the result. The expression must return a JSON-serialisable value. Pass `tab` (browser_tabs index) to run it in a background tab without selecting it.",
        schema(
            json!({
                "expression": req(
                    "string",
                    "JavaScript expression to evaluate (must return a value)"
                ),
                "tab": tab_schema(TAB),
            }),
            &["expression"]
        )
    ),
    |ctx, args| {
        let browser = crate::tab_browser(ctx, args)?;
        let expression = args.str("expression");
        visualize(
            &browser,
            "evalHudStart",
            &[json!(utf16_prefix(expression, 160))],
        );
        let text = match eval_in_page(browser.as_ref(), expression).await? {
            Value::String(text) => text,
            other => pretty(&other),
        };
        visualize(
            &browser,
            "evalHudDone",
            &[json!(preview(&text)), json!(false)],
        );
        Ok(text)
    }
);

tool!(
    DomExtract,
    ToolDef::new(
        "dom_extract",
        "Extract structured data from the rendered DOM by CSS selector. For each matching element, pulls the requested fields (text/href/src/value/html) plus any named attributes. This is the primary way to scrape results from server-rendered (SSR) pages — no JSON API required. Prefer this over ad-hoc browser_evaluate for pulling lists (search results, tables, cards).",
        schema(
            json!({
                "selector": req(
                    "string",
                    "CSS selector, e.g. \"a.result__title\" or \"ul.list > li\""
                ),
                "fields": {
                    "description": "Built-in fields to pull per node. Default ['text'].",
                    "type": "array",
                    "items": { "type": "string", "enum": ["text", "href", "src", "value", "html"] },
                },
                "attrs": {
                    "description": "Extra attribute names to read, e.g. [\"data-id\",\"aria-label\"]",
                    "type": "array",
                    "items": { "type": "string" },
                },
                "limit": {
                    "description": "Max nodes to return (default 50). Guards against huge dumps.",
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 500,
                },
            }),
            &["selector"]
        )
    ),
    |ctx, args| {
        let selector = args.str("selector");
        let fields = args
            .opt_strings("fields")
            .filter(|fields| !fields.is_empty());
        let fields = fields.unwrap_or_else(|| vec!["text"]);
        let attrs = args.opt_strings("attrs").unwrap_or_default();
        let cap = args.opt_number("limit").unwrap_or(50.0);
        const PER_NODE_CHARS: usize = 500;
        // Build an in-page expression with all params embedded as JSON literals so
        // the selector/attribute names can't break out of the string.
        let expr = format!(
            "(() => {{
  const selector = {};
  const fields = {};
  const attrs = {};
  const limit = {};
  const CAP = {PER_NODE_CHARS};
  const clip = (s) => s.length > CAP ? s.slice(0, CAP) + '…' : s;
  const nodes = Array.from(document.querySelectorAll(selector));
  const matched = nodes.length;
  const items = nodes.slice(0, limit).map((el, index) => {{
    const out = {{ index }};
    if (fields.includes('text')) out.text = clip((el.innerText || el.textContent || '').trim());
    if (fields.includes('href')) {{ const h = el.getAttribute('href'); if (h != null) out.href = el.href || h; }}
    if (fields.includes('src')) {{ const s = el.getAttribute('src'); if (s != null) out.src = el.src || s; }}
    if (fields.includes('value') && 'value' in el) out.value = el.value;
    if (fields.includes('html')) out.html = clip(el.innerHTML || '');
    if (attrs.length) {{
      const a = {{}};
      for (const name of attrs) {{ const v = el.getAttribute(name); if (v != null) a[name] = v; }}
      if (Object.keys(a).length) out.attrs = a;
    }}
    return out;
  }});
  return {{ matched, count: items.length, truncated: matched > items.length, items }};
}})()",
            js_str(selector),
            json!(fields),
            json!(attrs),
            js_number(cap)
        );
        let result = eval_in_page(ctx.browser.as_ref(), &expr).await?;
        if !result.is_object() {
            return Err("the page returned no result for the extraction".to_owned());
        }
        visualize(
            &ctx.browser,
            "extractHighlight",
            &[json!(selector), result["matched"].clone()],
        );
        Ok(pretty(&result))
    }
);

pub struct BrowserScreenshot;

impl Tool for BrowserScreenshot {
    fn def(&self) -> ToolDef {
        ToolDef::new(
            "browser_screenshot",
            "Capture a PNG screenshot of the current page (visible area). Returned as image content. Pass `tab` (browser_tabs index) to shoot a background tab without selecting it.",
            schema(json!({ "tab": tab_schema(TAB) }), &[]),
        )
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if let Err(text) = crate::validate(&self.def(), &args) {
                return err(text);
            }
            let browser = match crate::tab_browser(ctx, &crate::Args(args)) {
                Ok(browser) => browser,
                Err(text) => return err(text),
            };
            match browser.screenshot().await {
                // Shutter AFTER the capture so the flash never lands in the PNG.
                Ok(png) => {
                    visualize(&browser, "shutter", &[]);
                    ok_image("image/png", &BASE64.encode(png))
                }
                Err(text) => err(text),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use rever_core::traffic::RequestPatch;
    use rever_tools_api::tool::{Input, unsupported};

    use super::*;
    use crate::test_support::{FakeBrowser, SCOPE, call, context, settle_overlay};

    const SNAPSHOT: &str = "\n\n--- snapshot (refs from previous snapshot are now stale) ---\nurl: https://a.test/\ntitle: A\n\n";

    /// Splits an action result into the action line and what follows the snapshot header.
    fn parts(text: &str) -> (&str, &str) {
        text.split_once(SNAPSHOT)
            .unwrap_or_else(|| panic!("no snapshot in {text:?}"))
    }

    async fn snapshot(browser: &FakeBrowser, ctx: &ToolContext) -> String {
        let text = call(&BrowserSnapshot, ctx, json!({})).await.unwrap();
        browser.calls.lock().unwrap().clear();
        text
    }

    fn presses(browser: &FakeBrowser) -> Vec<Input> {
        let inputs = browser.inputs();
        let pressed = inputs
            .iter()
            .filter(|input| !matches!(input, Input::MouseMove { .. }));
        pressed.cloned().collect()
    }

    #[test]
    fn 미리보기는_공백을_한_칸으로_줄이고_120자에서_자른다() {
        assert_eq!(preview("  a\n\t b\u{a0}\u{feff}c  "), "a b c");
        assert_eq!(preview(""), "");
        assert_eq!(preview(&"x".repeat(120)), "x".repeat(120));
        assert_eq!(preview(&"x".repeat(121)), format!("{}…", "x".repeat(120)));
        // Counted in UTF-16 units: 60 emoji fill the 120.
        assert_eq!(preview(&"😀".repeat(61)), format!("{}…", "😀".repeat(60)));
    }

    #[test]
    fn 필터_노트는_빠진_것이_없으면_비어_있다() {
        assert_eq!(filter_note(&SnapshotStats::default()), "");
    }

    #[test]
    fn 필터_노트는_걸러진_것과_스캔_결과를_줄마다_적는다() {
        let stats = SnapshotStats {
            hidden: 2,
            offscreen: 5,
            click_scan_skipped: true,
            click_scanned: 7,
            click_matched: 4,
            click_only_refs: 3,
            new_refs: 1,
            fell_back_to_full: true,
            ..SnapshotStats::default()
        };
        assert_eq!(
            filter_note(&stats),
            "\n\n[viewport filter: 2 hidden/covered, 5 off-screen actionable element(s) omitted — scroll to reach them, or pass full=true for the whole page]\n\
             [click scan SKIPPED: the page has too many elements, so role-less click targets (a <div> with an onClick and no ARIA role) are missing from this outline entirely]\n\
             [click scan: 7 candidate(s), 4 correlated, 3 new ref(s) tagged (click-scan)]\n\
             [1 ref(s) marked *new — absent from the previous snapshot of this page]\n\
             [viewport filter DISABLED for this snapshot: it suppressed every element with nothing to attribute it to (likely a coordinate mismatch)]"
        );
        // Only click-only refs, nothing scanned: the scan line still shows.
        let stats = SnapshotStats {
            click_only_refs: 1,
            ..SnapshotStats::default()
        };
        assert_eq!(
            filter_note(&stats),
            "\n\n[click scan: 0 candidate(s), 0 correlated, 1 new ref(s) tagged (click-scan)]"
        );
    }

    #[test]
    fn 필터_노트는_프레임_상태를_한_줄에_이어_적는다() {
        let stats = SnapshotStats {
            frames: 2,
            frames_unreachable: 1,
            frames_empty: 1,
            ..SnapshotStats::default()
        };
        assert_eq!(
            filter_note(&stats),
            "\n\n[frames: 2 entered; 1 iframe(s) could not be entered (cross-site, runs in its own renderer — not yet supported); 1 entered but still empty (frame had not finished loading); click scan does not reach inside frames, so framed elements need an ARIA role]"
        );
        let stats = SnapshotStats {
            frames_unreachable: 3,
            ..SnapshotStats::default()
        };
        assert_eq!(
            filter_note(&stats),
            "\n\n[frames: 0 entered; 3 iframe(s) could not be entered (cross-site, runs in its own renderer — not yet supported)]"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 스냅샷은_주소와_제목과_윤곽을_돌려준다() {
        let (browser, ctx) = context(FakeBrowser::new());
        let text = call(&BrowserSnapshot, &ctx, json!({ "full": true }))
            .await
            .unwrap();
        assert!(
            text.starts_with("url: https://a.test/\ntitle: A\n\n"),
            "{text}"
        );
        assert!(text.contains("button \"Save\" [ref=r1]"), "{text}");
        assert!(browser.calls()[0].starts_with("script:(\"capture\""));
    }

    #[tokio::test(start_paused = true)]
    async fn 스냅샷을_못_뜨면_오류로_알린다() {
        let (browser, ctx) = context(FakeBrowser::new());
        *browser.capture.lock().unwrap() = None;
        assert_eq!(
            call(&BrowserSnapshot, &ctx, json!({})).await,
            Err("no page".to_owned())
        );
    }

    #[test]
    fn 큰_결과는_앞부분만_돌려주고_나머지를_이어_읽게_한다() {
        let (_, ctx) = context(FakeBrowser::new());
        assert_eq!(budgeted(&ctx, "small".to_owned()), "small");
        let text = budgeted(&ctx, "a".repeat(12_001));
        assert!(text.ends_with(
            "[truncated: showing 12000 of 12001 chars. Call tool_result_more with resultId=\"res_1\", offset=12000 for the rest.]"
        ));
        // Parked where tool_result_more looks for it.
        let store = ctx.state::<Mutex<ResultStore>>();
        assert!(
            lock(&store)
                .get_result_slice("res_1", 12_000, None)
                .is_some()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 다른_탭을_지정한_호출은_지원하지_않는다고_알린다() {
        let _scope = SCOPE.lock().await;
        let (browser, ctx) = context(FakeBrowser::new());
        let refused = Err(unsupported("driving a tab by index"));
        assert_eq!(
            call(
                &BrowserNavigate,
                &ctx,
                json!({ "url": "https://b.test/", "tab": 1 })
            )
            .await,
            refused
        );
        assert_eq!(
            call(&BrowserSnapshot, &ctx, json!({ "tab": 0 })).await,
            refused
        );
        assert_eq!(
            call(&BrowserClick, &ctx, json!({ "ref": "r1", "tab": 2 })).await,
            refused
        );
        assert_eq!(
            call(&BrowserHover, &ctx, json!({ "ref": "r1", "tab": 2 })).await,
            refused
        );
        assert_eq!(
            call(
                &BrowserType,
                &ctx,
                json!({ "ref": "r1", "text": "x", "tab": 2 })
            )
            .await,
            refused
        );
        assert_eq!(
            call(
                &BrowserEvaluate,
                &ctx,
                json!({ "expression": "1", "tab": 2 })
            )
            .await,
            refused
        );
        assert_eq!(
            call(&BrowserScreenshot, &ctx, json!({ "tab": 2 })).await,
            refused
        );
        assert!(browser.calls().is_empty() && browser.inputs().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn 이동하면_새_스냅샷과_그_동작이_일으킨_요청을_함께_돌려준다() {
        let _scope = SCOPE.lock().await;
        let (browser, ctx) = context(FakeBrowser::new());
        let text = call(&BrowserNavigate, &ctx, json!({ "url": "https://b.test/x" }))
            .await
            .unwrap();
        let (action, rest) = parts(&text);
        assert_eq!(action, "navigated to https://b.test/x");
        assert!(rest.contains("button \"Save\" [ref=r1]"));
        assert!(
            rest.contains("\n\n--- no XHR/Fetch requests triggered by this action (since="),
            "{rest}"
        );
        assert_eq!(browser.calls()[0], "navigate:https://b.test/x");
        settle_overlay().await;
        assert_eq!(
            browser.overlay(),
            ["window.__reverAi && window.__reverAi.navSweep()"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 범위_밖_주소로는_이동하지_않는다() {
        let _scope = SCOPE.lock().await;
        rever_scope::scope::set_scope(&["allowed.test"]);
        let (browser, ctx) = context(FakeBrowser::new());
        let navigated = call(
            &BrowserNavigate,
            &ctx,
            json!({ "url": "https://other.test/" }),
        )
        .await;
        let opened = call(
            &BrowserTabs,
            &ctx,
            json!({ "action": "open", "url": "https://other.test/" }),
        )
        .await;
        let allowed = call(
            &BrowserTabs,
            &ctx,
            json!({ "action": "open", "url": "https://allowed.test/" }),
        )
        .await;
        rever_scope::scope::clear_scope();

        let blocked = "Out of scope: other.test is not within the allowed scope [allowed.test]. Add it with scope_add if this target is intended.";
        assert_eq!(navigated, Err(blocked.to_owned()));
        assert_eq!(opened, Err(blocked.to_owned()));
        // In scope: the tab opens.
        assert!(
            allowed.is_ok_and(
                |text| text.starts_with("opened background tab 0: https://allowed.test/")
            )
        );
        assert!(browser.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn 스냅샷이_실패해도_동작_결과는_성공으로_돌려준다() {
        let (browser, ctx) = context(FakeBrowser::new());
        snapshot(&browser, &ctx).await;
        *browser.capture.lock().unwrap() = None;
        assert_eq!(
            call(&BrowserClick, &ctx, json!({ "ref": "r1" })).await,
            Ok("clicked r1\n\n[auto-snapshot failed: no page]".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 클릭이_일으킨_요청을_결과_끝에_적는다() {
        let (browser, ctx) = context(FakeBrowser::new());
        snapshot(&browser, &ctx).await;
        let patch = RequestPatch {
            request_id: "42".to_owned(),
            url: Some("https://a.test/api/save?x=1".to_owned()),
            method: Some("POST".to_owned()),
            resource_type: Some("XHR".to_owned()),
            started_at: Some(now_ms()),
            completed_at: Some(now_ms()),
            status: Some(201),
            ..RequestPatch::default()
        };
        lock(&ctx.traffic).upsert(patch, now_ms());

        let text = call(&BrowserClick, &ctx, json!({ "ref": "r1" }))
            .await
            .unwrap();
        let (action, rest) = parts(&text);
        assert_eq!(action, "clicked r1");
        assert!(
            rest.contains("\n\n--- requests triggered by this action (1, since="),
            "{rest}"
        );
        assert!(rest.ends_with(") ---\n#42 POST 201 https://a.test/api/save?x=1"));
        assert_eq!(presses(&browser).len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn 호버와_입력은_한_일을_첫_줄에_적는다() {
        let (browser, ctx) = context(FakeBrowser::new());
        snapshot(&browser, &ctx).await;
        let text = call(&BrowserHover, &ctx, json!({ "ref": "r1" }))
            .await
            .unwrap();
        assert_eq!(parts(&text).0, "hovered r1");
        assert!(presses(&browser).is_empty());

        let args = json!({ "ref": "r1", "text": "hi", "submit": true });
        let text = call(&BrowserType, &ctx, args).await.unwrap();
        assert_eq!(parts(&text).0, "typed into r1 + submit");
        let text = call(&BrowserType, &ctx, json!({ "ref": "r1", "text": "" }))
            .await
            .unwrap();
        assert_eq!(parts(&text).0, "typed into r1");

        assert_eq!(
            call(&BrowserClick, &ctx, json!({ "ref": "r77" })).await,
            Err("unknown ref \"r77\" — call browser_snapshot first".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 컨테이너_스크롤은_도착한_위치를_반올림해_적는다() {
        let browser = FakeBrowser::new().with_actions([
            json!({ "ok": { "canScroll": true, "scrollTop": 0 } }),
            json!({ "ok": { "x": 10, "y": 20 } }),
            json!({ "ok": { "canScroll": true, "scrollTop": 240.5 } }),
        ]);
        let (browser, ctx) = context(browser);
        snapshot(&browser, &ctx).await;
        let args = json!({ "ref": "r1", "deltaY": 240 });
        let text = call(&BrowserScrollElement, &ctx, args).await.unwrap();
        assert_eq!(parts(&text).0, "scrolled r1 to 241px");
        assert_eq!(
            browser.inputs(),
            [
                Input::Wheel {
                    x: 10.0,
                    y: 20.0,
                    dx: 0.0,
                    dy: 120.0
                },
                Input::Wheel {
                    x: 10.0,
                    y: 20.0,
                    dx: 0.0,
                    dy: 120.0
                }
            ]
        );

        let (browser, ctx) = context(
            FakeBrowser::new()
                .with_actions([json!({ "ok": { "canScroll": false, "scrollTop": 0 } })]),
        );
        snapshot(&browser, &ctx).await;
        assert_eq!(
            call(
                &BrowserScrollElement,
                &ctx,
                json!({ "ref": "r1", "deltaY": -5 })
            )
            .await,
            Err("element does not scroll vertically".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 그리기는_요소_안에서_누르고_움직여_뗀다() {
        let (browser, ctx) = context(FakeBrowser::new());
        snapshot(&browser, &ctx).await;
        let args = json!({ "ref": "r1", "from": { "x": 0, "y": 0 }, "to": { "x": 1, "y": 1 } });
        let text = call(&BrowserDraw, &ctx, args).await.unwrap();
        assert_eq!(parts(&text).0, "drew on r1");
        assert_eq!(
            presses(&browser),
            [
                Input::MouseDown { x: 80.0, y: 40.0 },
                Input::MouseUp { x: 120.0, y: 60.0 }
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 브라우저_엔진에_없는_기능은_지원하지_않는다고_알린다() {
        let _scope = SCOPE.lock().await;
        let (browser, ctx) = context(FakeBrowser::new());
        snapshot(&browser, &ctx).await;
        for (tool, args, what) in [
            (
                &BrowserPaste as &dyn rever_tools_api::tool::Tool,
                json!({ "ref": "r1", "text": "x" }),
                "writing the clipboard",
            ),
            (
                &BrowserHandleDialog,
                json!({ "accept": false, "promptText": "x" }),
                "answering native dialogs",
            ),
            (
                &BrowserFileUpload,
                json!({ "ref": "r1", "paths": [] }),
                "attaching files to a file input",
            ),
            (
                &SetViewport,
                json!({ "mode": "mobile" }),
                "switching between desktop and mobile mode",
            ),
        ] {
            assert_eq!(
                call(tool, &ctx, args.clone()).await,
                Err(unsupported(what)),
                "{args}"
            );
        }
        assert!(browser.calls().is_empty() && browser.inputs().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn 탭_열기와_파일_첨부는_인자를_먼저_확인한다() {
        let (browser, ctx) = context(FakeBrowser::new());
        for args in [
            json!({ "action": "open" }),
            json!({ "action": "open", "url": "" }),
        ] {
            assert_eq!(
                call(&BrowserTabs, &ctx, args).await,
                Err("url required for action \"open\"".to_owned())
            );
        }
        assert_eq!(
            call(
                &BrowserFileUpload,
                &ctx,
                json!({ "ref": "r1", "paths": ["/x"] })
            )
            .await,
            Err("unknown ref \"r1\" — call browser_snapshot first".to_owned())
        );
        snapshot(&browser, &ctx).await;
        assert_eq!(
            call(
                &BrowserFileUpload,
                &ctx,
                json!({ "ref": "r1", "paths": ["/no/such/file"] })
            )
            .await,
            Err("file(s) not found: /no/such/file — pass absolute paths".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 히스토리_이동은_도착한_주소를_적는다() {
        let (browser, ctx) = context(FakeBrowser::answering([
            Ok(json!(4)),
            Ok(json!("https://a.test/prev")),
        ]));
        let text = call(&BrowserHistory, &ctx, json!({ "direction": "back" }))
            .await
            .unwrap();
        assert_eq!(parts(&text).0, "went back to https://a.test/prev");
        assert_eq!(
            browser.calls()[..3],
            ["evaluate:history.length", "back", "evaluate:location.href"]
        );

        let (_, ctx) = context(FakeBrowser::answering([Ok(json!(1))]));
        assert_eq!(
            call(&BrowserHistory, &ctx, json!({ "direction": "forward" })).await,
            Err("nothing to go forward to — at entry 1 of 1".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 기본_버튼의_클릭은_왼쪽_클릭으로_적는다() {
        let (browser, ctx) = context(FakeBrowser::new());
        snapshot(&browser, &ctx).await;
        let text = call(&BrowserClickModified, &ctx, json!({ "ref": "r1" }))
            .await
            .unwrap();
        assert_eq!(parts(&text).0, "left-clicked r1");
        let args = json!({ "ref": "r1", "button": "left", "clickCount": 1 });
        let text = call(&BrowserClickModified, &ctx, args).await.unwrap();
        assert_eq!(parts(&text).0, "left-clicked r1");
        assert_eq!(presses(&browser).len(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn 드래그는_어떤_방식으로_했는지_적는다() {
        let (browser, ctx) = context(FakeBrowser::new());
        snapshot(&browser, &ctx).await;
        *browser.evaluations.lock().unwrap() = [Ok(json!("a|0|")), Ok(json!("a|1|x"))].into();
        let text = call(&BrowserDrag, &ctx, json!({ "from": "r1", "to": "r1" }))
            .await
            .unwrap();
        assert_eq!(parts(&text).0, "dragged r1 onto r1 (pointer drag)");

        *browser.evaluations.lock().unwrap() =
            [Ok(json!("a|0|")), Ok(json!("a|0|")), Ok(json!("ok"))].into();
        let text = call(&BrowserDrag, &ctx, json!({ "from": "r1", "to": "r1" }))
            .await
            .unwrap();
        assert_eq!(parts(&text).0, "dragged r1 onto r1 (synthetic drag)");
    }

    #[tokio::test(start_paused = true)]
    async fn 키를_누르고_횟수와_함께_적는다() {
        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!(true)), Ok(json!(true))]));
        snapshot(&browser, &ctx).await;
        let text = call(&BrowserPressKey, &ctx, json!({ "key": "Escape" }))
            .await
            .unwrap();
        assert_eq!(parts(&text).0, "pressed Escape");

        // The ref is focused, not clicked, before the key goes out.
        let args = json!({ "key": "ArrowDown", "ref": "r1", "repeat": 3 });
        let text = call(&BrowserPressKey, &ctx, args).await.unwrap();
        assert_eq!(parts(&text).0, "pressed ArrowDown x3");
        assert!(browser.calls().contains(
            &"script:(\"act\", {\"ref\":\"r1\",\"action\":\"focus\",\"value\":null})".to_owned()
        ));
        let key = |key: &str| Input::Key {
            key: key.to_owned(),
        };
        assert_eq!(
            browser.inputs(),
            [
                key("Escape"),
                key("ArrowDown"),
                key("ArrowDown"),
                key("ArrowDown")
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 조합키와_포커스_없는_페이지의_키는_거절한다() {
        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!(true)), Ok(json!(false))]));
        let args = json!({ "key": "a", "modifiers": ["Control"] });
        call(&BrowserPressKey, &ctx, args).await.unwrap();
        let chord = Input::KeyChord {
            key: "a".to_owned(),
            modifiers: 2,
        };
        assert_eq!(browser.inputs(), std::slice::from_ref(&chord));
        assert!(
            call(&BrowserPressKey, &ctx, json!({ "key": "a" }))
                .await
                .is_err_and(|e| e.starts_with("the page does not have keyboard focus"))
        );
        assert_eq!(browser.inputs(), [chord]);
    }

    #[tokio::test(start_paused = true)]
    async fn 탭을_나열하고_고르고_뒤에서_연다() {
        let (browser, ctx) = context(FakeBrowser::new());
        assert_eq!(
            call(&BrowserTabs, &ctx, json!({ "action": "list" })).await,
            Ok("no attached tabs".to_owned())
        );
        *browser.tabs.lock().unwrap() = vec![
            TabInfo {
                url: "https://a.test/".to_owned(),
                title: "A".to_owned(),
                active: true,
                ..TabInfo::default()
            },
            TabInfo {
                url: "https://b.test/".to_owned(),
                ..TabInfo::default()
            },
        ];
        assert_eq!(
            call(&BrowserTabs, &ctx, json!({ "action": "list" })).await,
            Ok(
                "0 * A — https://a.test/\n1   (untitled) — https://b.test/\n\n* = selected"
                    .to_owned()
            )
        );
        for index in [json!(2), json!(-1), json!(0.5), Value::Null] {
            let mut args = json!({ "action": "select" });
            if !index.is_null() {
                args["index"] = index;
            }
            assert_eq!(
                call(&BrowserTabs, &ctx, args).await,
                Err("index must be 0..1".to_owned())
            );
        }
        let selected = call(
            &BrowserTabs,
            &ctx,
            json!({ "action": "select", "index": 1 }),
        )
        .await
        .unwrap();
        assert!(
            selected.starts_with("selected tab 1: https://b.test/"),
            "{selected}"
        );
        assert!(browser.tabs.lock().unwrap()[1].active);

        let opened = call(
            &BrowserTabs,
            &ctx,
            json!({ "action": "open", "url": "https://c.test/" }),
        )
        .await;
        assert_eq!(
            opened,
            Ok("opened background tab 2: https://c.test/\n0   A — https://a.test/\n1 * (untitled) — https://b.test/\n2   (untitled) — https://c.test/\n\n* = selected. Read this tab with list_endpoints({tab: 2}).".to_owned())
        );
        assert_eq!(
            call(&BrowserTabs, &ctx, json!({ "action": "open" })).await,
            Err("url required for action \"open\"".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 스크린샷은_png를_이미지_데이터_url로_돌려준다() {
        let (_browser, ctx) = context(FakeBrowser::new());
        assert_eq!(
            call(&BrowserScreenshot, &ctx, json!({})).await,
            Ok("data:image/png;base64,iVBORw==".to_owned())
        );
        assert_eq!(
            call(&BrowserScreenshot, &ctx, json!({ "tab": 1 })).await,
            Err(unsupported("driving a tab by index"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 선택한_옵션의_라벨을_적고_못_고르면_선택지를_알린다() {
        let browser = FakeBrowser::new().with_actions([
            json!({ "ok": ["Two", "Three"] }),
            json!({ "error": "no option matched", "options": ["One", "Two"] }),
        ]);
        let (browser, ctx) = context(browser);
        snapshot(&browser, &ctx).await;
        let args = json!({ "ref": "r1", "values": ["2", "Three"] });
        let text = call(&BrowserSelectOption, &ctx, args).await.unwrap();
        assert_eq!(parts(&text).0, "selected Two, Three in r1");
        assert_eq!(
            browser.calls()[0],
            "script:(\"act\", {\"ref\":\"r1\",\"action\":\"select\",\"value\":[\"2\",\"Three\"]})"
        );
        assert_eq!(
            call(
                &BrowserSelectOption,
                &ctx,
                json!({ "ref": "r1", "values": ["x"] })
            )
            .await,
            Err("no option matched — options: One | Two".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 선택자_도구는_스냅샷_없이_동작한다() {
        let (browser, ctx) = context(FakeBrowser::new());
        let text = call(&BrowserClickSelector, &ctx, json!({ "selector": "#go" }))
            .await
            .unwrap();
        assert_eq!(parts(&text).0, "clicked selector #go");
        assert_eq!(presses(&browser).len(), 2);

        let text = call(&BrowserHoverSelector, &ctx, json!({ "selector": "nav a" }))
            .await
            .unwrap();
        assert_eq!(parts(&text).0, "hovered selector nav a");
        assert_eq!(presses(&browser).len(), 2);

        let args = json!({ "selector": "#q", "text": "한", "submit": true });
        let text = call(&BrowserTypeSelector, &ctx, args).await.unwrap();
        assert_eq!(parts(&text).0, "typed into selector #q + submit");
        assert_eq!(
            presses(&browser)[4..],
            [
                Input::Text {
                    text: "한".to_owned()
                },
                Input::Key {
                    key: "Enter".to_owned()
                }
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 페이지_스크롤은_도착한_위치를_적는다() {
        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!({ "scrollY": 300 }))]));
        assert_eq!(
            call(&BrowserScroll, &ctx, json!({ "deltaY": 300 })).await,
            Ok("scrolled, scrollY=300".to_owned())
        );
        assert!(browser.inputs().len() >= 3);

        // Neither argument: nothing moves, the position is still reported.
        let (browser, ctx) = context(FakeBrowser::answering([Ok(json!({ "scrollY": 12.5 }))]));
        assert_eq!(
            call(&BrowserScroll, &ctx, json!({})).await,
            Ok("scrolled, scrollY=12.5".to_owned())
        );
        assert!(browser.inputs().is_empty());

        let (_, ctx) = context(FakeBrowser::answering([
            Ok(json!(100)),
            Ok(json!({ "scrollY": 0 })),
        ]));
        assert_eq!(
            call(&BrowserScroll, &ctx, json!({ "y": 0, "deltaY": 50 })).await,
            Ok("scrolled, scrollY=0".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn 평가_결과는_문자열이면_그대로_아니면_json으로_돌려준다() {
        let (browser, ctx) = context(FakeBrowser::answering([
            Ok(json!("plain \"text\"")),
            Ok(json!({ "a": [1, 2.0], "b": null })),
            Ok(Value::Null),
            Err("ReferenceError: nope is not defined".to_owned()),
        ]));
        let eval = async |expression: &str| {
            call(&BrowserEvaluate, &ctx, json!({ "expression": expression })).await
        };
        assert_eq!(
            eval("document.title").await,
            Ok("plain \"text\"".to_owned())
        );
        assert_eq!(
            eval("({a:[1,2],b:null})").await,
            Ok("{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": null\n}".to_owned())
        );
        assert_eq!(eval("null").await, Ok("null".to_owned()));
        assert_eq!(
            eval("nope").await,
            Err("ReferenceError: nope is not defined".to_owned())
        );
        assert_eq!(browser.calls()[0], "evaluate:document.title");

        settle_overlay().await;
        let overlay = browser.overlay();
        assert_eq!(
            overlay[..2],
            [
                "window.__reverAi && window.__reverAi.evalHudStart(\"document.title\")",
                "window.__reverAi && window.__reverAi.evalHudDone(\"plain \\\"text\\\"\",false)"
            ]
        );
        // The failed evaluation never closes its HUD, like the TypeScript.
        assert_eq!(overlay.len(), 7);
    }

    #[tokio::test(start_paused = true)]
    async fn dom_extract는_인자를_json_리터럴로_넣어_평가하고_결과를_돌려준다() {
        let result = json!({
            "matched": 1, "count": 1, "truncated": false,
            "items": [{ "index": 0, "text": "Hi" }]
        });
        let (browser, ctx) = context(FakeBrowser::answering([Ok(result.clone())]));
        let args = json!({
            "selector": "a[title=\"x\"]", "fields": ["text", "href"], "attrs": ["data-id"], "limit": 3
        });
        assert_eq!(
            call(&DomExtract, &ctx, args).await,
            Ok("{\n  \"matched\": 1,\n  \"count\": 1,\n  \"truncated\": false,\n  \"items\": [\n    {\n      \"index\": 0,\n      \"text\": \"Hi\"\n    }\n  ]\n}".to_owned())
        );
        let expr = &browser.calls()[0];
        assert!(expr.starts_with(
            "evaluate:(() => {\n  const selector = \"a[title=\\\"x\\\"]\";\n  const fields = [\"text\",\"href\"];\n  const attrs = [\"data-id\"];\n  const limit = 3;\n  const CAP = 500;\n  const clip = (s) => s.length > CAP ? s.slice(0, CAP) + '…' : s;\n"
        ), "{expr}");
        assert!(expr.ends_with(
            "  return { matched, count: items.length, truncated: matched > items.length, items };\n})()"
        ));
        settle_overlay().await;
        assert_eq!(
            browser.overlay(),
            ["window.__reverAi && window.__reverAi.extractHighlight(\"a[title=\\\"x\\\"]\",1)"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dom_extract의_기본값은_text_필드와_50개다() {
        let (browser, ctx) = context(FakeBrowser::answering([
            Ok(json!({ "matched": 0, "count": 0, "truncated": false, "items": [] })),
            Ok(Value::Null),
            Err("SyntaxError: bad selector".to_owned()),
        ]));
        // An empty field list falls back to the default too.
        call(&DomExtract, &ctx, json!({ "selector": "li", "fields": [] }))
            .await
            .unwrap();
        assert!(
            browser.calls()[0].contains(
                "  const fields = [\"text\"];\n  const attrs = [];\n  const limit = 50;\n"
            )
        );
        assert_eq!(
            call(&DomExtract, &ctx, json!({ "selector": "li" })).await,
            Err("the page returned no result for the extraction".to_owned())
        );
        assert_eq!(
            call(&DomExtract, &ctx, json!({ "selector": "[" })).await,
            Err("SyntaxError: bad selector".to_owned())
        );
    }
}
