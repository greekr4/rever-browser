//! Port of `src/renderer/src/components/network/TrafficDetailDrawer.tsx` — one
//! captured request in three tabs: overview, headers, body.
//!
//! The React component asks the main process for the request and shows
//! "Loading…" meanwhile; here the capture store is read directly when the view
//! is made, so there is no loading state. The view is rebuilt for every request
//! id, which resets the tab the way the component's effect does.

use std::sync::Mutex;

use gpui::{
    AnyElement, ClickEvent, Context, Div, Entity, EventEmitter, FontWeight, SharedString, Window,
    div, prelude::*, px,
};
use rever_core::traffic::{Headers, StoredRequest, TrafficStore};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::ThemeMode;
use url::Url;

use crate::json_view::JsonView;
use crate::traffic_list::status_suffix;
use crate::ui::{MONO, Tokens, button, ghost_button, seg_button};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Headers,
    Body,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Overview, Tab::Headers, Tab::Body];

    /// The tab names are not translated in the Electron app either.
    pub fn label(self) -> &'static str {
        match self {
            Tab::Overview => "overview",
            Tab::Headers => "headers",
            Tab::Body => "body",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LoadState {
    /// The request left the capture buffer.
    NotFound,
    /// The capture store could not be read.
    Error,
    Loaded(Box<StoredRequest>),
}

/// The request as it is in the store right now.
pub fn load(store: &Mutex<TrafficStore>, request_id: &str) -> LoadState {
    match store.lock() {
        // Poisoned: a capture thread died while writing.
        Err(_) => LoadState::Error,
        Ok(store) => store
            .get(request_id)
            .map_or(LoadState::NotFound, |request| {
                LoadState::Loaded(Box::new(request.clone()))
            }),
    }
}

/// The chat draft "Send to chat" starts from.
pub fn send_to_chat_prompt(data: &StoredRequest) -> String {
    format!(
        "Analyze this request: {} {} (id: {}{})\n\nQuestion: ",
        data.method,
        data.url,
        data.request_id,
        status_suffix(data.status)
    )
}

/// The decoded query pairs of `url`, or `None` when there are none or the URL does not parse.
pub fn parse_query_params(url: &str) -> Option<Vec<(String, String)>> {
    let url = Url::parse(url).ok()?;
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    (!pairs.is_empty()).then_some(pairs)
}

/// `—` unless both ends are known (a zero timestamp counts as unknown).
pub fn elapsed_label(data: &StoredRequest) -> String {
    match data.completed_at {
        Some(completed) if completed != 0 && data.started_at != 0 => {
            format!("{}ms", i128::from(completed) - i128::from(data.started_at))
        }
        _ => "—".to_owned(),
    }
}

/// The label/value pairs of the overview tab, top to bottom.
pub fn overview_fields(data: &StoredRequest) -> Vec<(&'static str, String)> {
    let dash = || "—".to_owned();
    vec![
        ("Method", data.method.clone()),
        ("URL", data.url.clone()),
        ("Host", data.host.clone()),
        ("Type", data.resource_type.clone()),
        ("Status", data.status.map_or_else(dash, |s| s.to_string())),
        ("MIME", data.mime_type.clone().unwrap_or_else(dash)),
        (
            "Size",
            data.encoded_data_length
                .map_or_else(dash, |n| format!("{n} bytes")),
        ),
        ("Elapsed", elapsed_label(data)),
        ("requestId", data.request_id.clone()),
    ]
}

/// A header by lowercase name. CDP reports header names with whatever casing the wire used.
pub fn header_value<'a>(headers: Option<&'a Headers>, name: &str) -> Option<&'a str> {
    headers?
        .iter()
        .find(|(key, _)| !key.is_empty() && key.to_lowercase() == name)
        .map(|(_, value)| value.as_str())
}

/// What the body tab shows under "Response body".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseBody<'a> {
    /// The body could not be fetched.
    Error(&'a str),
    /// A binary body kept as base64, with its length in characters.
    Base64 {
        chars: usize,
    },
    Text(Option<&'a str>),
}

pub fn response_body(data: &StoredRequest) -> ResponseBody<'_> {
    let body = data.response_body.as_deref();
    match data.response_body_error.as_deref() {
        Some(error) if !error.is_empty() => ResponseBody::Error(error),
        _ if data.response_body_base64 == Some(true) => ResponseBody::Base64 {
            chars: body.map_or(0, |body| body.encode_utf16().count()),
        },
        _ => ResponseBody::Text(body),
    }
}

