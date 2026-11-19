//! Port of `src/renderer/src/components/repeater/WsRepeaterPanel.tsx` — the WebSocket
//! counterpart of the HTTP repeater: pick a frame the page sent, edit it, send it on the
//! page's own socket and watch what comes back. A sequence replays several frames in
//! order (reconnect → auth → subscribe).
//!
//! The React component reads its data over IPC. Here, as in the WebSocket panel, the
//! connections and frames come straight from the traffic store, and the two calls that
//! need the page (`ws:sockets`, `ws:send`) are the host's `WsHost`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    ClickEvent, Context, Div, Entity, MouseButton, MouseDownEvent, SharedString, Subscription,
    Window, deferred, div, prelude::*, px, relative,
};
use rever_core::traffic::{TrafficStore, WsDirection, WsFrame};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::Submitted;
use rever_ui_kit::theme::ThemeMode;
use rever_ui_menus::ui::fill;
use rever_ui_state::format_json::try_pretty;
use rever_ui_state::ws_sequence::{WsSendResult, WsSequenceResult, WsStep, run_sequence};
use rever_ui_traffic::json_view::js_number;
use rever_ui_websocket::web_socket_panel::{
    WsConnection, conn_label, display_payload, list_connections,
};
use rever_ui_websocket::{LiveSocket, SendOutcome, SendRequest, WsHost};

use crate::text_area::TextArea;
use crate::ui::{
    ButtonKind, LINE_HEIGHT, MONO, SANS, Tokens, button, clickable, empty, field_box, select_box,
    select_menu, select_option, tooltip,
};

pub const DEFAULT_DELAY_MS: f64 = 100.0;
/// `window.rev.ws.frames(id, since, 500)`: one poll takes the newest 500 frames of a connection.
const FRAMES_LIMIT: usize = 500;
/// `.slice(-1000)`: the panel keeps the newest 1000 frames of a socket.
const MAX_FRAMES: usize = 1000;
/// `.slice(-200)`: the newest 200 sent frames are listed.
const SENT_SHOWN: usize = 200;
/// A sent-frame row shows this many UTF-16 units of its payload.
const SENT_PREVIEW_UNITS: usize = 160;
/// Connections are re-read every second, frames twice a second, the page's open sockets
/// every two seconds.
const CONNECTIONS_POLL: Duration = Duration::from_millis(1000);
const FRAMES_POLL: Duration = Duration::from_millis(500);
const SOCKETS_POLL: Duration = Duration::from_millis(2000);
/// `WebSocket.OPEN`.
const READY_STATE_OPEN: u8 = 1;
/// The opcode of a binary frame.
const OPCODE_BINARY: u8 = 2;

/// A frame in the panel's list. React tells frames apart by object identity
/// (`f === original`); two frames can be equal field by field, so each gets a number.
#[derive(Clone, Debug, PartialEq)]
pub struct CapturedFrame {
    pub id: u64,
    pub frame: WsFrame,
}

/// The `ws:frames` handler with the panel's limit: the newest [`FRAMES_LIMIT`] frames at
/// or after `since`, oldest first.
fn frames_since(store: &TrafficStore, request_id: &str, since: u64) -> Vec<WsFrame> {
    let frames = store.ws_frames(request_id, Some(since));
    let skip = frames.len().saturating_sub(FRAMES_LIMIT);
    frames.into_iter().skip(skip).cloned().collect()
}

/// `text.slice(0, units)` in UTF-16 units. A surrogate pair across the cut is dropped
/// whole, where JavaScript would keep half of it.
fn utf16_prefix(text: &str, units: usize) -> &str {
    let mut seen = 0;
    let cut = text.char_indices().find(|(_, ch)| {
        seen += ch.len_utf16();
        seen > units
    });
    match cut {
        Some((at, _)) => text.get(..at).unwrap_or(text),
        None => text,
    }
}

