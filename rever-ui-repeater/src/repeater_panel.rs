//! Port of `src/renderer/src/components/repeater/RepeaterPanel.tsx` — the HTTP repeater:
//! the request loaded from Traffic on the left (method, URL, headers, body), the newest
//! response on the right, and the sends so far in a strip underneath.
//!
//! The React component reads the zustand store and sends over IPC. Here the store is the
//! host's [`RepeaterStore`] entity, shared with whatever loads requests into it, and
//! `window.rev.repeater.sendRaw` is the host's [`RepeaterSender`].

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, Entity, FontWeight, MouseButton, MouseDownEvent,
    Rgba, SharedString, Subscription, Window, deferred, div, prelude::*, px, relative,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};
use rever_ui_traffic::json_view::JsonView;

use crate::repeater::{
    Headers, HistoryEntry, RepeaterRequestSpec, RepeaterResponse, RepeaterStore,
};
use crate::text_area::TextArea;
use crate::ui::{
    ButtonKind, LINE_HEIGHT, MONO, SANS, Tokens, button, clickable, empty, field_box, select_box,
    select_menu, select_option, tooltip,
};

pub const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// `window.rev.repeater.sendRaw`: sends the request from the page and answers with what
/// came back. It may block; the panel calls it off the main thread. A send that reached
/// no response is still `Ok`, with status 0 and `error` set; `Err` is the call itself
/// failing (the IPC rejecting).
pub trait RepeaterSender: Send + Sync + 'static {
    fn send_raw(&self, spec: RepeaterRequestSpec) -> Result<RepeaterResponse, String>;
}

// ── header table edits ──────────────────────────────────────────────────────

/// `entriesToHeaders`: a row without a name is dropped, and of two rows with one name
/// the first keeps its place with the second's value.
fn entries_to_headers<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str)>) -> Headers {
    entries
        .into_iter()
        .filter(|(key, _)| !key.is_empty())
        .collect()
}

/// `setHeaderKey`: the row at `index` gets a new name.
pub fn set_header_key(headers: &Headers, index: usize, new_key: &str) -> Headers {
    entries_to_headers(
        headers
            .entries()
            .iter()
            .enumerate()
            .map(|(at, (key, value))| {
                let key = if at == index { new_key } else { key.as_str() };
                (key, value.as_str())
            }),
    )
}

/// `setHeaderValue`
pub fn set_header_value(headers: &Headers, index: usize, value: &str) -> Headers {
    entries_to_headers(
        headers
            .entries()
            .iter()
            .enumerate()
            .map(|(at, (key, old))| {
                let value = if at == index { value } else { old.as_str() };
                (key.as_str(), value)
            }),
    )
}

/// `removeHeader`
pub fn remove_header(headers: &Headers, index: usize) -> Headers {
    entries_to_headers(
        headers
            .entries()
            .iter()
            .enumerate()
            .filter(|(at, _)| *at != index)
            .map(|(_, (key, value))| (key.as_str(), value.as_str())),
    )
}

/// `addHeader`: an empty `X-New-Header`, numbered when that name is taken.
pub fn add_header(headers: &Headers) -> Headers {
    let mut name = "X-New-Header".to_owned();
    let mut n = 1;
    while headers.contains(&name) {
        name = format!("X-New-Header-{n}");
        n += 1;
    }
    let mut next = headers.clone();
    next.set(name, "");
    next
}

// ── what the response side prints ───────────────────────────────────────────

/// `statusColor`: 0 is a send that got no response. Anything from 1 to 299 takes the 2xx
/// color, unlike the traffic list's `Theme::http_status`.
pub fn status_color(theme: &Theme, status: u32) -> Rgba {
    match status {
        0 => theme.http_none,
        500.. => theme.http_5xx,
        400.. => theme.http_4xx,
        300.. => theme.http_3xx,
        _ => theme.http_2xx,
    }
}

/// `formatBytes`. `toFixed(1)` rounds a half up (1280 bytes are "1.3KB"), where Rust's
/// `{:.1}` rounds it to even, so the tenths are counted in integers.
pub fn format_bytes(n: u64) -> String {
    const KB: u128 = 1024;
    const MB: u128 = 1024 * 1024;
    let tenths = |unit: u128| (u128::from(n) * 10 + unit / 2) / unit;
    if u128::from(n) < KB {
        format!("{n}B")
    } else if u128::from(n) < MB {
        let tenths = tenths(KB);
        format!("{}.{}KB", tenths / 10, tenths % 10)
    } else {
        let tenths = tenths(MB);
        format!("{}.{}MB", tenths / 10, tenths % 10)
    }
}

/// The body field's placeholder.
pub fn body_placeholder(method: &str) -> &'static str {
    if method == "GET" || method == "HEAD" {
        "(no body for GET/HEAD)"
    } else {
        "Request body…"
    }
}

