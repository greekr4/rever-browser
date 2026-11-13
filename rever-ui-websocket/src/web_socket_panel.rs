//! Port of `src/renderer/src/components/panels/WebSocketPanel.tsx` — the WebSocket panel:
//! connections on the left, the selected connection's frames on the right, and a composer
//! that sends through the page's own socket.
//!
//! The React component reads its data over IPC. Here the connections and frames come
//! straight from the traffic store ([`list_connections`] and [`frames_since`] are the
//! `ws:list` and `ws:frames` handlers of `src/main/index.ts`), and the two calls that need
//! the page (`ws:sockets`, `ws:send`) are the host's [`WsHost`].
//!
//! Nothing of `src/renderer/src/lib/ws-sequence.ts` is ported here: the component does not
//! import it (step sequences belong to the WS Repeater panel).

use std::borrow::Cow;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use gpui::{
    AnyElement, AnyView, App, ClickEvent, Context, Div, ElementId, Entity, ListAlignment,
    ListOffset, ListState, Rgba, SharedString, Stateful, Window, div, list, prelude::*, px,
    relative, rgb,
};
use rever_core::traffic::{ListFilter, TrafficStore, WsDirection, WsFrame};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::Submitted;
use rever_ui_kit::theme::{Theme, ThemeMode};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::text_area::TextArea;

/// `listRequests({ limit: 200 })` behind `ws:list`: the newest 200 requests of any kind
/// are searched for WebSockets.
const LIST_LIMIT: usize = 200;
/// The default `limit` of `ws:frames`: one poll returns the newest 100 frames.
const FRAMES_LIMIT: usize = 100;
/// A frame row shows this many UTF-16 units of its payload, then an ellipsis.
const PAYLOAD_PREVIEW_UNITS: usize = 1024;
/// `rows={Math.min(6, …)}` of the composer.
const MAX_COMPOSER_ROWS: usize = 6;
/// Connections and frames are re-read every second, the page's open sockets every two.
const POLL: Duration = Duration::from_millis(1000);
const SOCKETS_POLL: Duration = Duration::from_millis(2000);
/// `WebSocket.OPEN`.
const READY_STATE_OPEN: u8 = 1;
/// The opcode of a binary frame.
const OPCODE_BINARY: u8 = 2;

/// `ui-monospace, monospace` and the browser's `normal` line height at 11px.
const MONO: &str = ".AppleSystemUIFontMonospaced";
const LINE_HEIGHT: f32 = 13.0;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WsConnection {
    pub request_id: String,
    pub url: String,
    pub started_at: u64,
}

/// A socket the page (or a cross-site iframe) holds, as `ws:sockets` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSocket {
    pub url: String,
    pub ready_state: u8,
    /// `"page"` for the top document, otherwise the frame it lives in.
    pub frame: String,
}

/// The arguments of `ws:send`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendRequest {
    pub url: String,
    pub payload: String,
    pub as_binary: bool,
    /// Reopen the socket from the page under the same URL when it is closed.
    pub reconnect: bool,
}

/// The answer of `ws:send`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SendOutcome {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub reconnected: bool,
}

/// What the panel needs from the page. Both calls may block: the panel runs them off the
/// main thread, as the Electron renderer awaits its IPC.
pub trait WsHost: Send + Sync + 'static {
    /// `window.rev.ws.sockets()`: the sockets still open in the page. An error keeps the
    /// panel's previous list.
    fn sockets(&self) -> Result<Vec<LiveSocket>, String>;
    /// `window.rev.ws.send(url, payload, asBinary, reconnect)`.
    fn send(&self, request: SendRequest) -> SendOutcome;
}

/// The `ws:list` handler: WebSocket requests among the newest [`LIST_LIMIT`], newest first.
pub fn list_connections(store: &TrafficStore) -> Vec<WsConnection> {
    let filter = ListFilter {
        limit: Some(LIST_LIMIT),
        ..ListFilter::default()
    };
    store
        .list(&filter)
        .into_iter()
        .filter(|request| request.resource_type == "WebSocket")
        .map(|request| WsConnection {
            request_id: request.request_id.clone(),
            url: request.url.clone(),
            started_at: request.started_at,
        })
        .collect()
}

/// The `ws:frames` handler: the newest [`FRAMES_LIMIT`] frames at or after `since`.
pub fn frames_since(store: &TrafficStore, request_id: &str, since: u64) -> Vec<WsFrame> {
    let frames = store.ws_frames(request_id, Some(since));
    let skip = frames.len().saturating_sub(FRAMES_LIMIT);
    frames.into_iter().skip(skip).cloned().collect()
}

/// host + path + query: two sockets on one host (a page and its chat widget) must stay
/// distinguishable. Anything `new URL()` rejects is shown as it is.
pub fn conn_label(url: &str) -> String {
    let Ok(parsed) = Url::parse(url) else {
        return url.to_owned();
    };
    let mut label = parsed.host_str().unwrap_or_default().to_owned();
    if let Some(port) = parsed.port() {
        label.push(':');
        label.push_str(&port.to_string());
    }
    label.push_str(parsed.path());
    // `URL.search` is empty for a bare "?".
    if let Some(query) = parsed.query().filter(|query| !query.is_empty()) {
        label.push('?');
        label.push_str(query);
    }
    label
}

/// The payload as a frame row shows it: cut after 1024 UTF-16 units, as
/// `payloadData.slice(0, 1024) + '…'` does. A surrogate pair across the cut is dropped
/// whole, where JavaScript would keep half of it.
pub fn display_payload(payload: &str) -> Cow<'_, str> {
    let mut units = 0;
    for (index, ch) in payload.char_indices() {
        units += ch.len_utf16();
        if units > PAYLOAD_PREVIEW_UNITS {
            return Cow::Owned(format!("{}…", &payload[..index]));
        }
    }
    Cow::Borrowed(payload)
}

/// The component's state and the rules that change it, without any interface.
#[derive(Default)]
pub struct PanelState {
    connections: Vec<WsConnection>,
    selected_id: Option<String>,
    frames: Vec<WsFrame>,
    /// The next poll asks for frames at or after this timestamp.
    frame_last_ts: u64,
    /// Counts the times `frames` was emptied, so a list view can tell "restarted" from "grew".
    frames_generation: u64,
    sending: bool,
    send_result: Option<String>,
    live_sockets: Vec<LiveSocket>,
}

impl PanelState {
    pub fn connections(&self) -> &[WsConnection] {
        &self.connections
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.selected_id.as_deref()
    }

    pub fn frames(&self) -> &[WsFrame] {
        &self.frames
    }

    pub fn frames_generation(&self) -> u64 {
        self.frames_generation
    }

    pub fn sending(&self) -> bool {
        self.sending
    }

    /// What the toolbar shows after a send. An empty error text shows nothing, as an
    /// empty string is falsy in the component.
    pub fn send_result(&self) -> Option<&str> {
        self.send_result.as_deref().filter(|text| !text.is_empty())
    }

    /// One tick of the two 1-second polls: connections, then the selected one's frames.
    /// Returns (something to redraw, frames were appended).
    pub fn refresh(&mut self, store: &TrafficStore) -> (bool, bool) {
        let generation = self.frames_generation;
        let changed = self.set_connections(list_connections(store));
        let appended = self.poll_frames(store) > 0;
        (
            changed || appended || generation != self.frames_generation,
            appended,
        )
    }

    /// The connection poll. A list with the same ids in the same order changes nothing.
    pub fn set_connections(&mut self, list: Vec<WsConnection>) -> bool {
        let same = list.len() == self.connections.len()
            && list
                .iter()
                .zip(&self.connections)
                .all(|(new, old)| new.request_id == old.request_id);
        if same {
            return false;
        }
        self.connections = list;
        self.auto_select();
        true
    }

    /// Auto-select the first connection when the selected one is gone.
    fn auto_select(&mut self) {
        let still_open = self.selected_id.as_ref().is_some_and(|selected| {
            self.connections
                .iter()
                .any(|connection| &connection.request_id == selected)
        });
        if !still_open {
            let first = self.connections.first().map(|c| c.request_id.clone());
            self.set_selected(first);
        }
    }

    fn set_selected(&mut self, id: Option<String>) {
        if self.selected_id == id {
            return;
        }
        self.selected_id = id;
        // The component's frame effect returns early without a selection, before it
        // empties the list: frames of a vanished connection stay on screen.
        if self.selected_id.is_some() {
            self.frame_last_ts = 0;
            self.frames.clear();
            self.frames_generation += 1;
        }
    }

    /// A click on a connection row. Clicking the selected row again keeps its frames.
    pub fn select(&mut self, request_id: &str) {
        self.set_selected(Some(request_id.to_owned()));
    }