/// What `white-space: nowrap` makes of a text: every run of spaces, tabs and line breaks
/// is one space, and none at the ends.
fn nowrap_text(text: &str) -> String {
    text.split([' ', '\t', '\n', '\r', '\u{C}'])
        .filter(|piece| !piece.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `text` is what the HTML standard calls a valid floating-point number, the
/// only thing an `<input type="number">` reports as its value.
fn is_valid_float(text: &str) -> bool {
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (unsigned, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((whole, fraction)) => (whole.is_empty() || digits(whole)) && digits(fraction),
        None => digits(mantissa),
    };
    mantissa_ok
        && exponent
            .is_none_or(|exponent| digits(exponent.strip_prefix(['-', '+']).unwrap_or(exponent)))
}

/// `Math.max(0, Number(e.target.value) || 0)` for the delay field. A number input's
/// value is "" for anything that is not a finite number, which counts as 0.
pub fn parse_delay(text: &str) -> f64 {
    let value = is_valid_float(text)
        .then(|| text.parse::<f64>().ok())
        .flatten()
        .filter(|value| value.is_finite())
        .unwrap_or(0.0);
    if value > 0.0 { value } else { 0.0 }
}

/// How long `setTimeout(resolve, ms)` waits. Its delay is a 32-bit integer: fractions
/// are cut off, and what does not fit fires at once instead of never.
fn delay_duration(ms: f64) -> Duration {
    if !(0.0..=2_147_483_647.0).contains(&ms) {
        return Duration::ZERO;
    }
    Duration::from_millis(ms as u64)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

fn lock(store: &Mutex<TrafficStore>) -> MutexGuard<'_, TrafficStore> {
    // A panic elsewhere while the store was locked must not take the panel down with it.
    store.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The component's state and the rules that change it, without any interface. The draft
/// lives in the editor; the methods that need it take it as an argument.
#[derive(Default)]
pub struct WsRepeaterState {
    conns: Vec<WsConnection>,
    url: Option<String>,
    frames: Vec<CapturedFrame>,
    next_frame_id: u64,
    /// The next poll asks for frames at or after this timestamp.
    since: u64,
    original: Option<CapturedFrame>,
    steps: Vec<WsStep>,
    sent_at: Option<u64>,
    busy: bool,
    status: Option<String>,
    live_sockets: Vec<LiveSocket>,
}

impl WsRepeaterState {
    /// A socket is addressed by URL: a reconnect is a new connection to the same one.
    pub fn urls(&self) -> Vec<&str> {
        let mut urls: Vec<&str> = Vec::new();
        for conn in &self.conns {
            if !urls.contains(&conn.url.as_str()) {
                urls.push(&conn.url);
            }
        }
        urls
    }

    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    pub fn frames(&self) -> &[CapturedFrame] {
        &self.frames
    }

    pub fn original(&self) -> Option<&CapturedFrame> {
        self.original.as_ref()
    }

    pub fn steps(&self) -> &[WsStep] {
        &self.steps
    }

    pub fn sent_at(&self) -> Option<u64> {
        self.sent_at
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    /// What the toolbar shows after a send. An empty text shows nothing, as an empty
    /// string is falsy in the component.
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref().filter(|text| !text.is_empty())
    }

    /// The connection poll. A list with the same ids in the same order changes nothing.
    pub fn set_connections(&mut self, list: Vec<WsConnection>) -> bool {
        let same = list.len() == self.conns.len()
            && list
                .iter()
                .zip(&self.conns)
                .all(|(new, old)| new.request_id == old.request_id);
        if same {
            return false;
        }
        self.conns = list;
        // Move to the first socket when the selected one is gone.
        let listed = self
            .url
            .as_deref()
            .is_some_and(|url| self.conns.iter().any(|conn| conn.url == url));
        if !listed {
            let first = self.conns.first().map(|conn| conn.url.clone());
            self.set_url(first);
        }
        true
    }

    fn set_url(&mut self, url: Option<String>) {
        if self.url == url {
            return;
        }
        self.url = url;
        // The component's frame effect returns early without a URL, before it empties
        // the list.
        if self.url.is_some() {
            self.frames.clear();
            self.since = 0;
        }
    }

    /// The socket `<select>` changed: what was loaded, sent and reported belongs to the
    /// socket before.
    pub fn pick(&mut self, url: &str) {
        self.set_url(Some(url.to_owned()));
        self.original = None;
        self.sent_at = None;
        self.status = None;
    }

    /// The frame poll: frames of every connection to this URL, oldest first. Appends
    /// what arrived since the last one and returns how many.
    pub fn poll_frames(&mut self, store: &TrafficStore) -> usize {
        let Some(url) = &self.url else {
            return 0;
        };
        let mut next: Vec<WsFrame> = self
            .conns
            .iter()
            .filter(|conn| &conn.url == url)
            .flat_map(|conn| frames_since(store, &conn.request_id, self.since))
            .collect();
        // Stable, as `Array.prototype.sort` is: frames of one millisecond keep the
        // order of their connections.
        next.sort_by_key(|frame| frame.timestamp);
        let Some(last) = next.last() else {
            return 0;
        };
        // A timestamp at the top of the range must not overflow the cursor.
        self.since = last.timestamp.saturating_add(1);
        let count = next.len();
        for frame in next {
            self.frames.push(CapturedFrame {
                id: self.next_frame_id,
                frame,
            });
            self.next_frame_id += 1;
        }
        let excess = self.frames.len().saturating_sub(MAX_FRAMES);
        self.frames.drain(..excess);
        count
    }

    /// The live-socket poll. Returns whether the list changed.
    pub fn set_live_sockets(&mut self, sockets: Vec<LiveSocket>) -> bool {
        if self.live_sockets == sockets {
            return false;
        }
        self.live_sockets = sockets;
        true
    }

    /// The open page socket of the selected URL, if the page still holds one.
    pub fn live(&self) -> Option<&LiveSocket> {
        let url = self.url.as_deref()?;
        self.live_sockets
            .iter()
            .find(|socket| socket.url == url && socket.ready_state == READY_STATE_OPEN)
    }

    /// The live indicator's string key.
    pub fn live_label_key(&self) -> &'static str {
        match self.live() {
            Some(socket) if socket.frame == "page" => "panel.wsSender.live",
            Some(_) => "panel.wsSender.liveFrame",
            None => "panel.wsSender.noLive",
        }
    }

    /// The send button's string key.
    pub fn send_label_key(&self) -> &'static str {
        if self.busy {
            "panel.wsSender.sending"
        } else if self.live().is_some() {
            "panel.wsSender.send"
        } else {
            "panel.wsSender.reconnectSend"
        }
    }

    /// The loaded frame was binary: its payload is base64 and goes out as bytes.
    pub fn as_binary(&self) -> bool {
        self.original
            .as_ref()
            .is_some_and(|original| original.frame.opcode == OPCODE_BINARY)
    }

    /// The frames the page sent, newest first, at most [`SENT_SHOWN`].
    pub fn sent_frames(&self) -> Vec<&CapturedFrame> {
        let sent: Vec<&CapturedFrame> = self
            .frames
            .iter()
            .filter(|captured| captured.frame.direction == WsDirection::Sent)
            .collect();
        let skip = sent.len().saturating_sub(SENT_SHOWN);
        sent.into_iter().skip(skip).rev().collect()
    }

    /// Everything received since the send, with the milliseconds since it — a socket has
    /// no request/response ids to pair by.
    pub fn replies(&self) -> Vec<(u64, &WsFrame)> {
        let Some(sent_at) = self.sent_at else {
            return Vec::new();
        };
        self.frames
            .iter()
            .map(|captured| &captured.frame)
            .filter(|frame| frame.direction == WsDirection::Received && frame.timestamp >= sent_at)
            .map(|frame| (frame.timestamp - sent_at, frame))
            .collect()
    }

    /// A click on a sent frame: it becomes the original, and its payload (returned) the
    /// draft.
    pub fn load(&mut self, id: u64) -> Option<&str> {
        let captured = self.frames.iter().find(|captured| captured.id == id)?;
        self.original = Some(captured.clone());
        self.original
            .as_ref()
            .map(|original| original.frame.payload_data.as_str())
    }

    /// Whether the editor may send `draft` now (the button's `disabled`, negated).
    pub fn can_send(&self, draft: &str) -> bool {
        self.url.is_some() && !draft.is_empty() && !self.busy
    }

    /// Start sending `draft`. Without an open socket the page reopens the same URL first.
    pub fn begin_send(&mut self, draft: &str, now_ms: u64) -> Option<SendRequest> {
        if !self.can_send(draft) {
            return None;
        }
        let request = SendRequest {
            url: self.url.clone()?,
            payload: draft.to_owned(),
            as_binary: self.as_binary(),
            reconnect: self.live().is_none(),
        };
        self.busy = true;
        self.sent_at = Some(now_ms);
        Some(request)
    }

    /// The send came back.
    pub fn finish_send(&mut self, outcome: &SendOutcome, lang: Lang) {
        self.status = Some(if outcome.ok {
            lang.t(if outcome.reconnected {
                "panel.wsSender.reconnectSent"
            } else {
                "panel.wsSender.sent"
            })
            .to_owned()
        } else {
            outcome.error.clone().unwrap_or_else(|| "error".to_owned())
        });
        self.busy = false;
    }

    /// "Add to sequence": the draft as a step, sent the way the draft would be.
    pub fn add_step(&mut self, draft: &str) {
        // The button is disabled on an empty draft.
        if draft.is_empty() {
            return;
        }
        self.steps.push(WsStep {
            payload: draft.to_owned(),
            delay_ms: DEFAULT_DELAY_MS,
            as_binary: Some(self.as_binary()),
        });
    }

    /// The delay field of a step changed to `text`.
    pub fn set_step_delay(&mut self, index: usize, text: &str) {
        if let Some(step) = self.steps.get_mut(index) {
            step.delay_ms = parse_delay(text);
        }
    }

    pub fn remove_step(&mut self, index: usize) {
        if index < self.steps.len() {
            self.steps.remove(index);
        }
    }

    pub fn can_play(&self) -> bool {
        self.url.is_some() && !self.steps.is_empty() && !self.busy
    }

    /// Start the sequence: the URL to send on and the steps as they are now.
    pub fn begin_play(&mut self, now_ms: u64) -> Option<(String, Vec<WsStep>)> {
        if !self.can_play() {
            return None;
        }
        let url = self.url.clone()?;
        self.busy = true;
        self.sent_at = Some(now_ms);
        Some((url, self.steps.clone()))
    }

    /// The sequence ended.
    pub fn finish_play(&mut self, result: &WsSequenceResult, lang: Lang) {
        // `r.error ? …`: an empty error text counts as none.
        let failed = result.error.as_deref().filter(|error| !error.is_empty());
        self.status = Some(match failed {
            Some(error) => fill(
                lang.t("wsRepeater.seqFailed"),
                &[("n", &(result.sent + 1).to_string()), ("error", error)],
            ),
            None => fill(
                lang.t("wsRepeater.seqDone"),
                &[("n", &result.sent.to_string())],
            ),
        });
        self.busy = false;
    }
}

/// The delay field of one step. The subscription reports edits and ends with the field.
struct DelayField {
    input: Entity<TextArea>,
    _subscription: Subscription,
}

pub struct WsRepeaterPanel {
    store: Arc<Mutex<TrafficStore>>,
    host: Arc<dyn WsHost>,
    lang: Lang,
    theme_mode: ThemeMode,
    state: WsRepeaterState,
    draft: Entity<TextArea>,
    /// One per step, in the steps' order.
    delay_fields: Vec<DelayField>,
    socket_open: bool,
    _subscriptions: [Subscription; 2],
}

impl WsRepeaterPanel {
    pub fn new(
        store: Arc<Mutex<TrafficStore>>,
        host: Arc<dyn WsHost>,
        lang: Lang,
        theme_mode: ThemeMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let draft = cx.new(|cx| TextArea::new(lang.t("panel.wsSender.payload"), cx));
        let subscriptions = [
            // Cmd+Enter and Ctrl+Enter send; plain Enter is a line break.
            cx.subscribe(&draft, |this, _, _: &Submitted, cx| this.send(cx)),
            // The buttons under the editor are enabled by its text.
            cx.observe(&draft, |_, _, cx| cx.notify()),
        ];

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CONNECTIONS_POLL).await;
                if this
                    .update(cx, |this, cx| this.poll_connections(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FRAMES_POLL).await;
                if this.update(cx, |this, cx| this.poll_frames(cx)).is_err() {
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
                    // An error keeps the previous list.
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
            state: WsRepeaterState::default(),
            draft,
            delay_fields: Vec::new(),
            socket_open: false,
            _subscriptions: subscriptions,
        };
        panel.poll_connections(cx);
        panel
    }

    /// Re-read the connections from the traffic store. The panel does this every second
    /// on its own; the host may call it as soon as it records traffic.
    pub fn poll_connections(&mut self, cx: &mut Context<Self>) {
        let store = lock(&self.store);
        if self.state.set_connections(list_connections(&store)) {
            // A newly selected socket shows its frames at once.
            self.state.poll_frames(&store);
            cx.notify();
        }
    }

    fn poll_frames(&mut self, cx: &mut Context<Self>) {
        if self.state.poll_frames(&lock(&self.store)) > 0 {
            cx.notify();
        }
    }

    fn pick(&mut self, url: &str, cx: &mut Context<Self>) {
        self.socket_open = false;
        // A `<select>` fires no change event for the option it already shows: the
        // loaded frame, the replies and the status stay.
        if self.state.url() != Some(url) {
            self.state.pick(url);
            self.state.poll_frames(&lock(&self.store));
        }
        cx.notify();
    }

    /// A click on the row at `index` of the sent-frame list (newest first).
    pub fn load_sent_frame(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(id) = self
            .state
            .sent_frames()
            .get(index)
            .map(|captured| captured.id)
        {
            self.load(id, cx);
        }
    }

    /// Replace the editor's text.
    pub fn set_draft(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.draft.update(cx, |draft, cx| draft.set_text(text, cx));
    }

    fn load(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(payload) = self.state.load(id).map(str::to_owned) {
            self.draft
                .update(cx, |draft, cx| draft.set_text(payload, cx));
        }
        cx.notify();
    }

    /// The Send button. Does nothing on an empty draft or while a send is out.
    pub fn send(&mut self, cx: &mut Context<Self>) {
        let request = self.state.begin_send(self.draft.read(cx).text(), now_ms());
        let Some(request) = request else {
            return;
        };
        let host = self.host.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { host.send(request) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.state.finish_send(&outcome, this.lang);
                cx.notify();
            });
        })
        .detach();
    }

    /// The Play button: sends the sequence's steps in order.
    pub fn play(&mut self, cx: &mut Context<Self>) {
        let Some((url, steps)) = self.state.begin_play(now_ms()) else {
            return;
        };
        let host = self.host.clone();
        let executor = cx.background_executor().clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = run_sequence(
                &steps,
                async |step: &WsStep, reconnect: bool| {
                    let request = SendRequest {
                        url: url.clone(),
                        payload: step.payload.clone(),
                        // `asBinary === true` in the `ws:send` handler.
                        as_binary: step.as_binary == Some(true),
                        reconnect,
                    };
                    let host = host.clone();
                    let outcome = executor.spawn(async move { host.send(request) }).await;
                    WsSendResult {
                        ok: outcome.ok,
                        error: outcome.error,
                    }
                },
                async |ms: f64| executor.timer(delay_duration(ms)).await,
            )
            .await;
            let _ = this.update(cx, |this, cx| {
                this.state.finish_play(&result, this.lang);
                cx.notify();
            });
        })
        .detach();
    }

    fn format(&mut self, cx: &mut Context<Self>) {
        let pretty = try_pretty(self.draft.read(cx).text());
        self.draft
            .update(cx, |draft, cx| draft.set_text(pretty, cx));
    }

    /// "Add to sequence": the draft becomes the sequence's last step.
    pub fn add_step(&mut self, cx: &mut Context<Self>) {
        let before = self.state.steps().len();
        self.state.add_step(self.draft.read(cx).text());
        if self.state.steps().len() > before {
            let input = cx.new(|cx| {
                let mut input = TextArea::single_line("", cx);
                input.set_text(js_number(DEFAULT_DELAY_MS), cx);
                input
            });
            let subscription = cx.observe(&input, |this, _, cx| this.on_delay_edited(cx));
            self.delay_fields.push(DelayField {
                input,
                _subscription: subscription,
            });
        }
        cx.notify();
    }

    fn remove_step(&mut self, index: usize, cx: &mut Context<Self>) {
        self.state.remove_step(index);
        if index < self.delay_fields.len() {
            self.delay_fields.remove(index);
        }
        cx.notify();
    }

    /// A delay field notified: every step takes the delay its field shows.
    fn on_delay_edited(&mut self, cx: &mut Context<Self>) {
        for (index, field) in self.delay_fields.iter().enumerate() {
            self.state
                .set_step_delay(index, field.input.read(cx).text());
        }
    }

    // ── rendering ───────────────────────────────────────────────────────────

    /// `HEAD`: the caption bar over a list.
    fn head(t: &Tokens) -> Div {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .py(px(4.0))
            .border_b_1()
            .border_color(t.theme.border)
            .text_size(px(10.0))
            .line_height(px(12.0))
            .text_color(t.theme.text_dim)
    }

    /// `ROW`: one frame or step.
    fn row(t: &Tokens) -> Div {
        div()
            .px(px(8.0))
            .py(px(3.0))
            .border_b_1()
            .border_color(t.theme.border)
            .font_family(MONO)
            .text_size(px(11.0))
            .line_height(px(LINE_HEIGHT))
    }

    fn socket_select(&self, t: &Tokens, cx: &Context<Self>) -> Div {
        let url = self.state.url().unwrap_or_default();
        let select = select_box("wsr-socket", conn_label(url), t)
            .px(px(4.0))
            .py(px(2.0))
            .font_family(MONO)
            .text_size(px(11.0))
            .line_height(px(LINE_HEIGHT))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.socket_open = !this.socket_open;
                cx.notify();
            }));
        let wrap = div().relative().flex_none().m(px(6.0)).child(select);
        if !self.socket_open {
            return wrap;
        }
        let menu = select_menu("wsr-socket-menu", t)
            .top(relative(1.0))
            .mt(px(2.0))
            .font_family(MONO)
            .text_size(px(11.0))
            .line_height(px(LINE_HEIGHT))
            // A press outside closes the list and goes no further, as a native popup does.
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Left {
                    this.socket_open = false;
                    cx.notify();
                }
                cx.stop_propagation();
            }))
            .children(
                self.state
                    .urls()
                    .into_iter()
                    .enumerate()
                    .map(|(index, option)| {
                        let picked = option.to_owned();
                        select_option(
                            ("wsr-socket-option", index),
                            conn_label(option),
                            option == url,
                            t,
                        )
                        .on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| this.pick(&picked, cx)),
                        )
                    }),
            );
        wrap.child(deferred(menu).with_priority(1))
    }

    fn sequence(&self, t: &Tokens, cx: &Context<Self>) -> Div {
        let lang = self.lang;
        let busy = self.state.busy();
        div()
            .debug_selector(|| "wsr-sequence".into())
            .max_h(relative(0.45))
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(t.theme.border)
            .child(
                Self::head(t)
                    .child(div().flex_1().child(lang.t("wsRepeater.sequence")))
                    .child(
                        button(
                            "wsr-play",
                            format!("▶ {}", lang.t("wsRepeater.play")),
                            ButtonKind::Primary,
                            !busy,
                            t,
                        )
                        .when(!busy, |button| {
                            button
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.play(cx)))
                        }),
                    ),
            )
            .child(
                clickable("wsr-steps")
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(
                        self.state
                            .steps()
                            .iter()
                            .zip(&self.delay_fields)
                            .enumerate()
                            .map(|(index, (step, field))| {
                                // A `title` is cut short by the browser; so is this one.
                                let title = display_payload(&step.payload).into_owned();
                                Self::row(t)
                                    .flex()
                                    .items_center()
                                    .gap(px(4.0))
                                    .child(
                                        // 52px of content plus the input's own padding and border.
                                        field_box(("wsr-step-delay", index), t)
                                            .w(px(58.0))
                                            .flex_none()
                                            .px(px(2.0))
                                            .py(px(1.0))
                                            .tooltip(tooltip(lang.t("wsRepeater.delay"), t))
                                            .child(field.input.clone()),
                                    )
                                    .child(
                                        clickable(("wsr-step-payload", index))
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .tooltip(tooltip(title, t))
                                            .child(nowrap_text(&step.payload)),
                                    )
                                    .child(
                                        button(
                                            ("wsr-step-remove", index),
                                            "✕",
                                            ButtonKind::DangerIcon,
                                            true,
                                            t,
                                        )
                                        .tooltip(tooltip(lang.t("wsRepeater.removeStep"), t))
                                        .on_click(
                                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                                this.remove_step(index, cx)
                                            }),
                                        ),
                                    )
                            }),
                    ),
            )
    }

    /// Left: socket, the frames the page sent on it, the sequence.
    fn left_column(&self, t: &Tokens, cx: &Context<Self>) -> Div {
        let lang = self.lang;
        let sent = self.state.sent_frames();
        let original = self.state.original().map(|original| original.id);
        div()
            // 230px of content plus the border.
            .w(px(231.0))
            .flex_none()
            .min_h_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(t.theme.border)
            .child(self.socket_select(t, cx))
            .child(Self::head(t).child(lang.t("wsRepeater.sentFrames")))
            .child(
                clickable("wsr-sent")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(sent.is_empty(), |list| {
                        list.child(empty(lang.t("wsRepeater.noSent"), t))
                    })
                    .children(sent.iter().enumerate().map(|(index, captured)| {
                        let id = captured.id;
                        let preview =
                            utf16_prefix(&captured.frame.payload_data, SENT_PREVIEW_UNITS);
                        Self::row(t)
                            .id(("wsr-sent-frame", index))
                            .debug_selector(move || format!("wsr-sent-frame-{index}"))
                            // 42px of content plus the row's padding and border.
                            .max_h(px(49.0))
                            .overflow_hidden()
                            .cursor_pointer()
                            .when(original == Some(id), |row| row.bg(t.row_active))
                            .on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| this.load(id, cx)),
                            )
                            .child(preview.to_owned())
                    })),
            )
            .when(!self.state.steps().is_empty(), |column| {
                column.child(self.sequence(t, cx))
            })
    }

    /// `.panel-toolbar`: which page socket the editor sends through.
    fn toolbar(&self, t: &Tokens) -> Div {
        let lang = self.lang;
        let url: SharedString = self.state.url().unwrap_or_default().to_owned().into();
        let live_color = if self.state.live().is_some() {
            t.status_ok
        } else {
            t.theme.text_dim
        };
        let status: Option<SharedString> = self.state.status().map(|text| text.to_owned().into());
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
            .child(
                div()
                    .flex_none()
                    .text_color(t.theme.text_dim)
                    .child(lang.t("panel.wsSender.target")),
            )
            .child(
                clickable("wsr-target")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO)
                    .tooltip(tooltip(url.clone(), t))
                    .child(url),
            )
            .child(
                div()
                    .debug_selector(|| "wsr-live".into())
                    .flex_none()
                    .text_color(live_color)
                    .child(lang.t(self.state.live_label_key())),
            )
            .when_some(status, |bar, status| {
                bar.child(
                    clickable("wsr-status")
                        .max_w(relative(0.4))
                        .truncate()
                        .text_color(t.theme.text_dim)
                        .tooltip(tooltip(status.clone(), t))
                        .child(status),
                )
            })
    }

    /// Middle: original on top, the edit below.
    fn middle_column(&self, t: &Tokens, cx: &Context<Self>) -> Div {
        let lang = self.lang;
        let has_draft = !self.draft.read(cx).text().is_empty();
        let can_send = has_draft && !self.state.busy();
        let as_binary = self.state.as_binary();
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.toolbar(t))
            .when_some(self.state.original(), |column, original| {
                column
                    .child(Self::head(t).child(format!(
                        "{}{}",
                        lang.t("wsRepeater.original"),
                        if as_binary { " · base64" } else { "" }
                    )))
                    .child(
                        // shortcut: the whole payload is one text block, shaped on every
                        // paint; draw it in pieces as JsonView does if megabyte frames
                        // are loaded here.
                        Self::row(t)
                            .id("wsr-original")
                            .debug_selector(|| "wsr-original".into())
                            .flex_none()
                            .max_h(relative(0.3))
                            .overflow_y_scroll()
                            .text_color(t.theme.text_dim)
                            .child(original.frame.payload_data.clone()),
                    )
            })
            .child(
                // The `<textarea>`: at least 40px of text plus its padding.
                clickable("wsr-draft")
                    .flex_1()
                    .min_h(px(52.0))
                    .flex()
                    .flex_col()
                    .px(px(10.0))
                    .py(px(6.0))
                    .text_color(t.theme.text)
                    .font_family(MONO)
                    .text_size(px(11.0))
                    .line_height(px(LINE_HEIGHT))
                    .child(self.draft.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.0))
                    .p(px(4.0))
                    .border_t_1()
                    .border_color(t.theme.border)
                    .child(
                        button(
                            "wsr-format",
                            lang.t("wsRepeater.format"),
                            ButtonKind::Ghost,
                            has_draft,
                            t,
                        )
                        .when(has_draft, |button| {
                            button.on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.format(cx)),
                            )
                        }),
                    )
                    .child(
                        button(
                            "wsr-add-step",
                            lang.t("wsRepeater.addStep"),
                            ButtonKind::Ghost,
                            has_draft,
                            t,
                        )
                        .when(has_draft, |button| {
                            button.on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.add_step(cx)),
                            )
                        }),
                    )
                    .child(div().flex_1())
                    .child(
                        button(
                            "wsr-send",
                            lang.t(self.state.send_label_key()),
                            ButtonKind::Primary,
                            can_send,
                            t,
                        )
                        .when(can_send, |button| {
                            button
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.send(cx)))
                        }),
                    ),
            )
    }

    /// Right: everything received since the send.
    fn right_column(&self, t: &Tokens) -> Div {
        let lang = self.lang;
        let replies = self.state.replies();
        div()
            .w(relative(0.32))
            .flex_none()
            .min_h_0()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(t.theme.border)
            .child(Self::head(t).child(lang.t("wsRepeater.responses")))
            .child(
                // shortcut: every reply row is laid out on each frame (at most 1000);
                // switch to GPUI's `list` if a busy socket shows in profiles.
                clickable("wsr-replies")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(replies.is_empty(), |list| {
                        list.child(empty(lang.t("wsRepeater.noResponses"), t))
                    })
                    .children(replies.iter().enumerate().map(|(index, (elapsed, frame))| {
                        Self::row(t)
                            .debug_selector(move || format!("wsr-reply-{index}"))
                            .flex()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(t.status_ok)
                                    .child(format!("+{elapsed}ms")),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(display_payload(&frame.payload_data).into_owned()),
                            )
                    })),
            )
    }
}