/// What the drawer asks of its host.
#[derive(Clone, Debug, PartialEq)]
pub enum TrafficDetailDrawerEvent {
    /// Put this text in the chat composer ("Send to chat").
    PushDraft(String),
    /// Load this request (by id) into the repeater ("Send to repeater").
    SendToRepeater(String),
    Close,
}

pub struct TrafficDetailDrawer {
    state: LoadState,
    tab: Tab,
    /// The body viewers exist only while the body tab shows, so leaving the tab
    /// resets their tree and mode as unmounting does in React.
    request_json: Option<Entity<JsonView>>,
    response_json: Option<Entity<JsonView>>,
    lang: Lang,
    theme_mode: ThemeMode,
}

impl EventEmitter<TrafficDetailDrawerEvent> for TrafficDetailDrawer {}

impl TrafficDetailDrawer {
    pub fn new(
        store: &Mutex<TrafficStore>,
        request_id: &str,
        lang: Lang,
        theme_mode: ThemeMode,
    ) -> Self {
        Self {
            state: load(store, request_id),
            tab: Tab::Overview,
            request_json: None,
            response_json: None,
            lang,
            theme_mode,
        }
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.request_json = None;
        self.response_json = None;
        if tab == Tab::Body
            && let LoadState::Loaded(data) = &self.state
        {
            let (lang, theme_mode) = (self.lang, self.theme_mode);
            let mut viewer = |body: Option<&str>, headers: Option<&Headers>| {
                let body = body.filter(|body| !body.is_empty())?;
                let content_type = header_value(headers, "content-type");
                let view = JsonView::new(body, content_type, lang, theme_mode);
                Some(cx.new(|_| view))
            };
            self.request_json = viewer(
                data.request_post_data.as_deref(),
                data.request_headers.as_ref(),
            );
            if let ResponseBody::Text(body) = response_body(data) {
                self.response_json = viewer(body, data.response_headers.as_ref());
            }
        }
        cx.notify();
    }

    fn on_send_to_chat(&mut self, cx: &mut Context<Self>) {
        if let LoadState::Loaded(data) = &self.state {
            cx.emit(TrafficDetailDrawerEvent::PushDraft(send_to_chat_prompt(
                data,
            )));
        }
    }

    fn on_send_to_repeater(&mut self, cx: &mut Context<Self>) {
        if let LoadState::Loaded(data) = &self.state {
            cx.emit(TrafficDetailDrawerEvent::SendToRepeater(
                data.request_id.clone(),
            ));
        }
    }

    fn overview(&self, data: &StoredRequest, t: &Tokens) -> AnyElement {
        let fields = overview_fields(data).into_iter().map(|(label, value)| {
            // `<Field>`; the request id is a `<code>`.
            div()
                .mb(px(8.0))
                .child(
                    div()
                        .mb(px(2.0))
                        .text_size(px(11.0))
                        .opacity(0.5)
                        .child(label),
                )
                .child(
                    div()
                        .when(label == "requestId", |value| value.font_family(MONO))
                        .child(value),
                )
        });
        div()
            .children(fields)
            .when_some(parse_query_params(&data.url), |overview, params| {
                overview.child(
                    div()
                        .mt(px(14.0))
                        .child(heading(self.lang.t("drawer.queryParams")))
                        .child(pair_table(params, t)),
                )
            })
            .into_any_element()
    }

    fn headers(&self, data: &StoredRequest, t: &Tokens) -> AnyElement {
        let table = |headers: Option<&Headers>| match headers.filter(|headers| !headers.is_empty())
        {
            Some(headers) => pair_table(
                headers
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
                t,
            ),
            None => div().mt(px(6.0)).opacity(0.5).child("—"),
        };
        div()
            .child(heading(self.lang.t("drawer.request")))
            .child(table(data.request_headers.as_ref()))
            .child(heading(self.lang.t("drawer.response")).mt(px(14.0)))
            .child(table(data.response_headers.as_ref()))
            .into_any_element()
    }