    /// The frame poll: appends what arrived since the last one and returns how many.
    pub fn poll_frames(&mut self, store: &TrafficStore) -> usize {
        let Some(selected) = &self.selected_id else {
            return 0;
        };
        let next = frames_since(store, selected, self.frame_last_ts);
        if let Some(last) = next.last() {
            // A timestamp at the top of the range must not overflow the cursor.
            self.frame_last_ts = last.timestamp.saturating_add(1);
        }
        let count = next.len();
        self.frames.extend(next);
        count
    }

    pub fn selected_url(&self) -> Option<&str> {
        let selected = self.selected_id.as_ref()?;
        self.connections
            .iter()
            .find(|connection| &connection.request_id == selected)
            .map(|connection| connection.url.as_str())
    }

    fn open_socket(&self, url: &str) -> Option<&LiveSocket> {
        self.live_sockets
            .iter()
            .find(|socket| socket.url == url && socket.ready_state == READY_STATE_OPEN)
    }

    /// The open page socket of the selected connection, if the page still holds one.
    pub fn live(&self) -> Option<&LiveSocket> {
        self.open_socket(self.selected_url()?)
    }

    /// The live indicator's string key; nothing without a selected connection.
    pub fn live_label_key(&self) -> Option<&'static str> {
        self.selected_url()?;
        Some(match self.live() {
            Some(socket) if socket.frame == "page" => "panel.wsSender.live",
            Some(_) => "panel.wsSender.liveFrame",
            None => "panel.wsSender.noLive",
        })
    }

    /// The send button's string key.
    pub fn send_label_key(&self) -> &'static str {
        if self.sending {
            "panel.wsSender.sending"
        } else if self.live().is_some() {
            "panel.wsSender.send"
        } else {
            "panel.wsSender.reconnectSend"
        }
    }

    /// Whether the composer may send `payload` now (the button's `disabled`, negated).
    pub fn can_send(&self, payload: &str) -> bool {
        self.selected_url().is_some() && !payload.is_empty() && !self.sending
    }

    /// Start a send on `url`. Without an open socket the page reopens the same URL first.
    pub fn begin_send(&mut self, url: &str, payload: String, as_binary: bool) -> SendRequest {
        self.sending = true;
        SendRequest {
            url: url.to_owned(),
            payload,
            as_binary,
            reconnect: self.open_socket(url).is_none(),
        }
    }

    /// The send on `url` came back.
    pub fn finish_send(
        &mut self,
        url: &str,
        outcome: &SendOutcome,
        lang: Lang,
        store: &TrafficStore,
    ) {
        self.send_result = Some(if outcome.ok {
            lang.t(if outcome.reconnected {
                "panel.wsSender.reconnectSent"
            } else {
                "panel.wsSender.sent"
            })
            .to_owned()
        } else {
            outcome.error.clone().unwrap_or_else(|| "error".to_owned())
        });
        if outcome.reconnected {
            // A reconnect is a new connection (new request id): move to it so the reply
            // is visible. The list is newest first.
            self.connections = list_connections(store);
            let reopened = self
                .connections
                .iter()
                .find(|connection| connection.url == url)
                .map(|connection| connection.request_id.clone());
            if reopened.is_some() {
                self.set_selected(reopened);
            }
            self.auto_select();
        }
        self.sending = false;
    }

    /// The live-socket poll. Returns whether the list changed.
    pub fn set_live_sockets(&mut self, sockets: Vec<LiveSocket>) -> bool {
        if self.live_sockets == sockets {
            return false;
        }
        self.live_sockets = sockets;
        true
    }
}

/// Tokens of `src/renderer/src/styles.css` that `Theme` does not carry yet.
struct ExtraColors {
    /// `--row-active`
    row_active: Rgba,
    /// `--status-info`
    status_info: Rgba,
    /// `--status-ok`
    status_ok: Rgba,
}

impl ExtraColors {
    fn of(theme: &Theme) -> Self {
        // `Theme` does not say which variant it is; its background does.
        if theme.bg == Theme::dark().bg {
            Self {
                row_active: rgb(0x1d2a3a),
                status_info: rgb(0x74b9ff),
                status_ok: rgb(0x6ee7b7),
            }
        } else {
            Self {
                row_active: rgb(0xe3edfa),
                status_info: rgb(0x1a5fb4),
                status_ok: rgb(0x1f7a34),
            }
        }
    }
}

/// `filter: brightness(1.1)` on a hovered primary button.
fn brighter(color: Rgba) -> Rgba {
    let up = |channel: f32| (channel * 1.1).min(1.0);
    Rgba {
        r: up(color.r),
        g: up(color.g),
        b: up(color.b),
        a: color.a,
    }
}

fn lock(store: &Mutex<TrafficStore>) -> MutexGuard<'_, TrafficStore> {
    // A panic elsewhere while the store was locked must not take the panel down with it.
    store.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a `title` attribute shows on hover.
struct Tooltip {
    text: SharedString,
    theme: Theme,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(480.0))
            .px(px(6.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(self.theme.border_2)
            .bg(self.theme.surface_3)
            .text_color(self.theme.text)
            .text_size(px(11.0))
            .child(self.text.clone())
    }
}

fn tooltip(
    text: impl Into<SharedString>,
    theme: &Theme,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let (text, theme) = (text.into(), *theme);
    move |_, cx| {
        cx.new(|_| Tooltip {
            text: text.clone(),
            theme,
        })
        .into()
    }
}

/// `.panel-empty`
fn empty(text: &'static str, theme: &Theme) -> Div {
    div()
        .flex_none()
        .px(px(12.0))
        .py(px(24.0))
        .text_center()
        .text_color(theme.text_dim)
        .text_size(px(11.0))
        .line_height(px(LINE_HEIGHT))
        .child(text)
}