impl Render for WsRepeaterPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::resolve(self.theme_mode, window);
        let root = div()
            .size_full()
            .min_h_0()
            .flex()
            .bg(t.theme.bg)
            .text_color(t.theme.text)
            .font_family(SANS)
            .text_size(px(13.0));
        if self.state.urls().is_empty() {
            return root
                .flex_col()
                .child(empty(self.lang.t("panel.noWs"), &t).debug_selector(|| "wsr-empty".into()));
        }
        root.child(self.left_column(&t, cx))
            .child(self.middle_column(&t, cx))
            .child(self.right_column(&t))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use rever_core::traffic::RequestPatch;
    use serde_json::json;

    use super::*;

    const A: &str = "wss://a.example/ws";
    const B: &str = "wss://b.example/ws";

    fn socket(store: &mut TrafficStore, id: &str, url: &str, started_at: u64) {
        store.upsert(
            RequestPatch {
                request_id: id.to_owned(),
                url: Some(url.to_owned()),
                resource_type: Some("WebSocket".to_owned()),
                started_at: Some(started_at),
                ..RequestPatch::default()
            },
            started_at,
        );
    }

    fn frame_with(
        store: &mut TrafficStore,
        id: &str,
        direction: WsDirection,
        opcode: u8,
        text: &str,
        ts: u64,
    ) {
        store.append_ws_frame(
            id,
            WsFrame {
                direction,
                opcode,
                payload_data: text.to_owned(),
                timestamp: ts,
                mask: None,
            },
        );
    }

    fn sent(store: &mut TrafficStore, id: &str, text: &str, ts: u64) {
        frame_with(store, id, WsDirection::Sent, 1, text, ts);
    }

    fn received(store: &mut TrafficStore, id: &str, text: &str, ts: u64) {
        frame_with(store, id, WsDirection::Received, 1, text, ts);
    }

    fn conn(id: &str, url: &str) -> WsConnection {
        WsConnection {
            request_id: id.to_owned(),
            url: url.to_owned(),
            started_at: 0,
        }
    }

    fn live(url: &str, ready_state: u8, frame: &str) -> LiveSocket {
        LiveSocket {
            url: url.to_owned(),
            ready_state,
            frame: frame.to_owned(),
        }
    }

    fn payloads(state: &WsRepeaterState) -> Vec<&str> {
        state
            .frames()
            .iter()
            .map(|captured| captured.frame.payload_data.as_str())
            .collect()
    }

    /// A state on socket A with one connection and the store behind it.
    fn on_a() -> (WsRepeaterState, TrafficStore) {
        let mut store = TrafficStore::default();
        socket(&mut store, "a1", A, 1);
        let mut state = WsRepeaterState::default();
        state.set_connections(list_connections(&store));
        (state, store)
    }

    fn step(payload: &str, delay_ms: f64) -> WsStep {
        WsStep {
            payload: payload.to_owned(),
            delay_ms,
            as_binary: Some(false),
        }
    }

    // ── small helpers ───────────────────────────────────────────────────────

    #[test]
    fn 보낸_프레임_미리보기는_utf16_160단위에서_자른다() {
        let text = "a".repeat(200);
        assert_eq!(utf16_prefix(&text, 160), "a".repeat(160));
        assert_eq!(utf16_prefix("short", 160), "short");
        assert_eq!(utf16_prefix("", 160), "");
        // 한글은 3바이트지만 1단위다.
        assert_eq!(utf16_prefix(&"가".repeat(200), 160), "가".repeat(160));
        // 이모지는 2단위: 자르는 자리에 걸치면 통째로 빠진다.
        assert_eq!(utf16_prefix("a😀b", 2), "a");
        assert_eq!(utf16_prefix("a😀b", 3), "a😀");
    }

    #[test]
    fn nowrap은_공백과_줄바꿈_묶음을_공백_하나로_만든다() {
        assert_eq!(nowrap_text("SEND\nid:1\n\nbody"), "SEND id:1 body");
        assert_eq!(nowrap_text("  a \t b\r\n"), "a b");
        assert_eq!(nowrap_text(""), "");
        // NBSP 는 접히는 공백이 아니다.
        assert_eq!(nowrap_text("a\u{A0}\u{A0}b"), "a\u{A0}\u{A0}b");
    }

    #[test]
    fn 대기_시간은_유효한_숫자만_받고_나머지는_0이다() {
        assert_eq!(parse_delay("100"), 100.0);
        assert_eq!(parse_delay("0"), 0.0);
        assert_eq!(parse_delay("1.5"), 1.5);
        assert_eq!(parse_delay(".5"), 0.5);
        assert_eq!(parse_delay("1e3"), 1000.0);
        assert_eq!(parse_delay("2E+2"), 200.0);
        // 숫자 입력란이 값으로 치지 않는 글은 "" 이고, Number("") || 0 은 0 이다.
        for text in [
            "", "abc", "12px", "1.", "+5", " 5", "1e", "--1", "0x10", "1,5", "५",
        ] {
            assert_eq!(parse_delay(text), 0.0, "{text:?}");
        }
        // Rust 의 parse 는 받지만 숫자 입력란은 받지 않는 글.
        for text in ["inf", "NaN", "infinity", "-inf"] {
            assert_eq!(parse_delay(text), 0.0, "{text:?}");
        }
    }

    #[test]
    fn 대기_시간은_음수가_될_수_없고_무한대는_0이다() {
        assert_eq!(parse_delay("-5"), 0.0);
        assert_eq!(parse_delay("-0.5"), 0.0);
        // Math.max(0, -0) 은 +0 이다.
        assert!(parse_delay("-0").is_sign_positive());
        assert_eq!(parse_delay("1e999"), 0.0);
    }

    #[test]
    fn 타이머_지연은_settimeout처럼_32비트를_넘으면_곧바로_끝난다() {
        assert_eq!(delay_duration(100.0), Duration::from_millis(100));
        // 소수부는 버린다.
        assert_eq!(delay_duration(250.9), Duration::from_millis(250));
        assert_eq!(delay_duration(0.5), Duration::ZERO);
        assert_eq!(delay_duration(2_147_483_647.0).as_millis(), 2_147_483_647);
        assert_eq!(delay_duration(2_147_483_648.0), Duration::ZERO);
        assert_eq!(delay_duration(1e300), Duration::ZERO);
        // 패닉 없이 견뎌야 하는 값들.
        assert_eq!(delay_duration(f64::NAN), Duration::ZERO);
        assert_eq!(delay_duration(f64::INFINITY), Duration::ZERO);
        assert_eq!(delay_duration(-3.0), Duration::ZERO);
    }

    #[test]
    fn 프레임은_연결마다_최근_500개까지만_since_이후로_읽는다() {
        let mut store = TrafficStore::default();
        socket(&mut store, "a1", A, 1);
        for index in 0..600 {
            sent(&mut store, "a1", &format!("f{index}"), 100 + index);
        }
        let frames = frames_since(&store, "a1", 0);
        assert_eq!(frames.len(), 500);
        assert_eq!(frames[0].payload_data, "f100");
        assert_eq!(frames[499].payload_data, "f599");
        assert_eq!(frames_since(&store, "a1", 698).len(), 2);
        assert!(frames_since(&store, "missing", 0).is_empty());
    }

    // ── sockets ─────────────────────────────────────────────────────────────

    #[test]
    fn 소켓_목록은_url로_묶고_첫_소켓을_자동으로_고른다() {
        let mut state = WsRepeaterState::default();
        assert!(state.urls().is_empty());
        assert_eq!(state.url(), None);
        // 최신순: b 의 재연결(b2), a, b 의 첫 연결.
        assert!(state.set_connections(vec![conn("b2", B), conn("a1", A), conn("b1", B)]));
        assert_eq!(state.urls(), [B, A]);
        assert_eq!(state.url(), Some(B));
    }

    #[test]
    fn 같은_id_순서의_목록은_아무것도_바꾸지_않는다() {
        let mut state = WsRepeaterState::default();
        state.set_connections(vec![conn("a1", A), conn("b1", B)]);
        state.pick(B);
        assert!(!state.set_connections(vec![conn("a1", A), conn("b1", B)]));
        assert_eq!(state.url(), Some(B));
        // 새 연결이 앞에 와도 고른 소켓은 유지된다.
        assert!(state.set_connections(vec![conn("a2", A), conn("a1", A), conn("b1", B)]));
        assert_eq!(state.url(), Some(B));
    }

    #[test]
    fn 고른_소켓이_사라지면_첫_소켓으로_옮기되_불러온_프레임과_상태는_남는다() {
        let (mut state, mut store) = on_a();
        sent(&mut store, "a1", "hello", 10);
        state.poll_frames(&store);
        let id = state.frames()[0].id;
        state.load(id);
        state.finish_send(
            &SendOutcome {
                ok: true,
                ..SendOutcome::default()
            },
            Lang::En,
        );

        socket(&mut store, "b1", B, 2);
        state.set_connections(vec![conn("b1", B)]);
        assert_eq!(state.url(), Some(B));
        // URL 이 바뀌면 프레임 목록은 비워진다.
        assert!(state.frames().is_empty());
        // pick 과 달리 자동 이동은 원본·상태를 지우지 않는다.
        assert!(state.original().is_some());
        assert_eq!(state.status(), Some("Sent"));
    }

    #[test]
    fn 소켓이_모두_사라지면_선택은_비고_프레임은_남는다() {
        let (mut state, mut store) = on_a();
        sent(&mut store, "a1", "hello", 10);
        state.poll_frames(&store);
        state.set_connections(Vec::new());
        assert_eq!(state.url(), None);
        assert!(state.urls().is_empty());
        assert_eq!(payloads(&state), ["hello"]);
        // 고른 소켓이 없으면 프레임을 읽지 않는다.
        assert_eq!(state.poll_frames(&store), 0);
    }

    #[test]
    fn 소켓을_고르면_불러온_원본과_전송_시각과_상태가_지워진다() {
        let mut store = TrafficStore::default();
        socket(&mut store, "a1", A, 1);
        socket(&mut store, "b1", B, 2);
        sent(&mut store, "a1", "from-a", 10);
        sent(&mut store, "b1", "from-b", 20);
        let mut state = WsRepeaterState::default();
        state.set_connections(list_connections(&store));
        state.poll_frames(&store);
        assert_eq!(payloads(&state), ["from-b"]);
        let id = state.frames()[0].id;
        state.load(id);
        state.begin_send("x", 500);
        state.finish_send(&SendOutcome::default(), Lang::En);

        state.pick(A);
        assert_eq!(state.url(), Some(A));
        assert_eq!(state.original(), None);
        assert_eq!(state.sent_at(), None);
        assert_eq!(state.status(), None);
        assert!(state.frames().is_empty());
        state.poll_frames(&store);
        assert_eq!(payloads(&state), ["from-a"]);
    }

    // ── frames ──────────────────────────────────────────────────────────────

    #[test]
    fn 같은_url의_모든_연결의_프레임을_시각순으로_합친다() {
        let mut store = TrafficStore::default();
        socket(&mut store, "a1", A, 1);
        socket(&mut store, "a2", A, 2);
        socket(&mut store, "b1", B, 3);
        sent(&mut store, "a1", "old-1", 10);
        received(&mut store, "a1", "old-2", 30);
        sent(&mut store, "a2", "new-1", 20);
        received(&mut store, "a2", "new-2", 40);
        sent(&mut store, "b1", "other", 25);
        let mut state = WsRepeaterState::default();
        state.set_connections(list_connections(&store));
        state.pick(A);
        assert_eq!(state.poll_frames(&store), 4);
        assert_eq!(payloads(&state), ["old-1", "new-1", "old-2", "new-2"]);
    }

    #[test]
    fn 프레임_폴링은_새로_온_것만_이어_붙인다() {
        let (mut state, mut store) = on_a();
        sent(&mut store, "a1", "one", 10);
        assert_eq!(state.poll_frames(&store), 1);
        assert_eq!(state.poll_frames(&store), 0);
        received(&mut store, "a1", "two", 11);
        assert_eq!(state.poll_frames(&store), 1);
        assert_eq!(payloads(&state), ["one", "two"]);
        // 마지막 프레임과 같은 밀리초에 늦게 온 프레임은 원본처럼 건너뛴다.
        received(&mut store, "a1", "late", 11);
        assert_eq!(state.poll_frames(&store), 0);
        // 번호는 프레임마다 다르다.
        assert_ne!(state.frames()[0].id, state.frames()[1].id);
    }

    #[test]
    fn 시각이_u64_끝인_프레임도_커서를_넘치게_하지_않는다() {
        let (mut state, mut store) = on_a();
        sent(&mut store, "a1", "edge", u64::MAX);
        assert_eq!(state.poll_frames(&store), 1);
        assert_eq!(state.poll_frames(&store), 1);
    }

    #[test]
    fn 프레임은_최근_1000개까지만_들고_있다() {
        let (mut state, mut store) = on_a();
        for round in 0..3u64 {
            for index in 0..400u64 {
                let n = round * 400 + index;
                sent(&mut store, "a1", &format!("f{n}"), 100 + n);
            }
            state.poll_frames(&store);
        }
        assert_eq!(state.frames().len(), MAX_FRAMES);
        assert_eq!(payloads(&state).first(), Some(&"f200"));
        assert_eq!(payloads(&state).last(), Some(&"f1199"));
    }

    #[test]
    fn 보낸_프레임_목록은_최신순으로_200개까지다() {
        let (mut state, mut store) = on_a();
        for index in 0..250u64 {
            sent(&mut store, "a1", &format!("s{index}"), 100 + index * 2);
            received(&mut store, "a1", &format!("r{index}"), 101 + index * 2);
        }
        state.poll_frames(&store);
        let sent = state.sent_frames();
        assert_eq!(sent.len(), SENT_SHOWN);
        assert_eq!(sent[0].frame.payload_data, "s249");
        assert_eq!(sent[199].frame.payload_data, "s50");
        assert!(sent.iter().all(|c| c.frame.direction == WsDirection::Sent));
    }

    #[test]
    fn 응답은_전송_시각_이후에_받은_프레임만_경과_시간과_함께_준다() {
        let (mut state, mut store) = on_a();
        received(&mut store, "a1", "before", 90);
        state.poll_frames(&store);
        // 아직 보내지 않았으면 응답도 없다.
        assert!(state.replies().is_empty());

        state.begin_send("ping", 100);
        sent(&mut store, "a1", "ping", 101);
        received(&mut store, "a1", "same-ms", 100);
        received(&mut store, "a1", "pong", 130);
        state.poll_frames(&store);
        let replies: Vec<(u64, &str)> = state
            .replies()
            .into_iter()
            .map(|(elapsed, frame)| (elapsed, frame.payload_data.as_str()))
            .collect();
        // 정렬은 시각순이라 100ms 의 프레임이 먼저 온다.
        assert_eq!(replies, [(0, "same-ms"), (30, "pong")]);
    }

    // ── load and send ───────────────────────────────────────────────────────

    #[test]
    fn 프레임을_불러오면_원본이_되고_바이너리_여부를_따른다() {
        let (mut state, mut store) = on_a();
        sent(&mut store, "a1", "{\"op\":1}", 10);
        frame_with(&mut store, "a1", WsDirection::Sent, 2, "AAEC", 11);
        state.poll_frames(&store);
        let (text_id, binary_id) = (state.frames()[0].id, state.frames()[1].id);
        assert!(!state.as_binary());

        assert_eq!(state.load(text_id), Some("{\"op\":1}"));
        assert_eq!(state.original().map(|o| o.id), Some(text_id));
        assert!(!state.as_binary());
        assert_eq!(state.load(binary_id), Some("AAEC"));
        assert!(state.as_binary());
        // 없는 번호는 원본을 바꾸지 않는다.
        assert_eq!(state.load(999), None);
        assert_eq!(state.original().map(|o| o.id), Some(binary_id));
    }

    #[test]
    fn 내용이_같은_프레임도_누른_것만_원본이다() {
        let (mut state, mut store) = on_a();
        sent(&mut store, "a1", "ping", 10);
        sent(&mut store, "a1", "ping", 10);
        state.poll_frames(&store);
        let (first, second) = (state.frames()[0].id, state.frames()[1].id);
        state.load(second);
        assert_eq!(state.original().map(|o| o.id), Some(second));
        assert_ne!(first, second);
    }

    #[test]
    fn 소켓과_내용이_있고_바쁘지_않을_때만_보낼_수_있다() {
        let mut state = WsRepeaterState::default();
        assert!(!state.can_send("ping"));
        assert_eq!(state.begin_send("ping", 1), None);
        state.set_connections(vec![conn("a1", A)]);
        assert!(!state.can_send(""));
        assert_eq!(state.begin_send("", 1), None);
        assert_eq!(state.sent_at(), None);
        assert!(state.can_send("ping"));
        assert!(state.begin_send("ping", 1).is_some());
        assert!(state.busy());
        assert!(!state.can_send("ping"));
        assert_eq!(state.begin_send("ping", 2), None);
        assert_eq!(state.sent_at(), Some(1));
    }

    #[test]
    fn 열린_소켓이_있으면_재연결_없이_없으면_재연결해서_보낸다() {
        let (mut state, _) = on_a();
        assert_eq!(state.live_label_key(), "panel.wsSender.noLive");
        assert_eq!(state.send_label_key(), "panel.wsSender.reconnectSend");
        let request = state.begin_send("ping", 7).unwrap();
        assert_eq!(
            request,
            SendRequest {
                url: A.into(),
                payload: "ping".into(),
                as_binary: false,
                reconnect: true,
            }
        );
        assert_eq!(state.send_label_key(), "panel.wsSender.sending");
        state.finish_send(&SendOutcome::default(), Lang::En);

        // 닫히는 중(2)인 소켓과 다른 URL 의 소켓은 열린 것이 아니다.
        assert!(state.set_live_sockets(vec![live(A, 2, "page"), live(B, 1, "page")]));
        assert_eq!(state.live_label_key(), "panel.wsSender.noLive");
        assert!(state.set_live_sockets(vec![live(A, 1, "page")]));
        assert!(!state.set_live_sockets(vec![live(A, 1, "page")]));
        assert_eq!(state.live_label_key(), "panel.wsSender.live");
        assert_eq!(state.send_label_key(), "panel.wsSender.send");
        assert!(!state.begin_send("ping", 8).unwrap().reconnect);
        state.finish_send(&SendOutcome::default(), Lang::En);

        state.set_live_sockets(vec![live(A, 1, "https://widget.example")]);
        assert_eq!(state.live_label_key(), "panel.wsSender.liveFrame");
    }

    #[test]
    fn 바이너리_원본을_불러온_채로_보내면_바이너리로_나간다() {
        let (mut state, mut store) = on_a();
        frame_with(&mut store, "a1", WsDirection::Sent, 2, "AAEC", 10);
        state.poll_frames(&store);
        let id = state.frames()[0].id;
        state.load(id);
        assert!(state.begin_send("AAED", 1).unwrap().as_binary);
    }

    #[test]
    fn 전송_결과는_성공_재연결_실패에_따라_상태_문구가_된다() {
        let (mut state, _) = on_a();
        let finish = |state: &mut WsRepeaterState, outcome: SendOutcome, lang| {
            state.begin_send("x", 1);
            state.finish_send(&outcome, lang);
            assert!(!state.busy());
            state.status().map(str::to_owned)
        };
        let ok = SendOutcome {
            ok: true,
            ..SendOutcome::default()
        };
        assert_eq!(
            finish(&mut state, ok.clone(), Lang::En).as_deref(),
            Some("Sent")
        );
        assert_eq!(finish(&mut state, ok, Lang::Ko).as_deref(), Some("전송됨"));
        let reconnected = SendOutcome {
            ok: true,
            reconnected: true,
            ..SendOutcome::default()
        };
        assert_eq!(
            finish(&mut state, reconnected, Lang::En).as_deref(),
            Some("Reconnected and sent")
        );
        let failed = SendOutcome {
            ok: false,
            error: Some("socket closed".into()),
            reconnected: false,
        };
        assert_eq!(
            finish(&mut state, failed, Lang::En).as_deref(),
            Some("socket closed")
        );
        // 오류 문구가 없으면 "error", 빈 문구면 아무것도 보이지 않는다.
        assert_eq!(
            finish(&mut state, SendOutcome::default(), Lang::En).as_deref(),
            Some("error")
        );
        let blank = SendOutcome {
            error: Some(String::new()),
            ..SendOutcome::default()
        };
        assert_eq!(finish(&mut state, blank, Lang::En), None);
    }

    // ── sequence ────────────────────────────────────────────────────────────

    #[test]
    fn 시퀀스에_넣은_단계는_초안과_기본_대기_100ms를_갖는다() {
        let (mut state, mut store) = on_a();
        state.add_step("");
        assert!(state.steps().is_empty());
        state.add_step("auth");
        assert_eq!(
            state.steps(),
            [WsStep {
                payload: "auth".into(),
                delay_ms: 100.0,
                as_binary: Some(false),
            }]
        );
        // 바이너리 원본을 불러온 뒤에 넣은 단계는 바이너리다.
        frame_with(&mut store, "a1", WsDirection::Sent, 2, "AAEC", 10);
        state.poll_frames(&store);
        let id = state.frames()[0].id;
        state.load(id);
        state.add_step("AAEC");
        assert_eq!(state.steps()[1].as_binary, Some(true));
        // React 의 단계 객체와 같은 모양으로 직렬화된다.
        assert_eq!(
            serde_json::to_value(&state.steps()[0]).unwrap(),
            json!({ "payload": "auth", "delayMs": 100.0, "asBinary": false })
        );
    }

    #[test]
    fn 단계의_대기_시간을_고치고_단계를_지울_수_있다() {
        let (mut state, _) = on_a();
        state.add_step("one");
        state.add_step("two");
        state.set_step_delay(1, "250");
        state.set_step_delay(0, "oops");
        assert_eq!(state.steps()[0].delay_ms, 0.0);
        assert_eq!(state.steps()[1].delay_ms, 250.0);
        // 범위 밖은 무시한다.
        state.set_step_delay(5, "1");
        state.remove_step(5);
        assert_eq!(state.steps().len(), 2);
        state.remove_step(0);
        assert_eq!(state.steps(), [step("two", 250.0)]);
    }

    #[test]
    fn 시퀀스는_소켓과_단계가_있고_바쁘지_않을_때만_시작한다() {
        let mut state = WsRepeaterState::default();
        state.add_step("orphan");
        // 소켓이 없다.
        assert_eq!(state.begin_play(1), None);
        state.set_connections(vec![conn("a1", A)]);
        let (url, steps) = state.begin_play(5).expect("the sequence starts");
        assert_eq!(url, A);
        assert_eq!(steps.len(), 1);
        assert!(state.busy());
        assert_eq!(state.sent_at(), Some(5));
        assert_eq!(state.begin_play(6), None);

        let (mut empty, _) = on_a();
        assert!(!empty.can_play());
        assert_eq!(empty.begin_play(1), None);
        assert_eq!(empty.sent_at(), None);
    }

    #[test]
    fn 시퀀스_결과는_보낸_개수나_실패한_단계_번호를_상태에_적는다() {
        let (mut state, _) = on_a();
        state.add_step("one");
        let finish = |state: &mut WsRepeaterState, sent, error: Option<&str>, lang| {
            state.begin_play(1);
            state.finish_play(
                &WsSequenceResult {
                    sent,
                    error: error.map(str::to_owned),
                },
                lang,
            );
            assert!(!state.busy());
            state.status().map(str::to_owned)
        };
        assert_eq!(
            finish(&mut state, 3, None, Lang::En).as_deref(),
            Some("Sequence sent (3 frames)")
        );
        assert_eq!(
            finish(&mut state, 3, None, Lang::Ko).as_deref(),
            Some("시퀀스 전송 완료 (3개 프레임)")
        );
        // 실패한 단계는 보낸 개수 + 1 번째다.
        assert_eq!(
            finish(&mut state, 1, Some("socket closed"), Lang::En).as_deref(),
            Some("Step 2 failed: socket closed")
        );
        // 빈 오류 문구는 실패로 치지 않는다(원본의 `r.error ?`).
        assert_eq!(
            finish(&mut state, 1, Some(""), Lang::En).as_deref(),
            Some("Sequence sent (1 frames)")
        );
        // 오류 문구의 `$&` 는 replaceAll 의 치환 규칙을 따른다.
        assert_eq!(
            finish(&mut state, 0, Some("cost $$5"), Lang::En).as_deref(),
            Some("Step 1 failed: cost $5")
        );
    }

    // ── the panel in a window ───────────────────────────────────────────────

    /// The page side: answers `sockets` and `send` from what the test put in.
    #[derive(Default)]
    struct FakePage {
        sockets: Mutex<Option<Vec<LiveSocket>>>,
        sent: Mutex<Vec<SendRequest>>,
        /// One answer per send, in order; the last one repeats.
        outcomes: Mutex<Vec<SendOutcome>>,
    }

    impl FakePage {
        fn with_open(urls: &[&str]) -> Arc<Self> {
            let page = Self::default();
            *page.sockets.lock().unwrap() =
                Some(urls.iter().map(|url| live(url, 1, "page")).collect());
            *page.outcomes.lock().unwrap() = vec![SendOutcome {
                ok: true,
                ..SendOutcome::default()
            }];
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
            self.sent.lock().unwrap().push(request);
            let mut outcomes = self.outcomes.lock().unwrap();
            if outcomes.len() > 1 {
                outcomes.remove(0)
            } else {
                outcomes.first().cloned().unwrap_or_default()
            }
        }
    }

    /// Two sockets; the list is newest first, so B is selected at the start.
    fn two_sockets() -> Arc<Mutex<TrafficStore>> {
        let mut store = TrafficStore::default();
        socket(&mut store, "a", A, 1);
        socket(&mut store, "b", B, 2);
        sent(&mut store, "a", "from-a", 10);
        sent(&mut store, "b", "{\"op\":\"sub\"}", 20);
        received(&mut store, "b", "welcome", 21);
        frame_with(&mut store, "b", WsDirection::Sent, 2, "AAEC", 30);
        Arc::new(Mutex::new(store))
    }

    fn open_panel<'a>(
        cx: &'a mut TestAppContext,
        store: &Arc<Mutex<TrafficStore>>,
        page: &Arc<FakePage>,
    ) -> (Entity<WsRepeaterPanel>, &'a mut VisualTestContext) {
        cx.update(rever_ui_kit::text_input::bind_keys);
        let (store, page) = (store.clone(), page.clone());
        cx.add_window_view(|_, cx| {
            WsRepeaterPanel::new(store, page, Lang::En, ThemeMode::Light, cx)
        })
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"));
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
    }

    fn draft(panel: &Entity<WsRepeaterPanel>, cx: &mut VisualTestContext) -> String {
        panel.read_with(cx, |panel, cx| panel.draft.read(cx).text().to_owned())
    }

    fn set_draft(panel: &Entity<WsRepeaterPanel>, cx: &mut VisualTestContext, text: &str) {
        panel.update(cx, |panel, cx| panel.set_draft(text, cx));
        cx.run_until_parked();
    }

    fn status(panel: &Entity<WsRepeaterPanel>, cx: &mut VisualTestContext) -> Option<String> {
        panel.read_with(cx, |panel, _| panel.state.status().map(str::to_owned))
    }

    fn wait(cx: &mut VisualTestContext, duration: Duration) {
        cx.executor().advance_clock(duration);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn 소켓이_없으면_안내만_보인다(cx: &mut TestAppContext) {
        let store = Arc::new(Mutex::new(TrafficStore::default()));
        let page = FakePage::with_open(&[]);
        let (_panel, cx) = open_panel(cx, &store, &page);
        assert!(cx.debug_bounds("wsr-empty").is_some());
        assert!(cx.debug_bounds("wsr-send").is_none());
        assert!(cx.debug_bounds("wsr-socket").is_none());
    }

    #[gpui::test]
    fn 열자마자_첫_소켓과_보낸_프레임과_열린_소켓을_보여준다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.state.url(), Some(B));
            let sent: Vec<&str> = panel
                .state
                .sent_frames()
                .iter()
                .map(|c| c.frame.payload_data.as_str())
                .collect();
            // 최신순.
            assert_eq!(sent, ["AAEC", "{\"op\":\"sub\"}"]);
            assert_eq!(panel.state.live_label_key(), "panel.wsSender.live");
        });
        // 원본의 data-testid 와 같은 이름으로 찾을 수 있다.
        for selector in [
            "wsr-socket",
            "wsr-sent",
            "wsr-live",
            "wsr-add-step",
            "wsr-send",
            "wsr-replies",
        ] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector}");
        }
        // 시퀀스와 원본과 상태는 아직 없다.
        for selector in ["wsr-sequence", "wsr-original", "wsr-status"] {
            assert!(cx.debug_bounds(selector).is_none(), "{selector}");
        }
        // 왼쪽 칸은 내용 230px 이다(테두리 1px 제외).
        assert_eq!(cx.debug_bounds("wsr-sent").unwrap().size.width, px(230.0));
    }

    #[gpui::test]
    fn 보낸_프레임을_누르면_원본과_초안이_된다(cx: &mut TestAppContext) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        click(cx, "wsr-sent-frame-1");
        assert_eq!(draft(&panel, cx), "{\"op\":\"sub\"}");
        assert!(cx.debug_bounds("wsr-original").is_some());
        panel.read_with(cx, |panel, _| assert!(!panel.state.as_binary()));

        // 바이너리 프레임을 고치지 않고 보내면 바이너리로 나간다.
        click(cx, "wsr-sent-frame-0");
        assert_eq!(draft(&panel, cx), "AAEC");
        click(cx, "wsr-send");
        assert_eq!(
            page.sent(),
            [SendRequest {
                url: B.into(),
                payload: "AAEC".into(),
                as_binary: true,
                reconnect: false,
            }]
        );
        assert_eq!(status(&panel, cx).as_deref(), Some("Sent"));
        assert!(cx.debug_bounds("wsr-status").is_some());
    }

    #[gpui::test]
    fn 빈_초안으로는_보내지도_단계를_넣지도_못한다(cx: &mut TestAppContext) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        click(cx, "wsr-send");
        click(cx, "wsr-add-step");
        click(cx, "wsr-format");
        click(cx, "wsr-draft");
        cx.simulate_keystrokes("cmd-enter");
        assert!(page.sent().is_empty());
        panel.read_with(cx, |panel, _| {
            assert!(panel.state.steps().is_empty());
            assert_eq!(panel.state.sent_at(), None);
        });
    }

    #[gpui::test]
    fn cmd_enter와_ctrl_enter는_보내고_enter는_줄을_바꾼다(cx: &mut TestAppContext) {
        // 페이지에는 A 만 열려 있고, 고른 소켓은 B 다: 재연결을 청한다.
        let (store, page) = (two_sockets(), FakePage::with_open(&[A]));
        let (panel, cx) = open_panel(cx, &store, &page);
        click(cx, "wsr-draft");
        cx.simulate_input("a");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("b");
        assert!(page.sent().is_empty());
        assert_eq!(draft(&panel, cx), "a\nb");
        cx.simulate_keystrokes("cmd-enter");
        cx.simulate_keystrokes("ctrl-enter");
        let sent = page.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].payload, "a\nb");
        assert!(sent[0].reconnect);
        assert!(!sent[0].as_binary);
        // 원본은 보낸 뒤에도 초안을 비우지 않는다.
        assert_eq!(draft(&panel, cx), "a\nb");
    }

    #[gpui::test]
    fn 보내는_동안에는_다시_보낼_수_없다(cx: &mut TestAppContext) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        set_draft(&panel, cx, "ping");
        panel.update(cx, |panel, cx| {
            panel.send(cx);
            // 답이 오기 전.
            assert!(panel.state.busy());
            assert_eq!(panel.state.send_label_key(), "panel.wsSender.sending");
            panel.send(cx);
            panel.play(cx);
        });
        cx.run_until_parked();
        assert_eq!(page.sent().len(), 1);
        panel.read_with(cx, |panel, _| assert!(!panel.state.busy()));
    }

    #[gpui::test]
    fn 전송이_실패하면_도구줄에_오류를_보인다(cx: &mut TestAppContext) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        *page.outcomes.lock().unwrap() = vec![SendOutcome {
            ok: false,
            error: Some("socket closed".into()),
            reconnected: false,
        }];
        let (panel, cx) = open_panel(cx, &store, &page);
        set_draft(&panel, cx, "ping");
        click(cx, "wsr-send");
        assert_eq!(status(&panel, cx).as_deref(), Some("socket closed"));
    }

    #[gpui::test]
    fn 전송_뒤에_받은_프레임이_반_초마다_응답_칸에_쌓인다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        // 보내기 전에는 받은 프레임이 있어도 응답 칸은 비어 있다.
        assert!(cx.debug_bounds("wsr-reply-0").is_none());
        set_draft(&panel, cx, "ping");
        click(cx, "wsr-send");
        let sent_at = panel
            .read_with(cx, |panel, _| panel.state.sent_at())
            .expect("the send was timed");
        received(&mut store.lock().unwrap(), "b", "pong", sent_at + 12);
        sent(&mut store.lock().unwrap(), "b", "ping", sent_at + 1);
        // 아직 0.5초가 지나지 않았다.
        assert!(cx.debug_bounds("wsr-reply-0").is_none());
        wait(cx, FRAMES_POLL);
        assert!(cx.debug_bounds("wsr-reply-0").is_some());
        panel.read_with(cx, |panel, _| {
            let replies = panel.state.replies();
            assert_eq!(replies.len(), 1);
            assert_eq!(replies[0].0, 12);
            assert_eq!(replies[0].1.payload_data, "pong");
            // 방금 보낸 프레임은 보낸 목록 맨 위에 온다.
            assert_eq!(panel.state.sent_frames()[0].frame.payload_data, "ping");
        });
    }

    #[gpui::test]
    fn 재연결로_생긴_새_연결의_프레임도_같은_소켓으로_읽는다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[]));
        let (panel, cx) = open_panel(cx, &store, &page);
        {
            let mut store = store.lock().unwrap();
            socket(&mut store, "b2", B, 3);
            received(&mut store, "b2", "hello-again", 50);
        }
        wait(cx, CONNECTIONS_POLL);
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.state.url(), Some(B));
            assert_eq!(panel.state.urls(), [B, A]);
            assert!(
                panel
                    .state
                    .frames()
                    .iter()
                    .any(|c| c.frame.payload_data == "hello-again")
            );
        });
    }

    #[gpui::test]
    fn 소켓_목록에서_고르면_그_소켓의_프레임으로_바뀌고_상태가_지워진다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        click(cx, "wsr-sent-frame-0");
        click(cx, "wsr-send");
        assert!(status(&panel, cx).is_some());

        assert!(cx.debug_bounds("wsr-socket-menu").is_none());
        click(cx, "wsr-socket");
        // 목록은 선택 상자 바로 아래에, 적어도 그 폭으로 열린다.
        let select = cx.debug_bounds("wsr-socket").unwrap();
        let menu = cx
            .debug_bounds("wsr-socket-menu")
            .expect("the list is open");
        assert!(menu.top() >= select.bottom());
        assert_eq!(menu.left(), select.left());
        assert!(menu.size.width >= select.size.width);
        // 목록은 [B, A].
        click(cx, "wsr-socket-option-1");
        // GPUI keeps the last bounds of an element that is gone, so ask the panel.
        panel.read_with(cx, |panel, _| assert!(!panel.socket_open));
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.state.url(), Some(A));
            assert_eq!(panel.state.original(), None);
            assert_eq!(panel.state.sent_at(), None);
            assert_eq!(panel.state.status(), None);
            assert_eq!(payloads(&panel.state), ["from-a"]);
            // A 는 페이지에 열린 소켓이 없다.
            assert_eq!(panel.state.send_label_key(), "panel.wsSender.reconnectSend");
        });
        // 원본은 소켓을 바꿔도 초안을 지우지 않는다.
        assert_eq!(draft(&panel, cx), "AAEC");

        // 밖을 누르면 고르지 않고 닫힌다.
        click(cx, "wsr-socket");
        panel.read_with(cx, |panel, _| assert!(panel.socket_open));
        click(cx, "wsr-target");
        panel.read_with(cx, |panel, _| assert!(!panel.socket_open));
        panel.read_with(cx, |panel, _| assert_eq!(panel.state.url(), Some(A)));
    }

    #[gpui::test]
    fn 이미_고른_소켓을_다시_고르면_목록만_닫히고_아무것도_지워지지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        click(cx, "wsr-sent-frame-0");
        click(cx, "wsr-send");
        click(cx, "wsr-socket");
        // 목록은 [B, A] 이고 B 가 고른 소켓이다: <select> 는 change 를 내지 않는다.
        click(cx, "wsr-socket-option-0");
        panel.read_with(cx, |panel, _| {
            assert!(!panel.socket_open);
            assert_eq!(panel.state.url(), Some(B));
            assert!(panel.state.original().is_some());
            assert!(panel.state.sent_at().is_some());
            assert_eq!(panel.state.status(), Some("Sent"));
            assert!(!panel.state.frames().is_empty());
        });
    }

    #[gpui::test]
    fn json_정렬_버튼은_초안을_들여쓴다(cx: &mut TestAppContext) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        set_draft(&panel, cx, "{\"op\":\"sub\",\"ids\":[1,2]}");
        click(cx, "wsr-format");
        assert_eq!(
            draft(&panel, cx),
            "{\n  \"op\": \"sub\",\n  \"ids\": [\n    1,\n    2\n  ]\n}"
        );
        // JSON 이 아니면 그대로 둔다.
        set_draft(&panel, cx, "SUBSCRIBE\nid:1");
        click(cx, "wsr-format");
        assert_eq!(draft(&panel, cx), "SUBSCRIBE\nid:1");
    }

    #[gpui::test]
    fn 시퀀스는_단계를_순서대로_보내고_첫_단계만_재연결을_청한다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        set_draft(&panel, cx, "auth");
        click(cx, "wsr-add-step");
        set_draft(&panel, cx, "subscribe");
        click(cx, "wsr-add-step");
        assert!(cx.debug_bounds("wsr-sequence").is_some());
        assert!(cx.debug_bounds("wsr-step-delay-1").is_some());

        click(cx, "wsr-play");
        // 첫 단계 앞의 100ms 가 지나기 전에는 아무것도 나가지 않는다.
        assert!(page.sent().is_empty());
        panel.read_with(cx, |panel, _| assert!(panel.state.busy()));
        wait(cx, Duration::from_millis(100));
        assert_eq!(page.sent().len(), 1);
        wait(cx, Duration::from_millis(100));
        let sent = page.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(
            (sent[0].payload.as_str(), sent[0].reconnect),
            ("auth", true)
        );
        assert_eq!(
            (sent[1].payload.as_str(), sent[1].reconnect),
            ("subscribe", false)
        );
        assert!(
            sent.iter()
                .all(|request| request.url == B && !request.as_binary)
        );
        assert_eq!(
            status(&panel, cx).as_deref(),
            Some("Sequence sent (2 frames)")
        );
        panel.read_with(cx, |panel, _| {
            assert!(!panel.state.busy());
            assert!(panel.state.sent_at().is_some());
        });
    }

    #[gpui::test]
    fn 시퀀스는_실패한_단계에서_멈추고_몇_번째인지_알린다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        *page.outcomes.lock().unwrap() = vec![
            SendOutcome {
                ok: true,
                ..SendOutcome::default()
            },
            SendOutcome {
                ok: false,
                error: Some("socket closed".into()),
                reconnected: false,
            },
        ];
        let (panel, cx) = open_panel(cx, &store, &page);
        for payload in ["one", "two", "three"] {
            set_draft(&panel, cx, payload);
            click(cx, "wsr-add-step");
        }
        click(cx, "wsr-play");
        wait(cx, Duration::from_millis(100));
        wait(cx, Duration::from_millis(100));
        wait(cx, Duration::from_millis(100));
        assert_eq!(page.sent().len(), 2);
        assert_eq!(
            status(&panel, cx).as_deref(),
            Some("Step 2 failed: socket closed")
        );
    }

    #[gpui::test]
    fn 대기_칸을_고치면_그_단계의_대기_시간이_바뀌고_0이면_기다리지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        set_draft(&panel, cx, "now");
        click(cx, "wsr-add-step");
        click(cx, "wsr-step-delay-0");
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("0");
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.state.steps()[0].delay_ms, 0.0)
        });
        click(cx, "wsr-play");
        // 대기가 0 이면 시계를 돌리지 않아도 나간다.
        assert_eq!(page.sent().len(), 1);

        click(cx, "wsr-step-delay-0");
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("2.5e2");
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.state.steps()[0].delay_ms, 250.0)
        });
        cx.simulate_input("x");
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.state.steps()[0].delay_ms, 0.0)
        });
    }

    #[gpui::test]
    fn 단계를_지우면_남은_단계가_제_대기_칸을_지킨다(cx: &mut TestAppContext) {
        let (store, page) = (two_sockets(), FakePage::with_open(&[B]));
        let (panel, cx) = open_panel(cx, &store, &page);
        for payload in ["one", "two"] {
            set_draft(&panel, cx, payload);
            click(cx, "wsr-add-step");
        }
        click(cx, "wsr-step-delay-1");
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_input("7");
        click(cx, "wsr-step-remove-0");
        panel.read_with(cx, |panel, cx| {
            assert_eq!(panel.state.steps(), [step("two", 7.0)]);
            assert_eq!(panel.delay_fields.len(), 1);
            assert_eq!(panel.delay_fields[0].input.read(cx).text(), "7");
        });
        click(cx, "wsr-step-remove-0");
        panel.read_with(cx, |panel, _| {
            assert!(panel.state.steps().is_empty());
            assert!(panel.delay_fields.is_empty());
        });
        let _ = page;
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
        assert_eq!(label(cx), "panel.wsSender.noLive");
        *page.sockets.lock().unwrap() = Some(vec![live(B, 1, "https://widget.example")]);
        wait(cx, Duration::from_millis(1000));
        assert_eq!(label(cx), "panel.wsSender.noLive");
        wait(cx, Duration::from_millis(1000));
        assert_eq!(label(cx), "panel.wsSender.liveFrame");
        // ws:sockets 가 배열이 아닌 것을 주면 원본은 상태를 바꾸지 않는다.
        *page.sockets.lock().unwrap() = None;
        wait(cx, SOCKETS_POLL);
        assert_eq!(label(cx), "panel.wsSender.liveFrame");
    }
}