    fn body(&self, data: &StoredRequest, t: &Tokens) -> AnyElement {
        // `<BodyBlock>`: the viewer, or a dash when there is no body.
        let block = |label: &'static str, viewer: &Option<Entity<JsonView>>| {
            div()
                .mb(px(14.0))
                .child(heading(label))
                .map(|block| match viewer {
                    Some(viewer) => block.child(viewer.clone()),
                    None => block.child(div().opacity(0.5).child("—")),
                })
        };
        let response_heading = || heading(self.lang.t("drawer.responseBody"));
        let response = match response_body(data) {
            ResponseBody::Error(error) => div().child(response_heading()).child(
                div()
                    .mt(px(5.0))
                    .text_size(px(11.0))
                    .text_color(t.theme.status_error)
                    .child(format!("error: {error}")),
            ),
            ResponseBody::Base64 { chars } => div().child(response_heading()).child(
                div()
                    .mt(px(5.0))
                    .text_size(px(11.0))
                    .opacity(0.5)
                    .child(format!("(base64-encoded binary, {chars} chars)")),
            ),
            ResponseBody::Text(_) => block("Response body", &self.response_json),
        };
        div()
            .child(block("Request body", &self.request_json))
            .child(response)
            .into_any_element()
    }
}

/// An `<h4>` of the drawer: bold, 12px, 6px above what it titles.
fn heading(label: impl Into<SharedString>) -> Div {
    div()
        .mb(px(6.0))
        .text_size(px(12.0))
        .font_weight(FontWeight::BOLD)
        .child(label.into())
}