/// `button.btn-ghost.btn-icon`
fn ghost_button(
    id: impl Into<ElementId>,
    glyph: &'static str,
    title: &'static str,
    theme: &Theme,
) -> Stateful<Div> {
    let (hover, hover_text) = (theme.chrome_hover, theme.text);
    div()
        .id(id)
        .min_w(px(22.0))
        .p(px(3.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .text_size(px(12.0))
        .line_height(px(16.0))
        .text_color(theme.text_2)
        .cursor_pointer()
        .hover(move |style| style.bg(hover).text_color(hover_text))
        .tooltip(tooltip(title, theme))
        .child(glyph)
}

pub struct WebSocketPanel {
    store: Arc<Mutex<TrafficStore>>,
    host: Arc<dyn WsHost>,
    lang: Lang,
    theme_mode: ThemeMode,
    state: PanelState,
    composer: Entity<TextArea>,
    frame_list: ListState,
    /// The `frames_generation` the list element was last told about.
    listed_generation: u64,
}

impl WebSocketPanel {
    pub fn new(
        store: Arc<Mutex<TrafficStore>>,
        host: Arc<dyn WsHost>,
        lang: Lang,
        theme_mode: ThemeMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer =
            cx.new(|cx| TextArea::new(lang.t("panel.wsSender.payload"), MAX_COMPOSER_ROWS, cx));
        // Enter sends through the page's own socket (auth preserved); Shift+Enter is a line break.
        cx.subscribe(&composer, |this, _, _: &Submitted, cx| this.send(cx))
            .detach();
        // The send button is enabled by the composer's text.
        cx.observe(&composer, |_, _, cx| cx.notify()).detach();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        let poll_host = host.clone();
        cx.spawn(async move |this, cx| {
            loop {
                let host = poll_host.clone();
                let sockets = cx
                    .background_executor()
                    .spawn(async move { host.sockets() })
                    .await;
                let alive = this.update(cx, |this, cx| {
                    if sockets.is_ok_and(|sockets| this.state.set_live_sockets(sockets)) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
                cx.background_executor().timer(SOCKETS_POLL).await;
            }
        })
        .detach();

        let mut panel = Self {
            store,
            host,
            lang,
            theme_mode,
            state: PanelState::default(),
            composer,
            frame_list: ListState::new(0, ListAlignment::Top, px(200.0)),
            listed_generation: 0,
        };
        panel.refresh(cx);
        panel
    }

    /// Re-read connections and frames from the traffic store. The panel does this every
    /// second on its own; the host may call it as soon as it records traffic.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let (changed, appended) = self.state.refresh(&lock(&self.store));
        self.sync_frame_list(appended);
        if changed {
            cx.notify();
        }
    }

    /// Put `text` in the composer, as the edit button of a frame does.
    pub fn set_payload(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.composer
            .update(cx, |composer, cx| composer.set_text(text, cx));
    }

    /// Tell the list element how `frames` changed; new frames pull it to the bottom.
    fn sync_frame_list(&mut self, appended: bool) {
        let count = self.state.frames().len();
        let listed = self.frame_list.item_count();
        if self.listed_generation != self.state.frames_generation() {
            self.listed_generation = self.state.frames_generation();
            self.frame_list.reset(count);
        } else if count > listed {
            self.frame_list.splice(listed..listed, count - listed);
        }
        if appended {
            // Past the last item: the list fills the view upward from the end.
            self.frame_list.scroll_to(ListOffset {
                item_ix: count,
                offset_in_item: px(0.0),
            });
        }
    }

    fn select(&mut self, request_id: &str, cx: &mut Context<Self>) {
        self.state.select(request_id);
        let appended = self.state.poll_frames(&lock(&self.store)) > 0;
        self.sync_frame_list(appended);
        cx.notify();
    }

    fn send_on(&mut self, url: String, payload: String, as_binary: bool, cx: &mut Context<Self>) {
        let request = self.state.begin_send(&url, payload, as_binary);
        let host = self.host.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { host.send(request) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let generation = this.state.frames_generation();
                this.state
                    .finish_send(&url, &outcome, this.lang, &lock(&this.store));
                // Moving to the reopened connection reads its frames at once, as a
                // click does; otherwise the next poll shows what was sent.
                let appended = generation != this.state.frames_generation()
                    && this.state.poll_frames(&lock(&this.store)) > 0;
                this.sync_frame_list(appended);
                cx.notify();
            });
        })
        .detach();
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let payload = self.composer.read(cx).text().to_owned();
        if !self.state.can_send(&payload) {
            return;
        }
        if let Some(url) = self.state.selected_url().map(str::to_owned) {
            self.send_on(url, payload, false, cx);
        }
    }

    /// Resend: the same content again, through the socket of the connection it travelled on.
    fn resend(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(url) = self.state.selected_url().map(str::to_owned) else {
            return;
        };
        let Some(frame) = self.state.frames().get(index) else {
            return;
        };
        let (payload, as_binary) = (frame.payload_data.clone(), frame.opcode == OPCODE_BINARY);
        self.send_on(url, payload, as_binary, cx);
    }

    /// Tamper: lift the payload into the composer and focus it — edit, then Send.
    fn edit_frame(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(frame) = self.state.frames().get(index) else {
            return;
        };
        let payload = frame.payload_data.clone();
        self.composer.update(cx, |composer, cx| {
            composer.set_text(payload, cx);
            composer.focus(window);
        });
    }

    fn theme(&self, window: &Window) -> Theme {
        Theme::resolve(self.theme_mode, window.appearance())
    }

    fn connection_list(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let row_active = ExtraColors::of(theme).row_active;
        div()
            .id("ws-connections")
            .w(px(180.0))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(theme.border)
            .overflow_y_scroll()
            .font_family(MONO)
            .text_size(px(11.0))
            .line_height(px(LINE_HEIGHT))
            .when(self.state.connections().is_empty(), |list| {
                list.child(empty(self.lang.t("panel.noWs"), theme))
            })
            .children(
                self.state
                    .connections()
                    .iter()
                    .enumerate()
                    .map(|(index, connection)| {
                        let selected =
                            self.state.selected_id() == Some(connection.request_id.as_str());
                        let request_id = connection.request_id.clone();
                        div()
                            .id(("ws-connection", index))
                            .debug_selector(move || format!("ws-connection-{index}"))
                            .px(px(8.0))
                            .py(px(6.0))
                            .cursor_pointer()
                            .when(selected, |row| row.bg(row_active))
                            .border_b_1()
                            .border_color(theme.border)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.select(&request_id, cx)
                            }))
                            .child(conn_label(&connection.url))
                    }),
            )
    }

    /// `.panel-toolbar`: which page socket the composer sends through.
    fn toolbar(&self, theme: &Theme) -> impl IntoElement {
        let lang = self.lang;
        let colors = ExtraColors::of(theme);
        let url: Option<SharedString> = self.state.selected_url().map(|url| url.to_owned().into());
        let target = div()
            .id("ws-target")
            .flex_1()
            .min_w_0()
            .truncate()
            .font_family(MONO)
            .child(url.clone().unwrap_or_else(|| "—".into()));
        let live_color = if self.state.live().is_some() {
            colors.status_ok
        } else {
            theme.text_dim
        };
        let result: Option<SharedString> =
            self.state.send_result().map(|text| text.to_owned().into());
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(11.0))
            .line_height(px(LINE_HEIGHT))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text_dim)
                    .child(lang.t("panel.wsSender.target")),
            )
            .child(match url {
                Some(url) => target.tooltip(tooltip(url, theme)),
                None => target,
            })
            .when_some(self.state.live_label_key(), |bar, key| {
                bar.child(
                    div()
                        .debug_selector(|| "ws-live".into())
                        .flex_none()
                        .text_color(live_color)
                        .child(lang.t(key)),
                )
            })
            .when_some(result, |bar, result| {
                bar.child(
                    div()
                        .id("ws-send-result")
                        .max_w(relative(0.35))
                        .truncate()
                        .text_color(theme.text_dim)
                        .tooltip(tooltip(result.clone(), theme))
                        .child(result),
                )
            })
    }

    fn frame_row(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(frame) = self.state.frames().get(index) else {
            return div().into_any_element();
        };
        let lang = self.lang;
        let colors = ExtraColors::of(theme);
        let sent = frame.direction == WsDirection::Sent;
        div()
            .w_full()
            .px(px(8.0))
            .py(px(3.0))
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .items_start()
            .gap(px(6.0))
            .font_family(MONO)
            .text_size(px(11.0))
            .line_height(px(LINE_HEIGHT))
            .child(
                div()
                    .flex_none()
                    .text_size(px(10.0))
                    .text_color(if sent {
                        colors.status_info
                    } else {
                        colors.status_ok
                    })
                    .child(if sent { "↑" } else { "↓" }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(theme.text)
                    .child(SharedString::from(
                        display_payload(&frame.payload_data).into_owned(),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(2.0))
                    .child(
                        ghost_button(
                            ("ws-resend", index),
                            "↻",
                            lang.t("panel.wsSender.resend"),
                            theme,
                        )
                        .debug_selector(move || format!("ws-resend-{index}"))
                        .on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| this.resend(index, cx)),
                        ),
                    )
                    .child(
                        ghost_button(
                            ("ws-edit", index),
                            "✎",
                            lang.t("panel.wsSender.edit"),
                            theme,
                        )
                        .debug_selector(move || format!("ws-edit-{index}"))
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| {
                                this.edit_frame(index, window, cx)
                            },
                        )),
                    ),
            )
            .into_any_element()
    }

    fn frames(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let lang = self.lang;
        let selected = self.state.selected_id().is_some();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .font_family(MONO)
            .when(self.state.frames().is_empty() && selected, |area| {
                area.child(empty(lang.t("panel.noFrames"), theme))
            })
            .when(!selected, |area| {
                area.child(empty(lang.t("panel.selectConnection"), theme))
            })
            .child(
                // Rows differ in height and there may be thousands: only the visible
                // ones are laid out.
                list(
                    self.frame_list.clone(),
                    cx.processor(|this, index: usize, window: &mut Window, cx| {
                        let theme = this.theme(window);
                        this.frame_row(index, &theme, cx)
                    }),
                )
                .flex_1()
                .min_h_0(),
            )
    }

    /// The composer — like the Console input.
    fn composer(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = self.state.can_send(self.composer.read(cx).text());
        let accent = theme.accent;
        let send = div()
            .id("ws-send")
            .debug_selector(|| "ws-send".into())
            .m(px(4.0))
            .px(px(8.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .bg(accent)
            .text_color(rgb(0xffffff))
            .text_size(px(12.0))
            .line_height(px(16.0))
            .child(self.lang.t(self.state.send_label_key()));
        let send = if enabled {
            send.cursor_pointer()
                .hover(move |style| style.bg(brighter(accent)))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.send(cx)))
        } else {
            send.opacity(0.42)
        };
        div()
            .flex()
            .flex_none()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.bg)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(10.0))
                    .py(px(4.0))
                    .font_family(MONO)
                    .text_size(px(11.0))
                    .line_height(px(LINE_HEIGHT))
                    .text_color(theme.text)
                    .child(self.composer.clone()),
            )
            // `align-self: flex-end`
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .justify_end()
                    .child(send),
            )
    }
}