/// The response headers as the `<pre>` lists them.
pub fn headers_text(headers: &Headers) -> String {
    headers
        .entries()
        .iter()
        .map(|(key, value)| format!("{key}: {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `status || fallback`: a status of 0 prints the fallback.
fn status_label(status: u32, fallback: &str) -> String {
    if status == 0 {
        fallback.to_owned()
    } else {
        status.to_string()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// The name and value fields of one header row. The subscriptions report edits and end
/// with the row.
struct HeaderRow {
    key: Entity<TextArea>,
    value: Entity<TextArea>,
    _subscriptions: [Subscription; 2],
}

pub struct RepeaterPanel {
    store: Entity<RepeaterStore>,
    sender: Arc<dyn RepeaterSender>,
    lang: Lang,
    theme_mode: ThemeMode,
    url: Entity<TextArea>,
    header_rows: Vec<HeaderRow>,
    body: Entity<TextArea>,
    method_open: bool,
    /// The `<details>` of the response headers; a new send closes it again.
    headers_open: bool,
    /// The viewer of the newest response's body, with the `ts` of the entry it shows.
    body_view: Option<(u64, Entity<JsonView>)>,
    _subscriptions: Vec<Subscription>,
}

impl RepeaterPanel {
    pub fn new(
        store: Entity<RepeaterStore>,
        sender: Arc<dyn RepeaterSender>,
        lang: Lang,
        theme_mode: ThemeMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let url = cx.new(|cx| TextArea::single_line("", cx));
        let body = cx.new(|cx| TextArea::new(body_placeholder(""), cx));
        let subscriptions = vec![
            cx.observe(&store, |this, _, cx| this.sync_from_store(cx)),
            cx.observe(&url, |this, _, cx| this.on_field_edited(cx)),
            cx.observe(&body, |this, _, cx| this.on_field_edited(cx)),
        ];
        let mut panel = Self {
            store,
            sender,
            lang,
            theme_mode,
            url,
            header_rows: Vec::new(),
            body,
            method_open: false,
            headers_open: false,
            body_view: None,
            _subscriptions: subscriptions,
        };
        panel.sync_from_store(cx);
        panel
    }

    fn new_header_row(cx: &mut Context<Self>) -> HeaderRow {
        let key = cx.new(|cx| TextArea::single_line("", cx));
        let value = cx.new(|cx| TextArea::single_line("", cx));
        let subscriptions = [
            cx.observe(&key, |this, _, cx| this.on_field_edited(cx)),
            cx.observe(&value, |this, _, cx| this.on_field_edited(cx)),
        ];
        HeaderRow {
            key,
            value,
            _subscriptions: subscriptions,
        }
    }

    /// The store changed (a load, a restore, a send, or this panel's own edit): make the
    /// fields show it. A field that already shows its value is left alone, so typing
    /// keeps its cursor.
    fn sync_from_store(&mut self, cx: &mut Context<Self>) {
        let (loading, latest_ts) = {
            let store = self.store.read(cx);
            (
                store.loading(),
                store.history().first().map(|entry| entry.ts),
            )
        };
        if loading {
            // `latest && !loading` unmounts the response while a send is out, which
            // resets the `<details>` and the body viewer.
            self.headers_open = false;
            self.body_view = None;
        } else if self.body_view.as_ref().map(|(ts, _)| *ts) != latest_ts {
            self.body_view = self.new_body_view(cx);
        }

        let Some(active) = self.store.read(cx).active().cloned() else {
            self.method_open = false;
            self.header_rows.clear();
            cx.notify();
            return;
        };
        set_field(&self.url, &active.url, cx);
        let body = active.body.as_deref().unwrap_or_default();
        self.body.update(cx, |field, cx| {
            if field.text() != body {
                field.set_text(body, cx);
            }
            field.set_placeholder(body_placeholder(&active.method), cx);
        });
        let entries = active.headers.entries();
        self.header_rows.truncate(entries.len());
        while self.header_rows.len() < entries.len() {
            self.header_rows.push(Self::new_header_row(cx));
        }
        for (row, (key, value)) in self.header_rows.iter().zip(entries) {
            set_field(&row.key, key, cx);
            set_field(&row.value, value, cx);
        }
        cx.notify();
    }

    fn new_body_view(&self, cx: &mut Context<Self>) -> Option<(u64, Entity<JsonView>)> {
        let (ts, body, content_type) = {
            let latest = self.store.read(cx).history().first()?;
            // An empty body prints "(empty)" instead of a viewer.
            (!latest.response.body.is_empty()).then_some(())?;
            (
                latest.ts,
                latest.response.body.clone(),
                latest
                    .response
                    .headers
                    .get("content-type")
                    .map(str::to_owned),
            )
        };
        let (lang, theme_mode) = (self.lang, self.theme_mode);
        let view = cx.new(|_| JsonView::new(body, content_type.as_deref(), lang, theme_mode));
        Some((ts, view))
    }

    /// A field notified: write what differs from the store back into it, as the
    /// `onChange` handlers do. One field changes at a time.
    fn on_field_edited(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.store.read(cx).active().cloned() else {
            return;
        };
        let mut next = active.clone();
        let url = self.url.read(cx).text();
        if url != active.url {
            next.url = url.to_owned();
        }
        let body = self.body.read(cx).text();
        if body != active.body.as_deref().unwrap_or_default() {
            // `e.target.value || undefined`
            next.body = (!body.is_empty()).then(|| body.to_owned());
        }
        for (index, (row, (key, value))) in self
            .header_rows
            .iter()
            .zip(active.headers.entries())
            .enumerate()
        {
            let typed_key = row.key.read(cx).text();
            if typed_key != key {
                next.headers = set_header_key(&active.headers, index, typed_key);
                break;
            }
            let typed_value = row.value.read(cx).text();
            if typed_value != value {
                next.headers = set_header_value(&active.headers, index, typed_value);
                break;
            }
        }
        if next != active {
            self.set_active(next, cx);
        }
    }

    fn set_active(&mut self, request: RepeaterRequestSpec, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.set_active(request);
            cx.notify();
        });
    }

    /// `update(patch)` for the edits that are not typed into a field.
    fn patch(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut RepeaterRequestSpec)) {
        let Some(mut next) = self.store.read(cx).active().cloned() else {
            return;
        };
        edit(&mut next);
        self.set_active(next, cx);
    }

    fn set_method(&mut self, method: &str, cx: &mut Context<Self>) {
        self.method_open = false;
        self.patch(cx, |request| request.method = method.to_owned());
        cx.notify();
    }

    /// The Send button. Does nothing without a request or while a send is out.
    pub fn send(&mut self, cx: &mut Context<Self>) {
        if self.store.read(cx).loading() {
            return;
        }
        let request = self.store.update(cx, |store, cx| {
            cx.notify();
            store.begin_send()
        });
        let Some(request) = request else {
            return;
        };
        let (sender, store) = (self.sender.clone(), self.store.downgrade());
        cx.spawn(async move |_, cx| {
            let spec = request.clone();
            let result = cx
                .background_executor()
                .spawn(async move { sender.send_raw(spec) })
                .await;
            let _ = store.update(cx, |store, cx| {
                store.finish_send(request, result, now_ms());
                cx.notify();
            });
        })
        .detach();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.clear();
            cx.notify();
        });
    }

    fn restore(&mut self, index: usize, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.restore_from_history(index);
            cx.notify();
        });
    }

    // ── rendering ───────────────────────────────────────────────────────────

    /// `.panel-toolbar`
    fn toolbar(&self, store: &RepeaterStore, t: &Tokens, cx: &Context<Self>) -> Div {
        let lang = self.lang;
        let loading = store.loading();
        let label: SharedString = store.source_label().unwrap_or_default().to_owned().into();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(t.theme.border)
            .text_size(px(11.0))
            .line_height(px(LINE_HEIGHT))
            .child(div().flex_none().opacity(0.6).child("source:"))
            .child(
                clickable("repeater-source")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO)
                    .opacity(0.8)
                    .tooltip(tooltip(label.clone(), t))
                    .child(label),
            )
            .child(
                button(
                    "repeater-send",
                    lang.t(if loading {
                        "repeater.sending"
                    } else {
                        "repeater.send"
                    }),
                    ButtonKind::Primary,
                    !loading,
                    t,
                )
                .when(!loading, |button| {
                    button.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.send(cx)))
                }),
            )
            .child(
                button(
                    "repeater-clear",
                    lang.t("common.clear"),
                    ButtonKind::Plain,
                    true,
                    t,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.clear(cx))),
            )
    }

    /// The "Request" and "Response" bars. `text-transform: uppercase` is applied here;
    /// GPUI has no `letter-spacing`.
    fn section_title(text: &str, t: &Tokens) -> Div {
        div()
            .flex_none()
            .px(px(12.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(t.theme.border)
            .text_size(px(10.0))
            .line_height(px(12.0))
            .opacity(0.6)
            .child(text.to_uppercase())
    }

    /// The small caption over a block ("Headers (3)", "Body", "History (2)").
    fn caption(text: String) -> Div {
        div()
            .text_size(px(10.0))
            .line_height(px(12.0))
            .opacity(0.6)
            .child(text.to_uppercase())
    }

    fn method_select(&self, active: &RepeaterRequestSpec, t: &Tokens, cx: &Context<Self>) -> Div {
        let select = select_box("repeater-method", active.method.clone(), t)
            // A native select is as wide as its widest option ("OPTIONS").
            .w(px(78.0))
            .px(px(6.0))
            .py(px(4.0))
            .text_size(px(11.0))
            .line_height(px(LINE_HEIGHT))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.method_open = !this.method_open;
                cx.notify();
            }));
        let wrap = div().relative().flex_none().child(select);
        if !self.method_open {
            return wrap;
        }
        let menu = select_menu("repeater-method-menu", t)
            .top(relative(1.0))
            .mt(px(2.0))
            .text_size(px(11.0))
            .line_height(px(LINE_HEIGHT))
            // A press outside closes the list and goes no further, as a native popup does.
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Left {
                    this.method_open = false;
                    cx.notify();
                }
                cx.stop_propagation();
            }))
            .children(METHODS.into_iter().enumerate().map(|(index, method)| {
                select_option(
                    ("repeater-method-option", index),
                    method,
                    active.method == method,
                    t,
                )
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.set_method(method, cx)),
                )
            }));
        wrap.child(deferred(menu).with_priority(1))
    }

    fn header_table(&self, t: &Tokens, cx: &Context<Self>) -> Div {
        let lang = self.lang;
        let cell = |id: (&'static str, usize), field: &Entity<TextArea>| {
            field_box(id, t)
                .w_full()
                .px(px(6.0))
                .py(px(2.0))
                .child(field.clone())
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .font_family(MONO)
            .text_size(px(11.0))
            .line_height(px(LINE_HEIGHT))
            .children(self.header_rows.iter().enumerate().map(|(index, row)| {
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .w(relative(0.4))
                            .flex_none()
                            .p(px(1.0))
                            .child(cell(("repeater-header-key", index), &row.key)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .p(px(1.0))
                            .child(cell(("repeater-header-value", index), &row.value)),
                    )
                    .child(
                        div().w(px(24.0)).flex_none().p(px(1.0)).child(
                            button(
                                ("repeater-header-remove", index),
                                "×",
                                ButtonKind::DangerIcon,
                                true,
                                t,
                            )
                            .tooltip(tooltip(lang.t("repeater.removeHeader"), t))
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    this.patch(cx, |request| {
                                        request.headers = remove_header(&request.headers, index)
                                    })
                                },
                            )),
                        ),
                    )
            }))
    }

    /// `RequestEditor`
    fn request_editor(&self, active: &RepeaterRequestSpec, t: &Tokens, cx: &Context<Self>) -> Div {
        let lang = self.lang;
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(t.theme.border)
            .child(Self::section_title("Request", t))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.0))
                    .p(px(10.0))
                    .child(self.method_select(active, t, cx))
                    .child(
                        field_box("repeater-url", t)
                            .flex_1()
                            .min_w_0()
                            .px(px(8.0))
                            .py(px(4.0))
                            .font_family(MONO)
                            .text_size(px(11.0))
                            .line_height(px(LINE_HEIGHT))
                            .child(self.url.clone()),
                    ),
            )
            .child(
                clickable("repeater-request-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(10.0))
                    .pb(px(10.0))
                    .child(
                        div()
                            .mt(px(8.0))
                            .mb(px(4.0))
                            .flex()
                            .items_center()
                            .child(Self::caption(format!("Headers ({})", active.headers.len())))
                            .child(div().flex_1())
                            .child(
                                // The button sits inside the caption's div: it is
                                // upper-cased and dimmed with it.
                                div().opacity(0.6).child(
                                    button(
                                        "repeater-add-header",
                                        lang.t("repeater.addHeader").to_uppercase(),
                                        ButtonKind::Ghost,
                                        true,
                                        t,
                                    )
                                    .on_click(cx.listener(
                                        |this, _: &ClickEvent, _, cx| {
                                            this.patch(cx, |request| {
                                                request.headers = add_header(&request.headers)
                                            })
                                        },
                                    )),
                                ),
                            ),
                    )
                    .child(self.header_table(t, cx))
                    .child(Self::caption("Body".to_owned()).mt(px(12.0)).mb(px(4.0)))
                    .child(
                        // The `<textarea>`: 120px high with its padding and border, the
                        // text scrolling inside.
                        clickable("repeater-body")
                            .w_full()
                            .h(px(120.0))
                            .flex()
                            .flex_col()
                            .p(px(8.0))
                            .bg(t.theme.bg)
                            .border_1()
                            .border_color(t.theme.border)
                            .rounded(px(4.0))
                            .text_color(t.theme.text)
                            .font_family(MONO)
                            .text_size(px(11.0))
                            .line_height(px(LINE_HEIGHT))
                            .child(self.body.clone()),
                    ),
            )
    }

    /// The newest response: status line, collapsible headers, body.
    fn response_block(&self, latest: &HistoryEntry, t: &Tokens, cx: &Context<Self>) -> Div {
        let response = &latest.response;
        let open = self.headers_open;
        let block = div()
            .font_family(MONO)
            .text_size(px(11.0))
            .line_height(px(LINE_HEIGHT))
            .child(
                div()
                    .debug_selector(|| "repeater-status".into())
                    .mb(px(8.0))
                    .flex()
                    .flex_wrap()
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .text_color(status_color(&t.theme, response.status))
                            .child(status_label(response.status, "—")),
                    )
                    // `{' '}` between the status and its text.
                    .child(format!(" {}", response.status_text))
                    .child(div().ml(px(10.0)).opacity(0.6).child(format!(
                        "{}ms · {}",
                        response.time_ms,
                        format_bytes(response.body_byte_length)
                    ))),
            )
            .child(
                // `<details>`
                div()
                    .mb(px(8.0))
                    .child(
                        clickable("repeater-response-headers")
                            .opacity(0.7)
                            .cursor_pointer()
                            .child(format!(
                                "{} Headers ({})",
                                if open { "▾" } else { "▸" },
                                response.headers.len()
                            ))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.headers_open = !this.headers_open;
                                cx.notify();
                            })),
                    )
                    .when(open, |details| {
                        details.child(
                            div()
                                .debug_selector(|| "repeater-response-headers-text".into())
                                .mt(px(4.0))
                                .p(px(8.0))
                                .bg(t.theme.bg)
                                .border_1()
                                .border_color(t.theme.border)
                                .rounded(px(4.0))
                                .child(headers_text(&response.headers)),
                        )
                    }),
            );
        let block = match &self.body_view {
            Some((_, view)) if !response.body.is_empty() => block.child(view.clone()),
            _ => block.child(div().opacity(0.5).child("(empty)")),
        };
        block.when(response.body_truncated, |block| {
            block.child(div().mt(px(4.0)).opacity(0.6).child("[truncated…]"))
        })
    }

    /// `ResponseViewer`
    fn response_viewer(&self, store: &RepeaterStore, t: &Tokens, cx: &Context<Self>) -> Div {
        let loading = store.loading();
        let latest = store.history().first();
        // An empty error text shows nothing, as an empty string is falsy in the component.
        let error = store.error().filter(|error| !error.is_empty());
        let note = |opacity: f32, text: &'static str| {
            div()
                .opacity(opacity)
                .text_size(px(11.0))
                .line_height(px(LINE_HEIGHT))
                .child(text)
        };
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(Self::section_title("Response", t))
            .child(
                clickable("repeater-response-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(10.0))
                    .when(loading, |area| area.child(note(0.6, "Sending…")))
                    .when(!loading && latest.is_none() && error.is_none(), |area| {
                        area.child(note(0.4, "No response yet — press Send."))
                    })
                    .when_some(error.filter(|_| !loading), |area, error| {
                        area.child(
                            div()
                                .debug_selector(|| "repeater-error".into())
                                .mb(px(8.0))
                                .text_color(t.theme.status_error)
                                .font_family(MONO)
                                .text_size(px(11.0))
                                .line_height(px(LINE_HEIGHT))
                                .child(format!("error: {error}")),
                        )
                    })
                    .when_some(latest.filter(|_| !loading), |area, latest| {
                        area.child(self.response_block(latest, t, cx))
                    }),
            )
    }

    /// `HistoryStrip`
    fn history_strip(
        &self,
        history: &[HistoryEntry],
        t: &Tokens,
        cx: &Context<Self>,
    ) -> AnyElement {
        let lang = self.lang;
        clickable("repeater-history")
            .flex_none()
            .max_h(px(110.0))
            .overflow_y_scroll()
            .border_t_1()
            .border_color(t.theme.border)
            .px(px(10.0))
            .py(px(6.0))
            .text_size(px(11.0))
            .line_height(px(LINE_HEIGHT))
            .child(Self::caption(format!("History ({})", history.len())).mb(px(4.0)))
            .children(history.iter().enumerate().map(|(index, entry)| {
                let url: SharedString = entry.request.url.clone().into();
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .py(px(2.0))
                    .font_family(MONO)
                    .child(
                        div()
                            .w(px(38.0))
                            .flex_none()
                            .text_color(status_color(&t.theme, entry.response.status))
                            .child(status_label(entry.response.status, "ERR")),
                    )
                    .child(
                        div()
                            .w(px(60.0))
                            .flex_none()
                            .opacity(0.7)
                            .child(format!("{}ms", entry.response.time_ms)),
                    )
                    .child(
                        div()
                            .w(px(70.0))
                            .flex_none()
                            .opacity(0.6)
                            .child(entry.request.method.clone()),
                    )
                    .child(
                        clickable(("repeater-history-url", index))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .opacity(0.7)
                            .tooltip(tooltip(url.clone(), t))
                            .child(url),
                    )
                    .child(
                        button(
                            ("repeater-restore", index),
                            lang.t("repeater.restore"),
                            ButtonKind::Ghost,
                            true,
                            t,
                        )
                        .on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| this.restore(index, cx)),
                        ),
                    )
            }))
            .into_any_element()
    }
}