/// The two-column table of the query parameters and the headers.
fn pair_table(pairs: Vec<(String, String)>, t: &Tokens) -> Div {
    div()
        .w_full()
        .text_size(px(11.0))
        .children(pairs.into_iter().map(|(key, value)| {
            div()
                .flex()
                .items_start()
                .border_b_1()
                .border_color(t.theme.border)
                .child(
                    // 130px of content plus the cell's padding.
                    div()
                        .w(px(142.0))
                        .flex_none()
                        .px(px(6.0))
                        .py(px(3.0))
                        .child(div().opacity(0.7).child(key)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px(px(6.0))
                        .py(px(3.0))
                        .font_family(MONO)
                        .child(value),
                )
        }))
}

impl Render for TrafficDetailDrawer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::resolve(self.theme_mode, window);
        let lang = self.lang;
        let content = match (&self.state, self.tab) {
            (LoadState::NotFound, _) => div()
                .mt(px(12.0))
                .opacity(0.5)
                .child("Request no longer in capture buffer.")
                .into_any_element(),
            (LoadState::Error, _) => div()
                .mt(px(12.0))
                .text_color(t.theme.status_error)
                .child("Failed to load request details.")
                .into_any_element(),
            (LoadState::Loaded(data), Tab::Overview) => self.overview(data, &t),
            (LoadState::Loaded(data), Tab::Headers) => self.headers(data, &t),
            (LoadState::Loaded(data), Tab::Body) => self.body(data, &t),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.theme.bg)
            .text_color(t.theme.text)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(t.theme.border)
                    .text_size(px(12.0))
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .child(lang.t("drawer.request")),
                    )
                    .child(
                        button("detail-send", lang.t("drawer.sendToChat"), &t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.on_send_to_chat(cx)),
                        ),
                    )
                    .child(
                        button("detail-repeater", lang.t("traffic.sendToRepeater"), &t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.on_send_to_repeater(cx)),
                        ),
                    )
                    .child(div().flex_1())
                    .child(
                        // `.btn-ghost.btn-icon`
                        ghost_button("detail-close", "✕", &t)
                            .min_w(px(22.0))
                            .p(px(3.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                cx.emit(TrafficDetailDrawerEvent::Close)
                            })),
                    ),
            )
            .child(
                // `nav.seg`
                div()
                    .flex()
                    .flex_none()
                    .gap(px(2.0))
                    .border_b_1()
                    .border_color(t.theme.border)
                    .children(Tab::ALL.map(|tab| {
                        seg_button(tab.label(), tab.label(), self.tab == tab, &t)
                            .flex_1()
                            .flex()
                            .justify_center()
                            .px(px(8.0))
                            .pt(px(6.0))
                            .pb(px(4.0))
                            .on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.set_tab(tab, cx)
                                }),
                            )
                    })),
            )
            .child(
                div()
                    .id("detail-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(12.0))
                    .text_size(px(12.0))
                    .child(content),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rever_core::traffic::RequestPatch;

    fn request() -> StoredRequest {
        StoredRequest {
            request_id: "42.7".into(),
            url: "https://api.test/v1/items?q=a+b&tag=%ED%95%9C&tag=2&empty".into(),
            host: "api.test".into(),
            method: "POST".into(),
            resource_type: "Fetch".into(),
            started_at: 1_000,
            ..StoredRequest::default()
        }
    }

    fn headers(pairs: &[(&str, &str)]) -> Headers {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn 저장소에_있는_요청을_불러온다() {
        let store = Mutex::new(TrafficStore::default());
        store.lock().unwrap().upsert(
            RequestPatch {
                request_id: "1".into(),
                url: Some("https://a.test/x".into()),
                method: Some("GET".into()),
                ..RequestPatch::default()
            },
            5,
        );
        let LoadState::Loaded(data) = load(&store, "1") else {
            panic!("expected the request");
        };
        assert_eq!(data.url, "https://a.test/x");
        assert_eq!(data.host, "a.test");
        assert_eq!(load(&store, "gone"), LoadState::NotFound);
    }

    #[test]
    fn 저장소를_읽을_수_없으면_오류_상태다() {
        let store = std::sync::Arc::new(Mutex::new(TrafficStore::default()));
        let poisoner = store.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the lock");
        })
        .join();
        assert_eq!(load(&store, "1"), LoadState::Error);
    }

    #[test]
    fn 새로_만든_서랍은_개요_탭에서_시작한다() {
        let store = Mutex::new(TrafficStore::default());
        let drawer = TrafficDetailDrawer::new(&store, "none", Lang::En, ThemeMode::Light);
        assert_eq!(drawer.tab(), Tab::Overview);
        assert_eq!(drawer.state, LoadState::NotFound);
        assert_eq!(Tab::ALL.map(Tab::label), ["overview", "headers", "body"]);
    }

    #[test]
    fn 채팅_초안에는_메서드_주소_id와_상태가_들어간다() {
        let mut data = request();
        data.url = "https://api.test/x".into();
        assert_eq!(
            send_to_chat_prompt(&data),
            "Analyze this request: POST https://api.test/x (id: 42.7)\n\nQuestion: "
        );
        data.status = Some(403);
        assert_eq!(
            send_to_chat_prompt(&data),
            "Analyze this request: POST https://api.test/x (id: 42.7, status: 403)\n\nQuestion: "
        );
        // Zero is falsy in the TypeScript, so it is left out like a missing status.
        data.status = Some(0);
        assert!(send_to_chat_prompt(&data).contains("(id: 42.7)"));
    }

    #[test]
    fn 쿼리_파라미터는_풀어서_순서대로_돌려준다() {
        let pair = |key: &str, value: &str| (key.to_owned(), value.to_owned());
        assert_eq!(
            parse_query_params(&request().url),
            Some(vec![
                pair("q", "a b"),
                pair("tag", "한"),
                pair("tag", "2"),
                pair("empty", "")
            ])
        );
    }

    #[test]
    fn 쿼리가_없거나_주소가_깨졌으면_파라미터가_없다() {
        assert_eq!(parse_query_params("https://a.test/path"), None);
        assert_eq!(parse_query_params("https://a.test/path?"), None);
        assert_eq!(parse_query_params("https://a.test/path?&&"), None);
        assert_eq!(parse_query_params("/relative?a=1"), None);
        assert_eq!(parse_query_params(""), None);
    }

    #[test]
    fn 걸린_시간은_양쪽_시각이_있을_때만_적는다() {
        let mut data = request();
        assert_eq!(elapsed_label(&data), "—");
        data.completed_at = Some(1_250);
        assert_eq!(elapsed_label(&data), "250ms");
        // A clock that went backwards shows as it is.
        data.completed_at = Some(900);
        assert_eq!(elapsed_label(&data), "-100ms");
        data.completed_at = Some(0);
        assert_eq!(elapsed_label(&data), "—");
        data.completed_at = Some(1_250);
        data.started_at = 0;
        assert_eq!(elapsed_label(&data), "—");
    }

    #[test]
    fn 개요는_없는_값을_줄표로_채운다() {
        let fields = overview_fields(&request());
        let labels: Vec<_> = fields.iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            [
                "Method",
                "URL",
                "Host",
                "Type",
                "Status",
                "MIME",
                "Size",
                "Elapsed",
                "requestId"
            ]
        );
        let values: Vec<_> = fields.iter().map(|(_, value)| value.as_str()).collect();
        assert_eq!(values[0], "POST");
        assert_eq!(values[2], "api.test");
        assert_eq!(values[3], "Fetch");
        assert_eq!(&values[4..8], ["—", "—", "—", "—"]);
        assert_eq!(values[8], "42.7");
    }

    #[test]
    fn 개요는_있는_값을_단위와_함께_적는다() {
        let mut data = request();
        data.status = Some(0);
        data.mime_type = Some(String::new());
        data.encoded_data_length = Some(0);
        data.completed_at = Some(1_034);
        let fields = overview_fields(&data);
        let values: Vec<_> = fields.iter().map(|(_, value)| value.as_str()).collect();
        // Present-but-empty values are shown, not replaced by a dash.
        assert_eq!(&values[4..8], ["0", "", "0 bytes", "34ms"]);

        data.status = Some(200);
        data.mime_type = Some("application/json".into());
        data.encoded_data_length = Some(15_360);
        let fields = overview_fields(&data);
        let values: Vec<_> = fields.iter().map(|(_, value)| value.as_str()).collect();
        assert_eq!(&values[4..7], ["200", "application/json", "15360 bytes"]);
    }

    #[test]
    fn 헤더는_대소문자와_상관없이_찾는다() {
        let map = headers(&[("Content-Type", "application/json"), ("x-id", "7")]);
        assert_eq!(
            header_value(Some(&map), "content-type"),
            Some("application/json")
        );
        assert_eq!(header_value(Some(&map), "x-id"), Some("7"));
        assert_eq!(header_value(Some(&map), "accept"), None);
        assert_eq!(header_value(None, "content-type"), None);
        // The name to look for is expected in lowercase, as the TypeScript passes it.
        assert_eq!(header_value(Some(&map), "Content-Type"), None);
    }

    #[test]
    fn 응답_본문은_오류_바이너리_텍스트_순으로_가린다() {
        let mut data = request();
        assert_eq!(response_body(&data), ResponseBody::Text(None));
        data.response_body = Some("{\"ok\":true}".into());
        assert_eq!(
            response_body(&data),
            ResponseBody::Text(Some("{\"ok\":true}"))
        );
        data.response_body_base64 = Some(false);
        assert!(matches!(response_body(&data), ResponseBody::Text(Some(_))));
        data.response_body_base64 = Some(true);
        data.response_body = Some("iVBORw0KGgo=".into());
        assert_eq!(response_body(&data), ResponseBody::Base64 { chars: 12 });
        data.response_body = None;
        assert_eq!(response_body(&data), ResponseBody::Base64 { chars: 0 });
        // An error wins over everything else; an empty one does not count.
        data.response_body_error = Some(String::new());
        assert_eq!(response_body(&data), ResponseBody::Base64 { chars: 0 });
        data.response_body_error = Some("No resource with given identifier".into());
        assert_eq!(
            response_body(&data),
            ResponseBody::Error("No resource with given identifier")
        );
    }

    // ── The view, in a test window ──────────────────────────────────────────

    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use std::cell::RefCell;
    use std::rc::Rc;

    type Events = Rc<RefCell<Vec<TrafficDetailDrawerEvent>>>;

    fn open(
        patch: Option<RequestPatch>,
        cx: &mut TestAppContext,
    ) -> (
        Entity<TrafficDetailDrawer>,
        Events,
        &mut VisualTestContext,
    ) {
        let mut store = TrafficStore::default();
        if let Some(patch) = patch {
            store.upsert(patch, 1_000);
        }
        let store = Mutex::new(store);
        let (drawer, cx) = cx.add_window_view(move |_, _| {
            TrafficDetailDrawer::new(&store, "r1", Lang::En, ThemeMode::Light)
        });
        let events = Events::default();
        cx.update(|_, cx| {
            let events = events.clone();
            cx.subscribe(&drawer, move |_, event: &TrafficDetailDrawerEvent, _| {
                events.borrow_mut().push(event.clone())
            })
            .detach();
        });
        (drawer, events, cx)
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let at = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"))
            .center();
        cx.simulate_click(at, Modifiers::none());
    }

    fn json_exchange() -> RequestPatch {
        RequestPatch {
            request_id: "r1".into(),
            url: Some("https://api.test/v1/orders?dry=1".into()),
            method: Some("POST".into()),
            resource_type: Some("Fetch".into()),
            status: Some(201),
            request_headers: Some(headers(&[("Content-Type", "application/json")])),
            request_post_data: Some(r#"{"sku":"KB-87"}"#.into()),
            response_headers: Some(headers(&[("content-type", "application/json")])),
            response_body: Some(r#"{"ok":true}"#.into()),
            ..RequestPatch::default()
        }
    }

    #[gpui::test]
    fn 탭을_누르면_바뀌고_본문_뷰어는_본문_탭에서만_있다(
        cx: &mut TestAppContext,
    ) {
        let (drawer, _, cx) = open(Some(json_exchange()), cx);
        let viewers = |drawer: &Entity<TrafficDetailDrawer>, cx: &mut VisualTestContext| {
            drawer.read_with(cx, |drawer, _| {
                (
                    drawer.tab,
                    drawer.request_json.as_ref().map(Entity::entity_id),
                    drawer.response_json.as_ref().map(Entity::entity_id),
                )
            })
        };
        assert_eq!(viewers(&drawer, cx), (Tab::Overview, None, None));
        click("headers", cx);
        assert_eq!(viewers(&drawer, cx), (Tab::Headers, None, None));
        click("body", cx);
        let on_body = viewers(&drawer, cx);
        assert_eq!(on_body.0, Tab::Body);
        assert!(on_body.1.is_some() && on_body.2.is_some());
        assert!(cx.debug_bounds("json-pretty").is_some());
        // Pressing the tab it is on changes nothing: the viewers keep their state.
        click("body", cx);
        assert_eq!(viewers(&drawer, cx), on_body);
        click("overview", cx);
        assert_eq!(viewers(&drawer, cx), (Tab::Overview, None, None));
        // Coming back makes new viewers, as remounting does in React.
        click("body", cx);
        let again = viewers(&drawer, cx);
        assert!(again.1.is_some() && again.1 != on_body.1);
    }

    #[gpui::test]
    fn 본문이_없거나_오류면_뷰어를_만들지_않는다(cx: &mut TestAppContext) {
        let patch = RequestPatch {
            request_post_data: Some(String::new()),
            response_body: None,
            response_body_error: Some("No data found for resource".into()),
            ..json_exchange()
        };
        let (drawer, _, cx) = open(Some(patch), cx);
        click("body", cx);
        drawer.read_with(cx, |drawer, _| {
            assert_eq!(drawer.tab, Tab::Body);
            assert!(drawer.request_json.is_none());
            assert!(drawer.response_json.is_none());
        });
        assert!(cx.debug_bounds("json-pretty").is_none());
    }

    #[gpui::test]
    fn 채팅_보내기와_닫기는_이벤트로_나간다(cx: &mut TestAppContext) {
        let (_drawer, events, cx) = open(Some(json_exchange()), cx);
        click("detail-send", cx);
        click("detail-close", cx);
        assert_eq!(
            *events.borrow(),
            [
                TrafficDetailDrawerEvent::PushDraft(
                    "Analyze this request: POST https://api.test/v1/orders?dry=1 (id: r1, status: 201)\n\nQuestion: "
                        .into()
                ),
                TrafficDetailDrawerEvent::Close,
            ]
        );
    }

    #[gpui::test]
    fn 버퍼에_없는_요청은_탭을_눌러도_보낼_것이_없다(cx: &mut TestAppContext) {
        let (drawer, events, cx) = open(None, cx);
        assert_eq!(
            drawer.read_with(cx, |drawer, _| drawer.state.clone()),
            LoadState::NotFound
        );
        click("body", cx);
        click("detail-send", cx);
        assert!(events.borrow().is_empty());
        drawer.read_with(cx, |drawer, _| {
            assert_eq!(drawer.tab, Tab::Body);
            assert!(drawer.request_json.is_none());
        });
    }
}