impl Render for WebSocketPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme(window);
        div()
            .size_full()
            .min_h_0()
            .flex()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(px(13.0))
            .font_family(".SystemUIFont")
            .child(self.connection_list(&theme, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.toolbar(&theme))
                    .child(self.frames(&theme, cx))
                    .child(self.composer(&theme, cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use rever_core::traffic::RequestPatch;

    use super::*;

    fn request(store: &mut TrafficStore, id: &str, url: &str, kind: &str, started_at: u64) {
        store.upsert(
            RequestPatch {
                request_id: id.to_owned(),
                url: Some(url.to_owned()),
                resource_type: Some(kind.to_owned()),
                started_at: Some(started_at),
                ..RequestPatch::default()
            },
            started_at,
        );
    }

    fn socket(store: &mut TrafficStore, id: &str, url: &str, started_at: u64) {
        request(store, id, url, "WebSocket", started_at);
    }

    fn frame(store: &mut TrafficStore, id: &str, direction: WsDirection, text: &str, ts: u64) {
        store.append_ws_frame(
            id,
            WsFrame {
                direction,
                opcode: 1,
                payload_data: text.to_owned(),
                timestamp: ts,
                mask: None,
            },
        );
    }

    fn live(url: &str, ready_state: u8, frame: &str) -> LiveSocket {
        LiveSocket {
            url: url.to_owned(),
            ready_state,
            frame: frame.to_owned(),
        }
    }

    fn connection(id: &str, url: &str) -> WsConnection {
        WsConnection {
            request_id: id.to_owned(),
            url: url.to_owned(),
            started_at: 0,
        }
    }

    fn payloads(state: &PanelState) -> Vec<&str> {
        state
            .frames()
            .iter()
            .map(|frame| frame.payload_data.as_str())
            .collect()
    }

    // ── connLabel ───────────────────────────────────────────────────────────

    #[test]
    fn 연결_라벨은_호스트와_경로와_쿼리다() {
        assert_eq!(
            conn_label("wss://chat.example.com/socket?room=7&v=2"),
            "chat.example.com/socket?room=7&v=2"
        );
        // 경로가 없으면 URL 이 "/" 를 채운다.
        assert_eq!(conn_label("wss://example.com"), "example.com/");
    }

    #[test]
    fn 연결_라벨은_기본이_아닌_포트를_남긴다() {
        assert_eq!(conn_label("ws://127.0.0.1:8777/ws"), "127.0.0.1:8777/ws");
        // 스킴의 기본 포트는 URL 이 지운다.
        assert_eq!(conn_label("wss://example.com:443/ws"), "example.com/ws");
        assert_eq!(conn_label("ws://[::1]:9000/x"), "[::1]:9000/x");
    }

    #[test]
    fn 같은_호스트의_두_소켓은_경로와_쿼리로_구분된다() {
        let page = conn_label("wss://example.com/live?channel=page");
        let widget = conn_label("wss://example.com/live?channel=widget");
        assert_ne!(page, widget);
    }

    #[test]
    fn url이_아닌_문자열은_그대로_라벨이_된다() {
        assert_eq!(conn_label("not a url"), "not a url");
        assert_eq!(conn_label(""), "");
        assert_eq!(conn_label("/relative/path"), "/relative/path");
    }

    #[test]
    fn 빈_쿼리와_프래그먼트는_라벨에_들어가지_않는다() {
        assert_eq!(conn_label("wss://example.com/ws?"), "example.com/ws");
        assert_eq!(
            conn_label("wss://example.com/ws?a=1#top"),
            "example.com/ws?a=1"
        );
        assert_eq!(conn_label("wss://user:pw@example.com/ws"), "example.com/ws");
    }

    #[test]
    fn 연결_라벨은_new_url이_정규화한_모습을_따른다() {
        // 기대값은 원본 connLabel 을 bun 과 node 로 돌려 얻었다.
        let cases = [
            ("WSS://EXAMPLE.com/A b?c d", "example.com/A%20b?c%20d"),
            (
                "wss://例え.jp/パス?q=値",
                "xn--r8jz45g.jp/%E3%83%91%E3%82%B9?q=%E5%80%A4",
            ),
            ("  wss://example.com/x  ", "example.com/x"),
            ("wss://example.com/a/../b/./c", "example.com/b/c"),
            ("wss://example.com?x", "example.com/?x"),
            ("wss://example.com/ws?#", "example.com/ws"),
            ("ws://example.com:80/x", "example.com/x"),
            ("ws://example.com:443/x", "example.com:443/x"),
            ("wss:example.com/x", "example.com/x"),
            ("ws://0x7f.1/x", "127.0.0.1/x"),
            ("ws://example.com/\\a\\b", "example.com//a/b"),
            ("ws://example.com/\tx\ny", "example.com/xy"),
            (
                "wss://example.com/ws?a=1&b='\"<>",
                "example.com/ws?a=1&b=%27%22%3C%3E",
            ),
            ("ws://[::ffff:1.2.3.4]/", "[::ffff:102:304]/"),
            // 호스트가 없는 스킴은 경로와 쿼리만 남는다.
            ("blob:https://example.com/uuid", "https://example.com/uuid"),
            ("data:text/plain,hi?x", "text/plain,hi?x"),
            ("file:///tmp/x?y", "/tmp/x?y"),
            ("about:blank", "blank"),
            // new URL 이 던지는 문자열은 그대로.
            ("ws://example.com:99999/", "ws://example.com:99999/"),
            ("ws://exa mple.com/", "ws://exa mple.com/"),
            ("ws://", "ws://"),
            ("ws://?x", "ws://?x"),
        ];
        for (url, label) in cases {
            assert_eq!(conn_label(url), label, "{url}");
        }
    }

    // ── frame payload preview ───────────────────────────────────────────────

    #[test]
    fn 페이로드가_1024자_이하면_그대로_보인다() {
        let text = "a".repeat(1024);
        assert_eq!(display_payload(&text), text);
        assert_eq!(display_payload(""), "");
        assert!(matches!(display_payload(&text), Cow::Borrowed(_)));
    }

    #[test]
    fn 페이로드가_1024자를_넘으면_잘라내고_말줄임표를_붙인다() {
        let text = "a".repeat(1025);
        assert_eq!(display_payload(&text), format!("{}…", "a".repeat(1024)));
        let long = "b".repeat(5000);
        assert_eq!(display_payload(&long).chars().count(), 1025);
    }

    #[test]
    fn 페이로드_길이는_바이트가_아니라_utf16_단위로_센다() {
        // 한글 1024자는 3072바이트지만 UTF-16 으로는 1024 단위다.
        let hangul = "가".repeat(1024);
        assert_eq!(display_payload(&hangul), hangul);
        let over = "가".repeat(1025);
        assert_eq!(display_payload(&over), format!("{}…", "가".repeat(1024)));
        // 이모지는 2 단위: 512개가 한도, 513개째부터 잘린다.
        let emoji = "😀".repeat(512);
        assert_eq!(display_payload(&emoji), emoji);
        let emoji_over = "😀".repeat(513);
        assert_eq!(
            display_payload(&emoji_over),
            format!("{}…", "😀".repeat(512))
        );
    }

    #[test]
    fn 자르는_자리에_걸친_서로게이트_쌍은_통째로_빠진다() {
        // 1023 단위 + 이모지(2 단위) = 1025: JS 는 반쪽을 남기지만 여기서는 깨진 글자를 만들지 않는다.
        let text = format!("{}😀", "a".repeat(1023));
        assert_eq!(display_payload(&text), format!("{}…", "a".repeat(1023)));
    }

    // ── ws:list / ws:frames ─────────────────────────────────────────────────

    #[test]
    fn 연결_목록은_웹소켓만_최신순으로_담는다() {
        let mut store = TrafficStore::default();
        socket(&mut store, "1", "wss://a.example/ws", 10);
        request(&mut store, "2", "https://a.example/api", "Fetch", 20);
        socket(&mut store, "3", "wss://b.example/ws", 30);
        let list = list_connections(&store);
        assert_eq!(
            list,
            vec![
                WsConnection {
                    request_id: "3".into(),
                    url: "wss://b.example/ws".into(),
                    started_at: 30
                },
                WsConnection {
                    request_id: "1".into(),
                    url: "wss://a.example/ws".into(),
                    started_at: 10
                },
            ]
        );
    }

    #[test]
    fn 연결_목록은_최근_요청_200개_안에서만_찾는다() {
        let mut store = TrafficStore::default();
        socket(&mut store, "old", "wss://old.example/ws", 1);
        for index in 0..200 {
            request(
                &mut store,
                &format!("r{index}"),
                "https://x.example/",
                "Fetch",
                2,
            );
        }
        // 웹소켓이 200개의 다른 요청에 밀려나면 목록에서 사라진다(필터가 limit 뒤에 걸린다).
        assert!(list_connections(&store).is_empty());
        socket(&mut store, "new", "wss://new.example/ws", 3);
        let ids: Vec<String> = list_connections(&store)
            .into_iter()
            .map(|connection| connection.request_id)
            .collect();
        assert_eq!(ids, ["new"]);
    }

    #[test]
    fn 프레임은_since_이후만_오래된_순으로_준다() {
        let mut store = TrafficStore::default();
        for ts in [100, 200, 300] {
            frame(&mut store, "1", WsDirection::Received, &ts.to_string(), ts);
        }
        let texts = |since| -> Vec<String> {
            frames_since(&store, "1", since)
                .into_iter()
                .map(|frame| frame.payload_data)
                .collect()
        };
        assert_eq!(texts(0), ["100", "200", "300"]);
        // 경계 시각은 포함된다.
        assert_eq!(texts(200), ["200", "300"]);
        assert_eq!(texts(301), Vec::<String>::new());
        assert!(frames_since(&store, "none", 0).is_empty());
    }

    #[test]
    fn 프레임은_한_번에_최근_100개까지만_준다() {
        let mut store = TrafficStore::default();
        for ts in 1..=250 {
            frame(&mut store, "1", WsDirection::Sent, &ts.to_string(), ts);
        }
        let frames = frames_since(&store, "1", 0);
        assert_eq!(frames.len(), 100);
        assert_eq!(frames.first().map(|f| f.timestamp), Some(151));
        assert_eq!(frames.last().map(|f| f.timestamp), Some(250));
    }

    // ── connection poll + auto-select ───────────────────────────────────────

    #[test]
    fn 연결이_생기면_첫_연결을_자동으로_고른다() {
        let mut state = PanelState::default();
        assert_eq!(state.selected_id(), None);
        assert!(state.set_connections(vec![
            connection("b", "wss://b.example/"),
            connection("a", "wss://a.example/"),
        ]));
        assert_eq!(state.selected_id(), Some("b"));
        assert_eq!(state.selected_url(), Some("wss://b.example/"));
    }

    #[test]
    fn 같은_id_순서의_목록은_아무것도_바꾸지_않는다() {
        let mut state = PanelState::default();
        state.set_connections(vec![connection("a", "wss://a.example/")]);
        // id 만 비교한다: URL 이 달라도 같은 목록으로 본다.
        assert!(!state.set_connections(vec![connection("a", "wss://changed.example/")]));
        assert_eq!(state.selected_url(), Some("wss://a.example/"));
        // 순서가 바뀌면 다른 목록이다.
        state.set_connections(vec![connection("a", "x"), connection("b", "y")]);
        assert!(state.set_connections(vec![connection("b", "y"), connection("a", "x")]));
    }

    #[test]
    fn 새_연결이_앞에_와도_고른_연결은_유지된다() {
        let mut state = PanelState::default();
        state.set_connections(vec![connection("a", "wss://a.example/")]);
        state.set_connections(vec![
            connection("b", "wss://b.example/"),
            connection("a", "wss://a.example/"),
        ]);
        assert_eq!(state.selected_id(), Some("a"));
    }

    #[test]
    fn 고른_연결이_사라지면_첫_연결로_옮긴다() {
        let mut state = PanelState::default();
        state.set_connections(vec![connection("a", "x"), connection("b", "y")]);
        state.select("b");
        state.set_connections(vec![connection("c", "z"), connection("a", "x")]);
        assert_eq!(state.selected_id(), Some("c"));
    }

    #[test]
    fn 연결이_모두_사라지면_선택은_비고_프레임은_화면에_남는다() {
        let mut store = TrafficStore::default();
        socket(&mut store, "a", "wss://a.example/", 1);
        frame(&mut store, "a", WsDirection::Received, "hello", 5);
        let mut state = PanelState::default();
        state.refresh(&store);
        assert_eq!(payloads(&state), ["hello"]);
        let generation = state.frames_generation();

        store.clear();
        let (changed, appended) = state.refresh(&store);
        assert!(changed);
        assert!(!appended);
        assert_eq!(state.selected_id(), None);
        assert_eq!(state.selected_url(), None);
        // 원본의 프레임 effect 는 선택이 없으면 비우기 전에 돌아간다.
        assert_eq!(payloads(&state), ["hello"]);
        assert_eq!(state.frames_generation(), generation);
    }

    // ── selection + frame poll ──────────────────────────────────────────────

    #[test]
    fn 프레임_폴링은_새로_온_것만_이어_붙인다() {
        let mut store = TrafficStore::default();
        socket(&mut store, "a", "wss://a.example/", 1);
        frame(&mut store, "a", WsDirection::Sent, "one", 10);
        frame(&mut store, "a", WsDirection::Received, "two", 20);
        let mut state = PanelState::default();
        assert_eq!(state.refresh(&store), (true, true));
        assert_eq!(payloads(&state), ["one", "two"]);

        // 바뀐 것이 없으면 다시 그릴 것도 없다.
        assert_eq!(state.refresh(&store), (false, false));
        assert_eq!(payloads(&state), ["one", "two"]);

        frame(&mut store, "a", WsDirection::Received, "three", 30);
        assert_eq!(state.refresh(&store), (true, true));
        assert_eq!(payloads(&state), ["one", "two", "three"]);
    }

    #[test]
    fn 마지막_프레임과_같은_밀리초에_늦게_온_프레임은_건너뛴다() {
        // 원본의 커서는 "마지막 시각 + 1" 이다.
        let mut store = TrafficStore::default();
        socket(&mut store, "a", "wss://a.example/", 1);
        frame(&mut store, "a", WsDirection::Sent, "first", 10);
        let mut state = PanelState::default();
        state.refresh(&store);
        frame(&mut store, "a", WsDirection::Received, "same-ms", 10);
        frame(&mut store, "a", WsDirection::Received, "next-ms", 11);
        assert_eq!(state.poll_frames(&store), 1);
        assert_eq!(payloads(&state), ["first", "next-ms"]);
    }

    #[test]
    fn 시각이_u64_끝인_프레임도_커서를_넘치게_하지_않는다() {
        let mut store = TrafficStore::default();
        socket(&mut store, "a", "wss://a.example/", 1);
        frame(&mut store, "a", WsDirection::Received, "last", u64::MAX);
        let mut state = PanelState::default();
        state.refresh(&store);
        assert_eq!(payloads(&state), ["last"]);
        state.poll_frames(&store);
    }

    #[test]
    fn 처음_폴링은_최근_100개만_가져온다() {
        let mut store = TrafficStore::default();
        socket(&mut store, "a", "wss://a.example/", 1);
        for ts in 1..=130 {
            frame(&mut store, "a", WsDirection::Received, &ts.to_string(), ts);
        }
        let mut state = PanelState::default();
        state.refresh(&store);
        assert_eq!(state.frames().len(), 100);
        assert_eq!(payloads(&state).first(), Some(&"31"));
    }

    #[test]
    fn 다른_연결을_고르면_프레임을_비우고_그_연결의_것을_읽는다() {
        let mut store = TrafficStore::default();
        socket(&mut store, "a", "wss://a.example/", 1);
        socket(&mut store, "b", "wss://b.example/", 2);
        frame(&mut store, "a", WsDirection::Sent, "from-a", 10);
        frame(&mut store, "b", WsDirection::Sent, "from-b", 5);
        let mut state = PanelState::default();
        state.refresh(&store);
        // 목록은 최신순이라 b 가 먼저 골라진다.
        assert_eq!(state.selected_id(), Some("b"));
        assert_eq!(payloads(&state), ["from-b"]);

        let generation = state.frames_generation();
        state.select("a");
        assert!(state.frames().is_empty());
        assert_eq!(state.frames_generation(), generation + 1);
        // 커서도 0 으로 돌아가서 b 의 마지막 시각보다 이른 프레임도 읽힌다.
        assert_eq!(state.poll_frames(&store), 1);
        assert_eq!(payloads(&state), ["from-a"]);
    }

    #[test]
    fn 이미_고른_연결을_다시_눌러도_프레임은_그대로다() {
        let mut store = TrafficStore::default();
        socket(&mut store, "a", "wss://a.example/", 1);
        frame(&mut store, "a", WsDirection::Sent, "kept", 10);
        let mut state = PanelState::default();
        state.refresh(&store);
        let generation = state.frames_generation();
        state.select("a");
        assert_eq!(payloads(&state), ["kept"]);
        assert_eq!(state.frames_generation(), generation);
        assert_eq!(state.poll_frames(&store), 0);
    }

    #[test]
    fn 선택이_없으면_프레임을_읽지_않는다() {
        let mut store = TrafficStore::default();
        frame(&mut store, "a", WsDirection::Sent, "orphan", 10);
        let mut state = PanelState::default();
        assert_eq!(state.poll_frames(&store), 0);
        assert!(state.frames().is_empty());
    }

    // ── live indicator + send button ────────────────────────────────────────

    #[test]
    fn 열린_소켓은_url이_같고_ready_state가_1인_것이다() {
        let mut state = PanelState::default();
        state.set_connections(vec![connection("a", "wss://a.example/ws")]);
        assert_eq!(state.live(), None);
        assert_eq!(state.live_label_key(), Some("panel.wsSender.noLive"));

        // 닫히는 중(2)이거나 다른 URL 이면 열린 소켓이 아니다.
        state.set_live_sockets(vec![
            live("wss://a.example/ws", 2, "page"),
            live("wss://other.example/ws", 1, "page"),
        ]);
        assert_eq!(state.live(), None);
        assert_eq!(state.send_label_key(), "panel.wsSender.reconnectSend");

        state.set_live_sockets(vec![live("wss://a.example/ws", 1, "page")]);
        assert_eq!(state.live_label_key(), Some("panel.wsSender.live"));
        assert_eq!(state.send_label_key(), "panel.wsSender.send");
    }

    #[test]
    fn iframe의_소켓은_다른_문구로_표시한다() {
        let mut state = PanelState::default();
        state.set_connections(vec![connection("a", "wss://a.example/ws")]);
        state.set_live_sockets(vec![live(
            "wss://a.example/ws",
            1,
            "https://widget.example",
        )]);
        assert_eq!(state.live_label_key(), Some("panel.wsSender.liveFrame"));
    }

    #[test]
    fn 고른_연결이_없으면_열린_소켓_표시도_없다() {
        let mut state = PanelState::default();
        state.set_live_sockets(vec![live("wss://a.example/ws", 1, "page")]);
        assert_eq!(state.live_label_key(), None);
        assert_eq!(state.live(), None);
        assert_eq!(state.send_label_key(), "panel.wsSender.reconnectSend");
    }

    #[test]
    fn 열린_소켓_목록이_같으면_바뀐_것이_없다고_알린다() {
        let mut state = PanelState::default();
        assert!(state.set_live_sockets(vec![live("wss://a.example/ws", 1, "page")]));
        assert!(!state.set_live_sockets(vec![live("wss://a.example/ws", 1, "page")]));
        assert!(state.set_live_sockets(Vec::new()));
    }

    #[test]
    fn 연결과_내용이_있고_보내는_중이_아닐_때만_보낼_수_있다() {
        let mut state = PanelState::default();
        assert!(!state.can_send("hi"));
        state.set_connections(vec![connection("a", "wss://a.example/ws")]);
        assert!(state.can_send("hi"));
        assert!(!state.can_send(""));
        // 공백만 있어도 내용이다: 원본은 trim 하지 않는다.
        assert!(state.can_send(" "));
        state.begin_send("wss://a.example/ws", "hi".into(), false);
        assert!(!state.can_send("hi"));
        assert_eq!(state.send_label_key(), "panel.wsSender.sending");
    }

    // ── sendOn ──────────────────────────────────────────────────────────────

    #[test]
    fn 열린_소켓이_있으면_재연결_없이_보낸다() {
        let mut state = PanelState::default();
        state.set_live_sockets(vec![live("wss://a.example/ws", 1, "page")]);
        let request = state.begin_send("wss://a.example/ws", "{\"op\":1}".into(), false);
        assert_eq!(
            request,
            SendRequest {
                url: "wss://a.example/ws".into(),
                payload: "{\"op\":1}".into(),
                as_binary: false,
                reconnect: false,
            }
        );
        assert!(state.sending());
    }

    #[test]
    fn 열린_소켓이_없으면_재연결을_요청한다() {
        let mut state = PanelState::default();
        state.set_live_sockets(vec![live("wss://a.example/ws", 3, "page")]);
        let request = state.begin_send("wss://a.example/ws", "AQID".into(), true);
        assert!(request.reconnect);
        assert!(request.as_binary);
    }

    #[test]
    fn 전송에_성공하면_전송됨을_표시하고_보내는_중을_끝낸다() {
        let store = TrafficStore::default();
        let mut state = PanelState::default();
        state.begin_send("wss://a.example/ws", "x".into(), false);
        let outcome = SendOutcome {
            ok: true,
            ..SendOutcome::default()
        };
        state.finish_send("wss://a.example/ws", &outcome, Lang::En, &store);
        assert_eq!(state.send_result(), Some("Sent"));
        assert!(!state.sending());
        state.finish_send("wss://a.example/ws", &outcome, Lang::Ko, &store);
        assert_eq!(state.send_result(), Some("전송됨"));
    }

    #[test]
    fn 전송에_실패하면_오류_문구를_그대로_보이고_없으면_error라고_한다() {
        let store = TrafficStore::default();
        let mut state = PanelState::default();
        let failed = SendOutcome {
            ok: false,
            error: Some("no open socket for that URL".into()),
            reconnected: false,
        };
        state.begin_send("wss://a.example/ws", "x".into(), false);
        state.finish_send("wss://a.example/ws", &failed, Lang::Ko, &store);
        assert_eq!(state.send_result(), Some("no open socket for that URL"));
        assert!(!state.sending());

        state.finish_send(
            "wss://a.example/ws",
            &SendOutcome::default(),
            Lang::En,
            &store,
        );
        assert_eq!(state.send_result(), Some("error"));

        // 빈 오류 문구는 원본에서 falsy 라 표시되지 않는다.
        let blank = SendOutcome {
            ok: false,
            error: Some(String::new()),
            reconnected: false,
        };
        state.finish_send("wss://a.example/ws", &blank, Lang::En, &store);
        assert_eq!(state.send_result(), None);
    }

    #[test]
    fn 재연결해서_보냈으면_새_연결로_옮겨_간다() {
        let mut store = TrafficStore::default();
        socket(&mut store, "old", "wss://a.example/ws", 1);
        socket(&mut store, "other", "wss://b.example/ws", 2);
        frame(&mut store, "old", WsDirection::Received, "before", 5);
        let mut state = PanelState::default();
        state.refresh(&store);
        state.select("old");
        state.poll_frames(&store);
        assert_eq!(payloads(&state), ["before"]);

        // 페이지가 같은 URL 로 다시 열었다: 새 requestId.
        socket(&mut store, "new", "wss://a.example/ws", 3);
        frame(&mut store, "new", WsDirection::Sent, "after", 9);
        let request = state.begin_send("wss://a.example/ws", "after".into(), false);
        assert!(request.reconnect);
        let outcome = SendOutcome {
            ok: true,
            error: None,
            reconnected: true,
        };
        state.finish_send("wss://a.example/ws", &outcome, Lang::En, &store);
        assert_eq!(state.send_result(), Some("Reconnected and sent"));
        // 목록은 최신순이므로 같은 URL 중 새 연결이 먼저 잡힌다.
        assert_eq!(state.selected_id(), Some("new"));
        assert_eq!(state.connections().len(), 3);
        assert!(state.frames().is_empty());
        state.poll_frames(&store);
        assert_eq!(payloads(&state), ["after"]);
    }

    #[test]
    fn 재연결했지만_목록에_그_url이_없으면_선택을_유지한다() {
        let mut store = TrafficStore::default();
        socket(&mut store, "a", "wss://a.example/ws", 1);
        let mut state = PanelState::default();
        state.refresh(&store);
        let outcome = SendOutcome {
            ok: true,
            error: None,
            reconnected: true,
        };
        state.begin_send("wss://gone.example/ws", "x".into(), false);
        state.finish_send("wss://gone.example/ws", &outcome, Lang::En, &store);
        assert_eq!(state.selected_id(), Some("a"));
    }

    // ── the panel in a window ───────────────────────────────────────────────

    const A: &str = "wss://a.example/ws";
    const B: &str = "wss://b.example/ws";

    /// The page side: answers `sockets` and `send` from what the test put in.
    #[derive(Default)]
    struct FakePage {
        sockets: Mutex<Option<Vec<LiveSocket>>>,
        sent: Mutex<Vec<SendRequest>>,
        outcome: Mutex<SendOutcome>,
        /// What the page does to the capture while it sends (a reconnect opens a request).
        on_send: Mutex<Option<Box<dyn Fn() + Send>>>,
    }

    impl FakePage {
        fn with_open(urls: &[&str]) -> Arc<Self> {
            let page = Self::default();
            *page.sockets.lock().unwrap() =
                Some(urls.iter().map(|url| live(url, 1, "page")).collect());
            *page.outcome.lock().unwrap() = SendOutcome {
                ok: true,
                ..SendOutcome::default()
            };
            Arc::new(page)
        }

        fn sent(&self) -> Vec<SendRequest> {
            self.sent.lock().unwrap().clone()
        }
    }

    impl WsHost for FakePage {
        fn sockets(&self) -> Result<Vec<LiveSocket>, String> {
            self.sockets
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| "no page".to_owned())
        }

        fn send(&self, request: SendRequest) -> SendOutcome {
            if let Some(effect) = &*self.on_send.lock().unwrap() {
                effect();
            }
            self.sent.lock().unwrap().push(request);
            self.outcome.lock().unwrap().clone()
        }
    }

    /// Two connections; the list is newest first, so "b" is selected at the start.
    fn two_sockets() -> Arc<Mutex<TrafficStore>> {
        let mut store = TrafficStore::default();
        socket(&mut store, "a", A, 1);
        socket(&mut store, "b", B, 2);
        frame(&mut store, "a", WsDirection::Received, "from-a", 10);
        frame(&mut store, "b", WsDirection::Sent, "from-b", 20);
        Arc::new(Mutex::new(store))
    }

    fn open_panel<'a>(
        cx: &'a mut TestAppContext,
        store: &Arc<Mutex<TrafficStore>>,
        page: &Arc<FakePage>,
    ) -> (Entity<WebSocketPanel>, &'a mut VisualTestContext) {
        cx.update(rever_ui_kit::text_input::bind_keys);
        let (store, page) = (store.clone(), page.clone());
        cx.add_window_view(|_, cx| WebSocketPanel::new(store, page, Lang::En, ThemeMode::Light, cx))
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"));
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
    }

    fn focus_composer(panel: &Entity<WebSocketPanel>, cx: &mut VisualTestContext) {
        panel.update_in(cx, |panel, window, cx| {
            panel.composer.read(cx).focus(window)
        });
        cx.run_until_parked();
    }

    fn composer_text(panel: &Entity<WebSocketPanel>, cx: &mut VisualTestContext) -> String {
        panel.read_with(cx, |panel, cx| panel.composer.read(cx).text().to_owned())
    }

    fn shown(panel: &Entity<WebSocketPanel>, cx: &mut VisualTestContext) -> Vec<String> {
        panel.read_with(cx, |panel, _| {
            payloads(&panel.state)
                .into_iter()
                .map(str::to_owned)
                .collect()
        })
    }

    #[gpui::test]
    fn 열자마자_첫_연결과_그_프레임과_열린_소켓을_보여준다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        assert_eq!(shown(&panel, cx), ["from-b"]);
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.state.selected_id(), Some("b"));
            assert_eq!(panel.state.live_label_key(), Some("panel.wsSender.live"));
            assert_eq!(panel.frame_list.item_count(), 1);
        });
        // 원본의 data-testid 와 같은 이름으로 찾을 수 있다.
        assert!(cx.debug_bounds("ws-live").is_some());
        assert!(cx.debug_bounds("ws-send").is_some());
        // 연결 목록은 폭 180px.
        let row = cx
            .debug_bounds("ws-connection-0")
            .expect("the first connection row");
        assert_eq!(row.size.width, px(179.0));
    }

    #[gpui::test]
    fn 연결이_없으면_열린_소켓_표시가_없다(cx: &mut TestAppContext) {
        let store = Arc::new(Mutex::new(TrafficStore::default()));
        let page = FakePage::with_open(&[A]);
        let (panel, cx) = open_panel(cx, &store, &page);
        assert!(cx.debug_bounds("ws-live").is_none());
        assert!(cx.debug_bounds("ws-connection-0").is_none());
        // 보낼 곳이 없으면 Enter 도 버튼도 아무것도 보내지 않는다.
        focus_composer(&panel, cx);
        cx.simulate_input("ping");
        cx.simulate_keystrokes("enter");
        click(cx, "ws-send");
        assert!(page.sent().is_empty());
        assert_eq!(composer_text(&panel, cx), "ping");
    }

    #[gpui::test]
    fn 연결_행을_누르면_그_연결의_프레임으로_바뀐다(cx: &mut TestAppContext) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        click(cx, "ws-connection-1");
        assert_eq!(shown(&panel, cx), ["from-a"]);
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.state.selected_url(), Some(A));
            // a 는 페이지에 열린 소켓이 없다.
            assert_eq!(panel.state.live_label_key(), Some("panel.wsSender.noLive"));
            assert_eq!(panel.state.send_label_key(), "panel.wsSender.reconnectSend");
            assert_eq!(panel.frame_list.item_count(), 1);
        });
    }

    #[gpui::test]
    fn enter는_고른_연결의_소켓으로_작성란의_내용을_보낸다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        focus_composer(&panel, cx);
        cx.simulate_input("ping");
        cx.simulate_keystrokes("enter");
        assert_eq!(
            page.sent(),
            [SendRequest {
                url: B.into(),
                payload: "ping".into(),
                as_binary: false,
                reconnect: false,
            }]
        );
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.state.send_result(), Some("Sent"));
            assert!(!panel.state.sending());
        });
        // 원본은 보낸 뒤에도 작성란을 비우지 않는다.
        assert_eq!(composer_text(&panel, cx), "ping");
    }

    #[gpui::test]
    fn shift_enter는_보내지_않고_줄을_바꾼다(cx: &mut TestAppContext) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        focus_composer(&panel, cx);
        cx.simulate_input("a");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("b");
        assert!(page.sent().is_empty());
        assert_eq!(composer_text(&panel, cx), "a\nb");
        cx.simulate_keystrokes("enter");
        assert_eq!(page.sent().len(), 1);
        assert_eq!(page.sent()[0].payload, "a\nb");
    }

    #[gpui::test]
    fn 빈_작성란에서는_enter도_버튼도_보내지_않는다(cx: &mut TestAppContext) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        focus_composer(&panel, cx);
        cx.simulate_keystrokes("enter");
        click(cx, "ws-send");
        assert!(page.sent().is_empty());
        assert_eq!(composer_text(&panel, cx), "");
    }

    #[gpui::test]
    fn 전송_버튼을_누르면_보내고_열린_소켓이_없으면_재연결을_청한다(
        cx: &mut TestAppContext,
    ) {
        // 페이지에는 a 만 열려 있고, 고른 연결은 b 다.
        let (store, page) = (two_sockets(), FakePage::with_open(&[A]));
        let (panel, cx) = open_panel(cx, &store, &page);
        panel.update(cx, |panel, cx| panel.set_payload("{\"op\":1}", cx));
        cx.run_until_parked();
        click(cx, "ws-send");
        assert_eq!(
            page.sent(),
            [SendRequest {
                url: B.into(),
                payload: "{\"op\":1}".into(),
                as_binary: false,
                reconnect: true,
            }]
        );
    }

    #[gpui::test]
    fn 보내는_동안에는_버튼이_전송_중이고_다시_보낼_수_없다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        panel.update(cx, |panel, cx| {
            panel.set_payload("ping", cx);
            panel.send(cx);
            // 답이 오기 전.
            assert!(panel.state.sending());
            assert_eq!(panel.state.send_label_key(), "panel.wsSender.sending");
            panel.send(cx);
        });
        cx.run_until_parked();
        assert_eq!(page.sent().len(), 1);
        panel.read_with(cx, |panel, _| {
            assert!(!panel.state.sending());
            assert_eq!(panel.state.send_label_key(), "panel.wsSender.send");
        });
    }

    #[gpui::test]
    fn 전송이_실패하면_도구줄에_오류를_보인다(cx: &mut TestAppContext) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        *page.outcome.lock().unwrap() = SendOutcome {
            ok: false,
            error: Some("socket closed".into()),
            reconnected: false,
        };
        let (panel, cx) = open_panel(cx, &store, &page);
        panel.update(cx, |panel, cx| {
            panel.set_payload("ping", cx);
            panel.send(cx);
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.state.send_result(), Some("socket closed"));
            assert!(!panel.state.sending());
        });
    }

    #[gpui::test]
    fn 재전송_버튼은_그_프레임을_바이너리_여부와_함께_다시_보낸다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        store.lock().unwrap().append_ws_frame(
            "b",
            WsFrame {
                direction: WsDirection::Received,
                opcode: 2,
                payload_data: "AQID".into(),
                timestamp: 30,
                mask: None,
            },
        );
        let (panel, cx) = open_panel(cx, &store, &page);
        assert_eq!(shown(&panel, cx), ["from-b", "AQID"]);
        click(cx, "ws-resend-1");
        click(cx, "ws-resend-0");
        let sent = page.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(
            (sent[0].payload.as_str(), sent[0].as_binary),
            ("AQID", true)
        );
        assert_eq!(
            (sent[1].payload.as_str(), sent[1].as_binary),
            ("from-b", false)
        );
        assert!(
            sent.iter()
                .all(|request| request.url == B && !request.reconnect)
        );
        // 재전송은 작성란을 건드리지 않는다.
        assert_eq!(composer_text(&panel, cx), "");
    }

    #[gpui::test]
    fn 편집_버튼은_잘리지_않은_페이로드를_작성란에_올리고_포커스를_준다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let long = format!("SEND\ndestination:/app\n\n{}", "x".repeat(2000));
        frame(
            &mut store.lock().unwrap(),
            "b",
            WsDirection::Received,
            &long,
            30,
        );
        let (panel, cx) = open_panel(cx, &store, &page);
        click(cx, "ws-edit-1");
        // 행에는 1024자까지만 보이지만 작성란에는 전부 올라간다.
        assert_eq!(composer_text(&panel, cx), long);
        assert!(page.sent().is_empty());
        // 포커스가 작성란에 있으므로 바로 고쳐서 보낼 수 있다.
        cx.simulate_input("!");
        cx.simulate_keystrokes("enter");
        assert_eq!(page.sent().len(), 1);
        assert_eq!(page.sent()[0].payload, format!("{long}!"));
    }

    #[gpui::test]
    fn crlf_프레임은_편집에_올린_채로_보내면_그대로_고치면_lf로_나간다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let stomp = "SEND\r\ndestination:/app\r\n\r\nbody";
        frame(
            &mut store.lock().unwrap(),
            "b",
            WsDirection::Received,
            stomp,
            30,
        );
        let (panel, cx) = open_panel(cx, &store, &page);
        click(cx, "ws-edit-1");
        // setPayload(f.payloadData) 뒤 바로 Send: 상태는 받은 문자열 그대로다.
        click(cx, "ws-send");
        assert_eq!(page.sent()[0].payload, stomp);
        // 한 글자라도 치면 textarea 의 값(LF)이 상태가 된다.
        cx.simulate_input("!");
        cx.simulate_keystrokes("enter");
        assert_eq!(page.sent()[1].payload, "SEND\ndestination:/app\n\nbody!");
        assert_eq!(composer_text(&panel, cx), "SEND\ndestination:/app\n\nbody!");
    }

    #[gpui::test]
    fn cmd_enter와_ctrl_enter도_보내고_조합_중의_enter는_보내지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        focus_composer(&panel, cx);
        cx.simulate_input("ping");
        cx.simulate_keystrokes("cmd-enter");
        cx.simulate_keystrokes("ctrl-enter");
        assert_eq!(page.sent().len(), 2);
        assert_eq!(composer_text(&panel, cx), "ping");

        // isComposing 인 Enter 는 IME 의 것이다.
        let composer = panel.read_with(cx, |panel, _| panel.composer.clone());
        composer.update_in(cx, |composer, window, cx| {
            use gpui::EntityInputHandler;
            composer.replace_and_mark_text_in_range(None, "한", Some(1..1), window, cx)
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        assert_eq!(page.sent().len(), 2);
    }

    #[gpui::test]
    fn 일_초마다_새_프레임을_읽고_목록을_맨_아래로_내린다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        for index in 0..150 {
            let text = format!("tick-{index}");
            frame(
                &mut store.lock().unwrap(),
                "b",
                WsDirection::Received,
                &text,
                100 + index,
            );
        }
        // 아직 1초가 지나지 않았다.
        assert_eq!(shown(&panel, cx).len(), 1);
        cx.executor().advance_clock(POLL);
        cx.run_until_parked();
        // 한 번의 폴링은 최근 100개만 가져온다.
        let frames = shown(&panel, cx);
        assert_eq!(frames.len(), 101);
        assert_eq!(frames.last().map(String::as_str), Some("tick-149"));
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.frame_list.item_count(), 101);
            // 창보다 긴 목록에서 마지막 행이 그려져 있다 = 맨 아래로 내려갔다.
            assert!(panel.frame_list.bounds_for_item(100).is_some());
            assert!(panel.frame_list.bounds_for_item(0).is_none());
        });
    }

    #[gpui::test]
    fn 열린_소켓은_2초마다_다시_확인하고_오류면_이전_목록을_유지한다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[]));
        let (panel, cx) = open_panel(cx, &store, &page);
        let label = |cx: &mut VisualTestContext| {
            panel.read_with(cx, |panel, _| panel.state.live_label_key())
        };
        assert_eq!(label(cx), Some("panel.wsSender.noLive"));

        *page.sockets.lock().unwrap() = Some(vec![live(B, 1, "https://widget.example")]);
        cx.executor().advance_clock(POLL);
        cx.run_until_parked();
        assert_eq!(label(cx), Some("panel.wsSender.noLive"));
        cx.executor().advance_clock(POLL);
        cx.run_until_parked();
        assert_eq!(label(cx), Some("panel.wsSender.liveFrame"));

        // ws:sockets 가 { error } 를 주면 원본은 상태를 바꾸지 않는다.
        *page.sockets.lock().unwrap() = None;
        cx.executor().advance_clock(SOCKETS_POLL);
        cx.run_until_parked();
        assert_eq!(label(cx), Some("panel.wsSender.liveFrame"));
    }

    #[gpui::test]
    fn 재연결해서_보내면_새_연결을_고르고_그_프레임을_보여준다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[]));
        *page.outcome.lock().unwrap() = SendOutcome {
            ok: true,
            error: None,
            reconnected: true,
        };
        let capture = store.clone();
        *page.on_send.lock().unwrap() = Some(Box::new(move || {
            let mut store = capture.lock().unwrap();
            socket(&mut store, "b2", B, 3);
            frame(&mut store, "b2", WsDirection::Sent, "ping", 40);
        }));
        let (panel, cx) = open_panel(cx, &store, &page);
        panel.update(cx, |panel, cx| {
            panel.set_payload("ping", cx);
            panel.send(cx);
        });
        cx.run_until_parked();
        assert!(page.sent()[0].reconnect);
        assert_eq!(shown(&panel, cx), ["ping"]);
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.state.selected_id(), Some("b2"));
            assert_eq!(panel.state.send_result(), Some("Reconnected and sent"));
            assert_eq!(panel.state.connections().len(), 3);
            assert_eq!(panel.frame_list.item_count(), 1);
        });
    }

    #[gpui::test]
    fn 수집이_비워지면_연결_목록이_비고_새로고침은_즉시_반영된다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        store.lock().unwrap().clear();
        // 호스트가 부르는 refresh 는 1초를 기다리지 않는다.
        panel.update(cx, |panel, cx| panel.refresh(cx));
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(panel.state.connections().is_empty());
            assert_eq!(panel.state.selected_id(), None);
            assert_eq!(panel.state.live_label_key(), None);
            // 선택이 사라져도 원본처럼 마지막 프레임은 목록에 남는다.
            assert_eq!(panel.frame_list.item_count(), 1);
        });
    }

    // ── IPC shapes ──────────────────────────────────────────────────────────

    #[test]
    fn ipc_값은_camel_case_필드로_오간다() {
        let socket: LiveSocket =
            serde_json::from_str(r#"{"url":"wss://a.example/ws","readyState":1,"frame":"page"}"#)
                .unwrap();
        assert_eq!(socket, live("wss://a.example/ws", 1, "page"));

        let request = SendRequest {
            url: "wss://a.example/ws".into(),
            payload: "x".into(),
            as_binary: true,
            reconnect: false,
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({
                "url": "wss://a.example/ws", "payload": "x", "asBinary": true, "reconnect": false
            })
        );
        assert_eq!(
            serde_json::to_value(connection("7", "wss://a.example/ws")).unwrap(),
            serde_json::json!({ "requestId": "7", "url": "wss://a.example/ws", "startedAt": 0 })
        );
    }

    #[test]
    fn 전송_결과는_빠진_필드와_모르는_필드를_견딘다() {
        // main 은 message 도 보내고, reconnected 와 error 는 있을 때만 보낸다.
        let outcome: SendOutcome =
            serde_json::from_str(r#"{"ok":true,"message":"sent 5 bytes"}"#).unwrap();
        assert_eq!(
            outcome,
            SendOutcome {
                ok: true,
                error: None,
                reconnected: false
            }
        );
        let failed: SendOutcome = serde_json::from_str(r#"{"ok":false,"error":"closed"}"#).unwrap();
        assert_eq!(failed.error.as_deref(), Some("closed"));
        assert!(serde_json::from_str::<SendOutcome>("not json").is_err());
    }

    // ── local tokens ────────────────────────────────────────────────────────

    #[test]
    fn 테마에_없는_토큰은_밝은_테마와_어두운_테마를_따라간다() {
        let dark = ExtraColors::of(&Theme::dark());
        assert!(dark.row_active == rgb(0x1d2a3a));
        assert!(dark.status_info == rgb(0x74b9ff));
        assert!(dark.status_ok == rgb(0x6ee7b7));
        let light = ExtraColors::of(&Theme::light());
        assert!(light.row_active == rgb(0xe3edfa));
        assert!(light.status_info == rgb(0x1a5fb4));
        assert!(light.status_ok == rgb(0x1f7a34));
    }

    #[test]
    fn 밝게_한_색은_채널이_1을_넘지_않는다() {
        let color = brighter(rgb(0xff8000));
        assert_eq!(color.r, 1.0);
        assert!((color.g - (128.0 / 255.0) * 1.1).abs() < 1e-6);
        assert_eq!(color.b, 0.0);
        assert_eq!(color.a, 1.0);
    }
}