/// Put `value` in a one-line field unless it already holds it.
fn set_field(field: &Entity<TextArea>, value: &str, cx: &mut App) {
    if field.read(cx).text() != value {
        field.update(cx, |field, cx| field.set_text(value, cx));
    }
}

impl Render for RepeaterPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::resolve(self.theme_mode, window);
        let root = div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(t.theme.bg)
            .text_color(t.theme.text)
            .font_family(SANS)
            .text_size(px(13.0));
        let store = self.store.read(cx);
        let Some(active) = store.active() else {
            return root.child(
                empty(self.lang.t("repeater.empty"), &t).debug_selector(|| "repeater-empty".into()),
            );
        };
        root.child(self.toolbar(store, &t, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.request_editor(active, &t, cx))
                    .child(self.response_viewer(store, &t, cx)),
            )
            .when(!store.history().is_empty(), |root| {
                root.child(self.history_strip(store.history(), &t, cx))
            })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use rever_core::traffic::StoredRequest;

    use super::*;

    fn headers(entries: &[(&str, &str)]) -> Headers {
        entries.iter().copied().collect()
    }

    fn keys(headers: &Headers) -> Vec<&str> {
        headers.entries().iter().map(|(k, _)| k.as_str()).collect()
    }

    // ── header table edits ──────────────────────────────────────────────────

    #[test]
    fn 헤더_이름을_바꾸면_그_행만_바뀌고_순서는_그대로다() {
        let before = headers(&[("accept", "*/*"), ("x-a", "1"), ("x-b", "2")]);
        let after = set_header_key(&before, 1, "x-renamed");
        assert_eq!(
            after,
            headers(&[("accept", "*/*"), ("x-renamed", "1"), ("x-b", "2")])
        );
    }

    #[test]
    fn 헤더_이름을_비우면_그_행이_사라진다() {
        let before = headers(&[("accept", "*/*"), ("x-a", "1")]);
        assert_eq!(
            set_header_key(&before, 1, ""),
            headers(&[("accept", "*/*")])
        );
    }

    #[test]
    fn 헤더_이름이_다른_행과_같아지면_앞_행_자리에_뒤_행의_값으로_합쳐진다() {
        let before = headers(&[("a", "1"), ("b", "2"), ("c", "3")]);
        // 셋째 행을 "a" 로: 첫 행 자리에 값 3.
        assert_eq!(
            set_header_key(&before, 2, "a"),
            headers(&[("a", "3"), ("b", "2")])
        );
        // 첫 행을 "c" 로: 첫 행 자리에 남고 값은 뒤 행(3)의 것.
        assert_eq!(
            set_header_key(&before, 0, "c"),
            headers(&[("c", "3"), ("b", "2")])
        );
    }

    #[test]
    fn 숫자_이름으로_바꾼_헤더는_자바스크립트_객체처럼_앞으로_온다() {
        // bun 으로 돌린 원본의 Object.entries 결과와 같다.
        let before = headers(&[("a", "1"), ("b", "2"), ("c", "3")]);
        assert_eq!(
            set_header_key(&before, 1, "7").entries(),
            headers(&[("7", "2"), ("a", "1"), ("c", "3")]).entries()
        );
        assert_eq!(keys(&set_header_key(&before, 1, "7")), ["7", "a", "c"]);
        // 표의 행 번호는 객체 순서를 따른다: {"2","a"} 의 둘째 행은 "a" 다.
        let numbered = headers(&[("2", "x"), ("a", "y")]);
        let renamed = set_header_key(&numbered, 1, "1");
        assert_eq!(keys(&renamed), ["1", "2"]);
        assert_eq!(renamed.get("1"), Some("y"));
        // "07" 은 배열 인덱스가 아니라서 제자리에 남는다.
        let mixed = headers(&[("a", "1"), ("10", "t"), ("b", "2")]);
        assert_eq!(keys(&mixed), ["10", "a", "b"]);
        assert_eq!(keys(&set_header_key(&mixed, 2, "07")), ["10", "a", "07"]);
        // 값 고치기도 객체 순서의 행을 고친다.
        let shuffled = headers(&[("b", "1"), ("3", "q"), ("a", "2")]);
        let changed = set_header_value(&shuffled, 0, "zz");
        assert_eq!(keys(&changed), ["3", "b", "a"]);
        assert_eq!(changed.get("3"), Some("zz"));
    }

    #[test]
    fn 헤더_값을_바꾸면_이름과_순서는_그대로다() {
        let before = headers(&[("a", "1"), ("b", "2")]);
        assert_eq!(
            set_header_value(&before, 0, "changed"),
            headers(&[("a", "changed"), ("b", "2")])
        );
        // 빈 값도 값이다.
        assert_eq!(
            set_header_value(&before, 1, ""),
            headers(&[("a", "1"), ("b", "")])
        );
    }

    #[test]
    fn 헤더를_지우면_그_행만_빠지고_범위_밖_인덱스는_아무것도_지우지_않는다() {
        let before = headers(&[("a", "1"), ("b", "2"), ("c", "3")]);
        assert_eq!(
            remove_header(&before, 1),
            headers(&[("a", "1"), ("c", "3")])
        );
        assert_eq!(remove_header(&before, 9), before);
        assert_eq!(set_header_key(&before, 9, "x"), before);
        assert_eq!(set_header_value(&before, 9, "x"), before);
        assert_eq!(remove_header(&Headers::new(), 0), Headers::new());
    }

    #[test]
    fn 새_헤더는_빈_값의_x_new_header이고_이름이_겹치면_번호를_붙인다() {
        let one = add_header(&headers(&[("accept", "*/*")]));
        assert_eq!(keys(&one), ["accept", "X-New-Header"]);
        assert_eq!(one.get("X-New-Header"), Some(""));
        let two = add_header(&one);
        assert_eq!(keys(&two), ["accept", "X-New-Header", "X-New-Header-1"]);
        let three = add_header(&two);
        assert_eq!(
            keys(&three),
            ["accept", "X-New-Header", "X-New-Header-1", "X-New-Header-2"]
        );
        // 이름은 대소문자까지 같아야 겹친 것이다.
        let other_case = add_header(&headers(&[("x-new-header", "")]));
        assert_eq!(keys(&other_case), ["x-new-header", "X-New-Header"]);
    }

    // ── what the response side prints ───────────────────────────────────────

    #[test]
    fn 상태_색은_0은_없음이고_300_400_500에서_바뀐다() {
        let theme = Theme::light();
        assert_eq!(status_color(&theme, 0), theme.http_none);
        assert_eq!(status_color(&theme, 200), theme.http_2xx);
        assert_eq!(status_color(&theme, 299), theme.http_2xx);
        // 원본은 1xx 도 2xx 색으로 칠한다.
        assert_eq!(status_color(&theme, 101), theme.http_2xx);
        assert_eq!(status_color(&theme, 300), theme.http_3xx);
        assert_eq!(status_color(&theme, 399), theme.http_3xx);
        assert_eq!(status_color(&theme, 400), theme.http_4xx);
        assert_eq!(status_color(&theme, 499), theme.http_4xx);
        assert_eq!(status_color(&theme, 500), theme.http_5xx);
        assert_eq!(status_color(&theme, 999), theme.http_5xx);
    }

    #[test]
    fn 바이트_수는_b_kb_mb로_소수_한_자리까지_적는다() {
        assert_eq!(format_bytes(0), "0B");
        assert_eq!(format_bytes(1023), "1023B");
        assert_eq!(format_bytes(1024), "1.0KB");
        assert_eq!(format_bytes(1536), "1.5KB");
        assert_eq!(format_bytes(65_536), "64.0KB");
        // (1048575 / 1024).toFixed(1) === "1024.0"
        assert_eq!(format_bytes(1_048_575), "1024.0KB");
        assert_eq!(format_bytes(1_048_576), "1.0MB");
        assert_eq!(format_bytes(5_767_168), "5.5MB");
    }

    #[test]
    fn 바이트_수의_반올림은_tofixed처럼_반을_올린다() {
        // (1280 / 1024).toFixed(1) === "1.3" (1.25); Rust 의 {:.1} 은 "1.2" 다.
        assert_eq!(format_bytes(1280), "1.3KB");
        // (1792 / 1024).toFixed(1) === "1.8" (1.75)
        assert_eq!(format_bytes(1792), "1.8KB");
        // (1310720 / 1048576).toFixed(1) === "1.3" (1.25)
        assert_eq!(format_bytes(1_310_720), "1.3MB");
        // bun 으로 돌린 원본의 값: 반올림이 단위 경계를 넘는 자리.
        assert_eq!(format_bytes(1075), "1.0KB");
        assert_eq!(format_bytes(1126), "1.1KB");
        assert_eq!(format_bytes(1_048_524), "1023.9KB");
        assert_eq!(format_bytes(1_048_525), "1024.0KB");
        assert_eq!(format_bytes(1_101_004), "1.0MB");
        assert_eq!(format_bytes(1_101_005), "1.1MB");
        assert_eq!(format_bytes(9_007_199_254_740_991), "8589934592.0MB");
        // 아주 큰 값도 넘치지 않는다.
        assert!(format_bytes(u64::MAX).ends_with("MB"));
    }

    #[test]
    fn 본문_자리표시자는_get과_head에서만_본문_없음을_알린다() {
        assert_eq!(body_placeholder("GET"), "(no body for GET/HEAD)");
        assert_eq!(body_placeholder("HEAD"), "(no body for GET/HEAD)");
        assert_eq!(body_placeholder("POST"), "Request body…");
        // 대소문자를 가린다.
        assert_eq!(body_placeholder("get"), "Request body…");
        assert_eq!(body_placeholder(""), "Request body…");
    }

    #[test]
    fn 응답_헤더는_이름_콜론_값을_줄마다_적는다() {
        assert_eq!(
            headers_text(&headers(&[("content-type", "text/html"), ("x-a", "")])),
            "content-type: text/html\nx-a: "
        );
        assert_eq!(headers_text(&Headers::new()), "");
    }

    #[test]
    fn 상태_0은_대체_문구로_적는다() {
        assert_eq!(status_label(0, "—"), "—");
        assert_eq!(status_label(0, "ERR"), "ERR");
        assert_eq!(status_label(204, "—"), "204");
    }

    // ── the panel in a window ───────────────────────────────────────────────

    /// The page side: records what was sent and answers with what the test put in.
    struct FakeSender {
        sent: Mutex<Vec<RepeaterRequestSpec>>,
        answer: Mutex<Result<RepeaterResponse, String>>,
    }

    impl FakeSender {
        fn answering(answer: Result<RepeaterResponse, String>) -> Arc<Self> {
            Arc::new(Self {
                sent: Mutex::new(Vec::new()),
                answer: Mutex::new(answer),
            })
        }

        fn sent(&self) -> Vec<RepeaterRequestSpec> {
            self.sent.lock().unwrap().clone()
        }
    }

    impl RepeaterSender for FakeSender {
        fn send_raw(&self, spec: RepeaterRequestSpec) -> Result<RepeaterResponse, String> {
            self.sent.lock().unwrap().push(spec);
            self.answer.lock().unwrap().clone()
        }
    }

    fn ok_response() -> RepeaterResponse {
        RepeaterResponse {
            status: 200,
            status_text: "OK".into(),
            headers: headers(&[("content-type", "application/json")]),
            body: "{\"ok\":true}".into(),
            body_truncated: false,
            body_byte_length: 11,
            time_ms: 42,
            error: None,
        }
    }

    fn stored() -> StoredRequest {
        StoredRequest {
            request_id: "r1".into(),
            url: "https://api.test/items".into(),
            method: "POST".into(),
            request_headers: Some(
                [("accept", "*/*"), ("x-token", "abc")]
                    .into_iter()
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .collect(),
            ),
            request_post_data: Some("{\"a\":1}".into()),
            ..StoredRequest::default()
        }
    }

    struct Stage<'a> {
        panel: Entity<RepeaterPanel>,
        store: Entity<RepeaterStore>,
        sender: Arc<FakeSender>,
        cx: &'a mut VisualTestContext,
    }

    impl Stage<'_> {
        fn click(&mut self, selector: &'static str) {
            let bounds = self
                .cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} is not on screen"));
            self.cx
                .simulate_click(bounds.center(), Modifiers::default());
            self.cx.run_until_parked();
        }

        fn shown(&mut self, selector: &'static str) -> bool {
            self.cx.debug_bounds(selector).is_some()
        }

        fn active(&mut self) -> RepeaterRequestSpec {
            self.store
                .read_with(self.cx, |store, _| store.active().cloned())
                .expect("a request is loaded")
        }

        fn header_fields(&mut self) -> Vec<(String, String)> {
            self.panel.read_with(self.cx, |panel, cx| {
                panel
                    .header_rows
                    .iter()
                    .map(|row| {
                        (
                            row.key.read(cx).text().to_owned(),
                            row.value.read(cx).text().to_owned(),
                        )
                    })
                    .collect()
            })
        }

        fn load(&mut self) {
            self.store.update(self.cx, |store, cx| {
                store.load_from_traffic("r1", Some(&stored()));
                cx.notify();
            });
            self.cx.run_until_parked();
        }
    }

    fn open(
        cx: &mut TestAppContext,
        answer: Result<RepeaterResponse, String>,
        loaded: bool,
    ) -> Stage<'_> {
        cx.update(rever_ui_kit::text_input::bind_keys);
        let sender = FakeSender::answering(answer);
        let store = cx.new(|_| RepeaterStore::new());
        let (panel, cx) = cx.add_window_view({
            let (store, sender) = (store.clone(), sender.clone());
            move |_, cx| RepeaterPanel::new(store, sender, Lang::En, ThemeMode::Light, cx)
        });
        let mut stage = Stage {
            panel,
            store,
            sender,
            cx,
        };
        if loaded {
            stage.load();
        }
        stage
    }

    #[gpui::test]
    fn 불러온_요청이_없으면_안내만_보인다(cx: &mut TestAppContext) {
        let mut stage = open(cx, Ok(ok_response()), false);
        assert!(stage.shown("repeater-empty"));
        assert!(!stage.shown("repeater-send"));
        assert!(!stage.shown("repeater-url"));
    }

    #[gpui::test]
    fn 요청을_불러오면_필드가_그_요청으로_채워진다(cx: &mut TestAppContext) {
        let mut stage = open(cx, Ok(ok_response()), true);
        assert!(!stage.shown("repeater-empty"));
        assert!(stage.shown("repeater-send"));
        // 응답이 없으니 기록 띠도 없다.
        assert!(!stage.shown("repeater-history"));
        assert_eq!(
            stage.header_fields(),
            [
                ("accept".to_owned(), "*/*".to_owned()),
                ("x-token".into(), "abc".into())
            ]
        );
        stage.panel.read_with(stage.cx, |panel, cx| {
            assert_eq!(panel.url.read(cx).text(), "https://api.test/items");
            assert_eq!(panel.body.read(cx).text(), "{\"a\":1}");
        });
        // 두 칸은 같은 폭으로 나뉜다.
        let window = stage.cx.debug_bounds("repeater-request-scroll").unwrap();
        let response = stage.cx.debug_bounds("repeater-response-scroll").unwrap();
        assert!((window.size.width - response.size.width).abs() <= px(1.0));
        // 본문 칸은 120px 이다.
        let body = stage.cx.debug_bounds("repeater-body").unwrap();
        assert_eq!(body.size.height, px(120.0));
    }

    #[gpui::test]
    fn url을_고치면_스토어의_요청이_바뀐다(cx: &mut TestAppContext) {
        let mut stage = open(cx, Ok(ok_response()), true);
        stage.click("repeater-url");
        stage.cx.simulate_keystrokes("end");
        stage.cx.simulate_input("?page=2");
        assert_eq!(stage.active().url, "https://api.test/items?page=2");
        // 다른 필드는 그대로다.
        assert_eq!(stage.active().method, "POST");
        assert_eq!(stage.active().headers.len(), 2);
    }

    #[gpui::test]
    fn 헤더_값과_이름을_고치면_그_행만_바뀐다(cx: &mut TestAppContext) {
        let mut stage = open(cx, Ok(ok_response()), true);
        stage.click("repeater-header-value-1");
        stage.cx.simulate_keystrokes("end");
        stage.cx.simulate_input("def");
        assert_eq!(stage.active().headers.get("x-token"), Some("abcdef"));
        stage.click("repeater-header-key-0");
        stage.cx.simulate_keystrokes("end");
        stage.cx.simulate_input("-x");
        assert_eq!(
            stage.active().headers,
            headers(&[("accept-x", "*/*"), ("x-token", "abcdef")])
        );
        // 입력 중인 칸은 다시 채워지지 않아 커서가 제자리에 있다.
        stage.cx.simulate_input("y");
        assert_eq!(stage.active().headers.get("accept-xy"), Some("*/*"));
    }

    #[gpui::test]
    fn 헤더_이름을_다_지우면_행이_사라지고_다음_행이_그_자리에_온다(
        cx: &mut TestAppContext,
    ) {
        let mut stage = open(cx, Ok(ok_response()), true);
        stage.click("repeater-header-key-0");
        stage.cx.simulate_keystrokes("cmd-a backspace");
        assert_eq!(stage.active().headers, headers(&[("x-token", "abc")]));
        assert_eq!(
            stage.header_fields(),
            [("x-token".to_owned(), "abc".to_owned())]
        );
    }

    #[gpui::test]
    fn 추가_버튼은_빈_헤더_행을_만들고_x_버튼은_그_행을_지운다(
        cx: &mut TestAppContext,
    ) {
        let mut stage = open(cx, Ok(ok_response()), true);
        stage.click("repeater-add-header");
        stage.click("repeater-add-header");
        assert_eq!(
            keys(&stage.active().headers),
            ["accept", "x-token", "X-New-Header", "X-New-Header-1"]
        );
        assert_eq!(stage.header_fields().len(), 4);
        stage.click("repeater-header-remove-1");
        assert_eq!(
            keys(&stage.active().headers),
            ["accept", "X-New-Header", "X-New-Header-1"]
        );
        assert_eq!(stage.header_fields()[1].0, "X-New-Header");
        assert_eq!(stage.header_fields().len(), 3);
    }

    #[gpui::test]
    fn 메서드_목록에서_고르면_요청의_메서드가_바뀌고_목록이_닫힌다(
        cx: &mut TestAppContext,
    ) {
        let mut stage = open(cx, Ok(ok_response()), true);
        assert!(!stage.shown("repeater-method-menu"));
        stage.click("repeater-method");
        // 목록은 선택 상자 바로 아래에, 적어도 그 폭으로 열린다.
        let select = stage.cx.debug_bounds("repeater-method").unwrap();
        let menu = stage
            .cx
            .debug_bounds("repeater-method-menu")
            .expect("the list is open");
        assert!(menu.top() >= select.bottom());
        assert_eq!(menu.left(), select.left());
        assert!(menu.size.width >= select.size.width);
        // METHODS 의 넷째는 DELETE(인덱스 4).
        stage.click("repeater-method-option-4");
        assert_eq!(stage.active().method, "DELETE");
        // GPUI keeps the last bounds of an element that is gone, so ask the panel.
        let open = |stage: &mut Stage| {
            stage
                .panel
                .read_with(stage.cx, |panel, _| panel.method_open)
        };
        assert!(!open(&mut stage));

        // 밖을 누르면 고르지 않고 닫힌다.
        stage.click("repeater-method");
        assert!(open(&mut stage));
        stage.click("repeater-source");
        assert!(!open(&mut stage));
        assert_eq!(stage.active().method, "DELETE");
    }

    #[gpui::test]
    fn 본문을_고치면_스토어에_반영되고_다_지우면_본문이_없어진다(
        cx: &mut TestAppContext,
    ) {
        let mut stage = open(cx, Ok(ok_response()), true);
        stage.click("repeater-body");
        stage.cx.simulate_keystrokes("cmd-a");
        stage.cx.simulate_input("a=1");
        stage.cx.simulate_keystrokes("enter");
        stage.cx.simulate_input("b=2");
        assert_eq!(stage.active().body.as_deref(), Some("a=1\nb=2"));
        stage.cx.simulate_keystrokes("cmd-a backspace");
        assert_eq!(stage.active().body, None);
    }

    #[gpui::test]
    fn 보내기는_편집한_요청을_보내고_응답과_기록을_보여준다(
        cx: &mut TestAppContext,
    ) {
        let mut stage = open(cx, Ok(ok_response()), true);
        stage.click("repeater-url");
        stage.cx.simulate_keystrokes("end");
        stage.cx.simulate_input("/7");
        stage.click("repeater-send");
        let sent = stage.sender.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].url, "https://api.test/items/7");
        assert_eq!(sent[0].method, "POST");
        assert_eq!(sent[0].body.as_deref(), Some("{\"a\":1}"));

        stage.store.read_with(stage.cx, |store, _| {
            assert!(!store.loading());
            assert_eq!(store.history().len(), 1);
            assert_eq!(store.history()[0].request.url, "https://api.test/items/7");
            assert!(store.history()[0].ts > 0);
        });
        assert!(stage.shown("repeater-status"));
        assert!(stage.shown("repeater-history"));
        assert!(stage.shown("repeater-restore-0"));
        assert!(!stage.shown("repeater-error"));
        // 본문이 있으니 뷰어가 만들어졌다.
        assert!(
            stage
                .panel
                .read_with(stage.cx, |panel, _| panel.body_view.is_some())
        );
    }

    #[gpui::test]
    fn 보내는_동안에는_다시_보낼_수_없다(cx: &mut TestAppContext) {
        let stage = open(cx, Ok(ok_response()), true);
        stage.panel.update(stage.cx, |panel, cx| {
            panel.send(cx);
            // 답이 오기 전.
            assert!(panel.store.read(cx).loading());
            panel.send(cx);
        });
        stage.cx.run_until_parked();
        assert_eq!(stage.sender.sent().len(), 1);
        assert!(!stage.store.read_with(stage.cx, |store, _| store.loading()));
    }

    #[gpui::test]
    fn 응답_헤더는_눌러야_펼쳐지고_새로_보내면_다시_접힌다(
        cx: &mut TestAppContext,
    ) {
        let mut stage = open(cx, Ok(ok_response()), true);
        stage.click("repeater-send");
        assert!(!stage.shown("repeater-response-headers-text"));
        stage.click("repeater-response-headers");
        assert!(stage.shown("repeater-response-headers-text"));
        stage.click("repeater-send");
        // GPUI keeps the last bounds of an element that is gone, so ask the panel.
        assert!(
            !stage
                .panel
                .read_with(stage.cx, |panel, _| panel.headers_open)
        );
        assert_eq!(stage.sender.sent().len(), 2);
    }

    #[gpui::test]
    fn 응답_없이_끝난_전송은_오류와_함께_기록에_err로_남는다(
        cx: &mut TestAppContext,
    ) {
        let failed = RepeaterResponse {
            error: Some("net::ERR_CONNECTION_REFUSED".into()),
            ..RepeaterResponse::default()
        };
        let mut stage = open(cx, Ok(failed), true);
        stage.click("repeater-send");
        assert!(stage.shown("repeater-error"));
        assert!(stage.shown("repeater-status"));
        stage.store.read_with(stage.cx, |store, _| {
            assert_eq!(store.error(), Some("net::ERR_CONNECTION_REFUSED"));
            assert_eq!(store.history().len(), 1);
        });
        // 본문이 비었으니 뷰어 대신 (empty) 다.
        assert!(
            stage
                .panel
                .read_with(stage.cx, |panel, _| panel.body_view.is_none())
        );
    }

    #[gpui::test]
    fn 전송_호출이_실패하면_오류만_보이고_기록은_없다(cx: &mut TestAppContext) {
        let mut stage = open(cx, Err("ipc closed".into()), true);
        stage.click("repeater-send");
        assert!(stage.shown("repeater-error"));
        assert!(!stage.shown("repeater-status"));
        assert!(!stage.shown("repeater-history"));
    }

    #[gpui::test]
    fn 빈_오류_문구는_오류로_보이지_않는다(cx: &mut TestAppContext) {
        // `error && !loading`: 빈 문자열은 거짓이다.
        let mut stage = open(cx, Err(String::new()), true);
        stage.click("repeater-send");
        stage.store.read_with(stage.cx, |store, _| {
            assert_eq!(store.error(), Some(""));
            assert!(!store.loading());
        });
        assert!(!stage.shown("repeater-error"));
        assert!(!stage.shown("repeater-status"));
    }

    #[gpui::test]
    fn 복원_버튼은_그때_보낸_요청을_편집기에_되돌린다(cx: &mut TestAppContext) {
        let mut stage = open(cx, Ok(ok_response()), true);
        stage.click("repeater-send");
        stage.click("repeater-url");
        stage.cx.simulate_keystrokes("cmd-a");
        stage.cx.simulate_input("https://other.test/");
        stage.click("repeater-header-remove-0");
        assert_eq!(stage.active().url, "https://other.test/");
        assert_eq!(stage.header_fields().len(), 1);

        stage.click("repeater-restore-0");
        assert_eq!(stage.active().url, "https://api.test/items");
        stage.panel.read_with(stage.cx, |panel, cx| {
            assert_eq!(panel.url.read(cx).text(), "https://api.test/items");
        });
        assert_eq!(stage.header_fields().len(), 2);
    }

    #[gpui::test]
    fn 지우기_버튼은_요청을_내리고_안내로_돌아간다(cx: &mut TestAppContext) {
        let mut stage = open(cx, Ok(ok_response()), true);
        stage.click("repeater-send");
        stage.click("repeater-clear");
        assert!(stage.shown("repeater-empty"));
        stage.store.read_with(stage.cx, |store, _| {
            assert_eq!(store.active(), None);
            assert!(store.history().is_empty());
        });
        // 다시 불러오면 필드가 새로 채워진다.
        stage.load();
        assert_eq!(stage.header_fields().len(), 2);
    }

    #[gpui::test]
    fn 줄바꿈이_든_값도_한_줄_필드에_넣을_수_있고_고치기_전에는_스토어를_건드리지_않는다(
        cx: &mut TestAppContext,
    ) {
        let mut stage = open(cx, Ok(ok_response()), false);
        let mut request = stored();
        request.url = "https://api.test/a\nb".into();
        stage.store.update(stage.cx, |store, cx| {
            store.load_from_traffic("r1", Some(&request));
            cx.notify();
        });
        stage.cx.run_until_parked();
        stage.panel.read_with(stage.cx, |panel, cx| {
            assert_eq!(panel.url.read(cx).text(), "https://api.test/a\nb");
        });
        assert_eq!(stage.active().url, "https://api.test/a\nb");
        // 고치면 필드가 보여주던 대로(줄바꿈 없이) 스토어에 들어간다.
        stage.click("repeater-url");
        stage.cx.simulate_keystrokes("end");
        stage.cx.simulate_input("c");
        assert_eq!(stage.active().url, "https://api.test/abc");
    }
}
