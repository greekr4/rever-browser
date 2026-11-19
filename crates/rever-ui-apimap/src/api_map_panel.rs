//! Port of `src/renderer/src/components/panels/ApiMapPanel.tsx` — the ring map of captured API endpoints.
//!
//! The logic (domains, hosts, which nodes a ring shows, where each one sits) is in plain
//! functions; [`ApiMapPanel`] draws their result as the component's SVG does. The
//! endpoints come from the capture store through `rever_analysis`, where the component
//! read them from the renderer's traffic store. Clicking an endpoint emits
//! [`ApiMapEvent::OpenRequest`] where the component called `openDetail`.

use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::{Arc, Mutex};

use gpui::{
    AnyElement, AnyView, App, BorderStyle, Bounds, BoxShadow, ClickEvent, Context, Div, Entity,
    EventEmitter, Font, FontWeight, MouseButton, MouseDownEvent, PaintQuad, Path, PathBuilder,
    Pixels, Point, Rgba, ShapedLine, SharedString, Stateful, TextRun, Window, canvas, deferred,
    div, font, point, prelude::*, px, quad, relative, rgb, rgba, size,
};
use rever_analysis::endpoints::{EndpointSummary, summarize_endpoints};
use rever_core::traffic::{ListFilter, MAX_ENTRIES, StoredRequest, TrafficStore};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};

// Inline English in the component (not dictionary keys), kept as it is there.
const EMPTY: &str =
    "No API calls captured yet — browse the target site and XHR/Fetch requests will map here.";
const HINT: &str = "click a subdomain to open it";

/// `monospace` in the component (`--mono` is not defined by the stylesheet). GPUI resolves
/// no generic families, and Menlo is the monospace font every macOS has.
const MONO: &str = "Menlo";

// ── Colors ──────────────────────────────────────────────────────────────────

/// The kit's theme plus the styles.css tokens it lacks.
#[derive(Clone, Copy)]
pub struct Tokens {
    pub theme: Theme,
    /// `--surface`
    pub surface: Rgba,
    /// `--accent-text`
    pub accent_text: Rgba,
    /// `--chip-warn-text`
    pub chip_warn_text: Rgba,
    /// The menu's drop shadow (`--shadow`).
    pub shadow: Rgba,
}

impl Tokens {
    pub fn resolve(mode: ThemeMode, window: &Window) -> Self {
        let theme = Theme::resolve(mode, window.appearance());
        // The kit decides light or dark; this only asks which one it picked.
        if theme.bg == Theme::dark().bg {
            Self::dark()
        } else {
            Self::light()
        }
    }

    pub fn dark() -> Self {
        Self {
            theme: Theme::dark(),
            surface: rgb(0x1a1a1a),
            accent_text: rgb(0x4a8fff),
            chip_warn_text: rgb(0xeeccaa),
            shadow: rgba(0x00000066),
        }
    }

    pub fn light() -> Self {
        Self {
            theme: Theme::light(),
            surface: rgb(0xffffff),
            accent_text: rgb(0x1c56bd),
            chip_warn_text: rgb(0x8a6a1f),
            shadow: rgba(0x00000029),
        }
    }
}

/// The component reads `--act-*` variables the stylesheet never defines, so its
/// fallback colors are what every theme shows. An unknown method is `--text-dim`.
pub fn method_color(method: &str, t: &Tokens) -> Rgba {
    match method {
        "GET" => rgb(0x1f9d55),
        "POST" => rgb(0xc2691a),
        "PUT" => rgb(0x2f6fed),
        "PATCH" => rgb(0x8b5cf6),
        "DELETE" => rgb(0xe5484d),
        _ => t.theme.text_dim,
    }
}

fn fade(color: Rgba, opacity: f32) -> Rgba {
    Rgba {
        a: color.a * opacity,
        ..color
    }
}

// ── JavaScript string semantics ─────────────────────────────────────────────

/// JavaScript's `string.length`.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `text.slice(0, units)`. A surrogate pair split by the cut is left out whole, since a
/// Rust string cannot hold the lone half.
fn utf16_prefix(text: &str, units: usize) -> &str {
    let mut seen = 0;
    let end = text
        .char_indices()
        .find(|(_, c)| {
            seen += c.len_utf16();
            seen > units
        })
        .map_or(text.len(), |(at, _)| at);
    text.get(..end).unwrap_or(text)
}

/// `text.slice(-units)` for `units > 0` (a split pair is left out).
fn utf16_suffix(text: &str, units: usize) -> &str {
    let mut seen = 0;
    let start = text
        .char_indices()
        .rev()
        .take_while(|(_, c)| {
            seen += c.len_utf16();
            seen <= units
        })
        .last()
        .map_or(text.len(), |(at, _)| at);
    text.get(start..).unwrap_or("")
}

// ── Endpoints ───────────────────────────────────────────────────────────────

// The TypeScript pattern is /(oauth|token|login|signin|auth|session|refresh)/i.
const AUTH_HINTS: [&str; 7] = [
    "oauth", "token", "login", "signin", "auth", "session", "refresh",
];

/// Second-level labels that are part of the public suffix, not the site name:
/// mail.naver.co.kr → naver.co.kr, not co.kr.
const PUBLIC_SLD: [&str; 12] = [
    "co", "ne", "or", "go", "re", "pe", "ac", "com", "net", "org", "edu", "gov",
];

/// Endpoints the map keeps (`MAX_ENDPOINTS` of the renderer's traffic store).
pub const MAX_ENDPOINTS: usize = 300;

/// One method + origin + templated path, with how often it was called.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ApiEndpoint {
    /// `METHOD origin/templated-path`
    pub key: String,
    pub origin: String,
    /// Empty when unknown: [`host_of`] then reads the host out of `origin`.
    pub host: String,
    pub path: String,
    pub method: String,
    pub count: usize,
    pub last_request_id: String,
    /// Epoch milliseconds of the last request.
    pub last_seen: u64,
    pub statuses: Vec<u16>,
    /// The path looks like part of a sign-in flow.
    pub auth: bool,
}

/// A request already counted: its bucket (`endpointKey` on the store's entry) and the
/// status last reported for it.
#[derive(Debug)]
struct Counted {
    key: String,
    status: Option<u16>,
}

/// The endpoint buckets of the renderer's traffic store (`aggregate` there), kept up
/// from the capture store instead of from network events.
///
/// Endpoints are counted once per request, never by rescanning what the capture holds.
/// They outlive the entry ring buffer on purpose — the map should not forget an
/// endpoint just because its request scrolled out.
#[derive(Debug, Default)]
pub struct EndpointMap {
    /// In insertion order, as the keys of the store's object. shortcut: lookups scan
    /// at most 300 buckets; switch to an indexed map if the cap grows.
    buckets: Vec<ApiEndpoint>,
    /// By request id, for the requests still in the capture.
    counted: HashMap<String, Counted>,
    /// Id of the newest request the last `apply` saw.
    newest: Option<String>,
}

impl EndpointMap {
    /// Take in what the capture store gained since the last call: new XHR/Fetch
    /// requests are counted, and new statuses added to their endpoints.
    pub fn apply(&mut self, store: &TrafficStore) {
        // A ring that is not full has dropped nothing, so when the newest request of
        // last time is missing from it the capture was cleared — and clearing it
        // clears the map. shortcut: a clear followed by a full ring of new requests
        // before the next call goes unnoticed; have the store count its clears if
        // the host ever refreshes that rarely.
        let gone = |id: &String| store.get(id).is_none();
        if store.len() < MAX_ENTRIES && self.newest.as_ref().is_some_and(gone) {
            self.buckets.clear();
        }
        self.counted.retain(|id, _| !gone(id));
        let live = store.list(&ListFilter {
            limit: Some(MAX_ENTRIES),
            ..ListFilter::default()
        });
        self.newest = live.first().map(|request| request.request_id.clone());
        // Oldest first — the order the requests arrived in.
        for request in live.into_iter().rev() {
            if !self.counted.contains_key(&request.request_id) {
                // None for anything but an XHR/Fetch request with a readable URL.
                let Some(summary) = summarize_endpoints(std::iter::once(request)).pop() else {
                    continue;
                };
                let key = self.count(request, summary);
                let counted = Counted { key, status: None };
                self.counted.insert(request.request_id.clone(), counted);
            }
            let Some(counted) = self.counted.get_mut(&request.request_id) else {
                continue;
            };
            // The store's `response` event: seen once per status the request reports.
            if counted.status == request.status {
                continue;
            }
            counted.status = request.status;
            // The bucket may have been dropped since the request was counted.
            let bucket = self.buckets.iter_mut().find(|ep| ep.key == counted.key);
            if let (Some(status), Some(bucket)) = (request.status, bucket)
                && !bucket.statuses.contains(&status)
            {
                bucket.statuses.push(status);
            }
        }
    }

    /// Count `request` into its bucket and return the bucket's key.
    fn count(&mut self, request: &StoredRequest, summary: EndpointSummary) -> String {
        let key = format!("{} {}{}", summary.method, summary.origin, summary.path);
        match self.buckets.iter_mut().find(|ep| ep.key == key) {
            Some(hit) => {
                hit.count += 1;
                hit.last_request_id = request.request_id.clone();
                hit.last_seen = request.started_at;
            }
            None => {
                let lower = summary.path.to_ascii_lowercase();
                self.buckets.push(ApiEndpoint {
                    key: key.clone(),
                    // The summaries carry no host; `host_of` derives it from the origin,
                    // as the component does for endpoints captured before `host` existed.
                    host: String::new(),
                    auth: AUTH_HINTS.iter().any(|hint| lower.contains(hint)),
                    origin: summary.origin,
                    path: summary.path,
                    method: summary.method,
                    count: 1,
                    last_request_id: request.request_id.clone(),
                    last_seen: request.started_at,
                    statuses: Vec::new(),
                });
            }
        }
        // drop the least-recently-seen endpoints; of two seen in the same millisecond
        // the one known longer goes first, as the store's stable sort has it
        while self.buckets.len() > MAX_ENDPOINTS {
            let oldest = self
                .buckets
                .iter()
                .enumerate()
                .min_by_key(|(_, ep)| ep.last_seen)
                .map(|(index, _)| index);
            let Some(oldest) = oldest else { break };
            self.buckets.remove(oldest);
        }
        key
    }

    /// The endpoints, most-called first.
    pub fn sorted(&self) -> Vec<ApiEndpoint> {
        let mut endpoints = self.buckets.clone();
        // The component sorts by count alone and its sort is stable, so equally busy
        // endpoints stay in first-seen order and a node does not jump as traffic arrives.
        endpoints.sort_by(|a, b| b.count.cmp(&a.count));
        endpoints
    }
}

/// The store's XHR/Fetch requests as endpoints, most-called first: what the map shows
/// when it has seen nothing but what the store holds now.
pub fn endpoints_of(store: &TrafficStore) -> Vec<ApiEndpoint> {
    let mut map = EndpointMap::default();
    map.apply(store);
    map.sorted()
}

/// endpoints captured before `host` existed only carry an origin
pub fn host_of(endpoint: &ApiEndpoint) -> String {
    if !endpoint.host.is_empty() {
        return endpoint.host.clone();
    }
    let origin = endpoint.origin.as_str();
    let rest = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .unwrap_or(origin);
    // /:\d+$/
    let digits = rest.trim_end_matches(|c: char| c.is_ascii_digit());
    match digits.strip_suffix(':') {
        Some(host) if digits.len() < rest.len() => host.to_owned(),
        _ => rest.to_owned(),
    }
}

/// mail.naver.com → naver.com · api.shop.co.kr → shop.co.kr · localhost → localhost
pub fn base_domain(host: &str) -> String {
    let parts: Vec<&str> = host.split('.').collect();
    let [_, .., sld, tld] = parts.as_slice() else {
        return host.to_owned();
    };
    let take = if utf16_len(tld) <= 3 && PUBLIC_SLD.contains(sld) {
        3
    } else {
        2
    };
    let kept: Vec<&str> = parts
        .iter()
        .skip(parts.len().saturating_sub(take))
        .copied()
        .collect();
    kept.join(".")
}

/// the part of the host that is not the base domain — 'mail', or '@' for the apex
pub fn sub_label(host: &str, base: &str) -> String {
    if host == base {
        return "@".to_owned();
    }
    // host.slice(0, host.length - base.length - 1); a negative end counts from the end.
    let length = utf16_len(host) as i64;
    let end = length - utf16_len(base) as i64 - 1;
    let end = if end < 0 { (length + end).max(0) } else { end };
    utf16_prefix(host, end as usize).to_owned()
}

/// The last `n - 1` characters behind an ellipsis when `text` is longer than `n`.
pub fn clip(text: &str, n: usize) -> String {
    if utf16_len(text) <= n {
        return text.to_owned();
    }
    match n {
        // slice(-(0 - 1)) is slice(1): all but the first unit.
        0 => format!("…{}", text.chars().skip(1).collect::<String>()),
        // slice(-0) is slice(0): the whole string.
        1 => format!("…{text}"),
        _ => format!("…{}", utf16_suffix(text, n - 1)),
    }
}

/// Base domains with how many endpoints each has, most first.
pub fn domains(endpoints: &[ApiEndpoint]) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for endpoint in endpoints {
        let domain = base_domain(&host_of(endpoint));
        match counts.iter_mut().find(|(known, _)| *known == domain) {
            Some((_, count)) => *count += 1,
            None => counts.push((domain, 1)),
        }
    }
    // Stable, as Array#sort is: ties keep first-seen order.
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    counts
}

/// The picked domain while it still has endpoints, else the busiest one.
pub fn pick_domain<'a>(picked: Option<&'a str>, domains: &'a [(String, usize)]) -> Option<&'a str> {
    picked
        // `picked && …`: an empty string counts as nothing picked.
        .filter(|picked| !picked.is_empty() && domains.iter().any(|(domain, _)| domain == picked))
        .or_else(|| domains.first().map(|(domain, _)| domain.as_str()))
}

#[derive(Debug, PartialEq)]
pub struct HostGroup<'a> {
    pub host: String,
    pub eps: Vec<&'a ApiEndpoint>,
    /// Requests over all of the host's endpoints.
    pub hits: usize,
}

/// hosts under the selected domain, busiest first
pub fn hosts<'a>(endpoints: &'a [ApiEndpoint], domain: &str) -> Vec<HostGroup<'a>> {
    let mut groups: Vec<HostGroup<'a>> = Vec::new();
    // `if (!domain) return []`: an empty name is no domain.
    if domain.is_empty() {
        return groups;
    }
    for endpoint in endpoints {
        let host = host_of(endpoint);
        if base_domain(&host) != domain {
            continue;
        }
        match groups.iter_mut().find(|group| group.host == host) {
            Some(group) => group.eps.push(endpoint),
            None => groups.push(HostGroup {
                host,
                eps: vec![endpoint],
                hits: 0,
            }),
        }
    }
    for group in &mut groups {
        group.hits = group.eps.iter().map(|endpoint| endpoint.count).sum();
    }
    groups.sort_by(|a, b| b.eps.len().cmp(&a.eps.len()));
    groups
}

/// The host whose endpoints are shown, or `None` for the ring of hosts.
pub fn pick_host(focus: Option<&str>, hosts: &[HostGroup]) -> Option<String> {
    let known = focus.filter(|focus| hosts.iter().any(|group| group.host == *focus));
    let host = match (known, hosts) {
        (Some(focus), _) => Some(focus.to_owned()),
        // a single-host domain has nothing to drill into — show its endpoints straight away
        (None, [only]) => Some(only.host.clone()),
        _ => None,
    };
    // The component tests `host` for truthiness everywhere: an empty name is no host.
    host.filter(|host| !host.is_empty())
}

/// What clicking a node does.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeAction {
    /// Drill into this host.
    Focus(String),
    /// Open the endpoint's last request, by its id.
    OpenRequest(String),
}

/// what a ring node draws — hosts and endpoints share the same shape
#[derive(Clone, Debug, PartialEq)]
pub struct MapNode {
    pub key: String,
    pub badge: String,
    pub color: Rgba,
    pub label: String,
    pub meta: String,
    pub auth: bool,
    pub title: String,
    pub action: NodeAction,
}

/// Endpoints drawn around a host.
pub const MAX_ENDPOINT_NODES: usize = 20;
/// Hosts drawn around a domain.
pub const MAX_HOST_NODES: usize = 16;

/// The ring's nodes: the endpoints of `host`, or the hosts of `domain` when none is open.
pub fn nodes(domain: &str, host: Option<&str>, hosts: &[HostGroup], t: &Tokens) -> Vec<MapNode> {
    if let Some(host) = host {
        let eps = hosts
            .iter()
            .find(|group| group.host == host)
            .map_or(&[][..], |group| group.eps.as_slice());
        return eps
            .iter()
            .take(MAX_ENDPOINT_NODES)
            .map(|ep| {
                let statuses: Vec<String> = ep.statuses.iter().map(u16::to_string).collect();
                let mut meta = format!("×{}", ep.count);
                if !statuses.is_empty() {
                    meta = format!("{meta} · {}", statuses.join("/"));
                }
                MapNode {
                    key: ep.key.clone(),
                    badge: utf16_prefix(&ep.method, 4).to_owned(),
                    color: method_color(&ep.method, t),
                    label: clip(&ep.path, 26),
                    meta,
                    auth: ep.auth,
                    title: format!("{} {}{}", ep.method, ep.origin, ep.path),
                    action: NodeAction::OpenRequest(ep.last_request_id.clone()),
                }
            })
            .collect();
    }
    hosts
        .iter()
        .take(MAX_HOST_NODES)
        .map(|group| MapNode {
            key: group.host.clone(),
            badge: group.eps.len().to_string(),
            color: t.accent_text,
            label: clip(&sub_label(&group.host, domain), 24),
            meta: format!("{} endpoints · {} reqs", group.eps.len(), group.hits),
            auth: group.eps.iter().any(|ep| ep.auth),
            title: group.host.clone(),
            action: NodeAction::Focus(group.host.clone()),
        })
        .collect()
}

/// The toolbar's summary of what the ring shows.
pub fn status_text(
    host: Option<&str>,
    node_count: usize,
    host_count: usize,
    endpoints: &[ApiEndpoint],
    domain: &str,
) -> String {
    match host {
        Some(host) => format!("{host} · {node_count} endpoints"),
        None => {
            let in_domain = endpoints
                .iter()
                .filter(|endpoint| base_domain(&host_of(endpoint)) == domain)
                .count();
            format!("{host_count} subdomains · {in_domain} endpoints")
        }
    }
}

// ── Geometry (the SVG's viewBox units) ──────────────────────────────────────

pub const W: f64 = 800.0;
pub const H: f64 = 420.0;
pub const CX: f64 = W / 2.0;
pub const CY: f64 = H / 2.0;
/// The outer ring, always drawn.
pub const OUTER_RING: f64 = 175.0;
/// The inner ring, drawn once the nodes alternate between two.
pub const INNER_RING: f64 = 112.0;
const NODE_RADIUS: f64 = 20.0;

/// More than ten nodes alternate between the two rings.
pub fn two_rings(node_count: usize) -> bool {
    node_count > 10
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Start,
    Middle,
    End,
}

/// Where a node's circle sits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub x: f64,
    pub y: f64,
    pub cos: f64,
}

/// Node `index` of `total`, clockwise from twelve o'clock.
pub fn place(index: usize, total: usize) -> Placed {
    let angle = (index as f64 / total as f64) * PI * 2.0 - PI / 2.0;
    let radius = if two_rings(total) {
        if index.is_multiple_of(2) {
            OUTER_RING
        } else {
            INNER_RING
        }
    } else if total <= 6 {
        130.0
    } else {
        155.0
    };
    Placed {
        x: CX + angle.cos() * radius,
        y: CY + angle.sin() * radius,
        cos: angle.cos(),
    }
}

impl Placed {
    /// Labels beside the map's vertical axis are centered on their node; the rest
    /// grow away from the centre.
    pub fn anchor(&self) -> Anchor {
        if self.cos.abs() < 0.25 {
            Anchor::Middle
        } else if self.cos > 0.0 {
            Anchor::Start
        } else {
            Anchor::End
        }
    }

    /// The label's anchor point and baseline; the meta line sits 12 below it.
    pub fn label_origin(&self) -> (f64, f64) {
        match self.anchor() {
            // above a node the meta line sits below the label, so the pair has to
            // clear the circle by its own height — not just the label's
            Anchor::Middle if self.y < CY => (self.x, self.y - 40.0),
            Anchor::Middle => (self.x, self.y + 34.0),
            Anchor::Start => (self.x + 26.0, self.y + 4.0),
            Anchor::End => (self.x - 26.0, self.y + 4.0),
        }
    }
}

/// `preserveAspectRatio="xMidYMid meet"`: the scale that fits the viewBox into a
/// `width` × `height` box, and the offset that centers it.
pub fn fit(width: f32, height: f32) -> (f32, f32, f32) {
    let scale = (width / W as f32).min(height / H as f32).max(0.0);
    (
        scale,
        (width - W as f32 * scale) / 2.0,
        (height - H as f32 * scale) / 2.0,
    )
}

// ── Drawing ─────────────────────────────────────────────────────────────────

enum Op {
    Path(Path<Pixels>, Rgba),
    Quad(PaintQuad),
    // Boxed: a shaped line is some 3KB, twenty times the other variants.
    Text(Box<ShapedLine>, Point<Pixels>),
}

/// An SVG `<circle>` of radius `outer` (stroke included) around `centre`.
fn disc(
    centre: Point<Pixels>,
    outer: f32,
    fill: Rgba,
    stroke: f32,
    stroke_color: Rgba,
) -> PaintQuad {
    quad(
        Bounds::centered_at(centre, size(px(outer * 2.0), px(outer * 2.0))),
        px(outer),
        fill,
        px(stroke),
        stroke_color,
        BorderStyle::Solid,
    )
}

/// A dashed circle: `stroke-dasharray="2 6"` at the map's scale.
fn ring(centre: Point<Pixels>, radius: f32, scale: f32) -> Option<Path<Pixels>> {
    let r = px(radius);
    let right = point(centre.x + r, centre.y);
    let left = point(centre.x - r, centre.y);
    let mut builder =
        PathBuilder::stroke(px(scale)).dash_array(&[px(2.0 * scale), px(6.0 * scale)]);
    builder.move_to(right);
    builder.arc_to(point(r, r), px(0.0), false, true, left);
    builder.arc_to(point(r, r), px(0.0), false, true, right);
    builder.close();
    builder.build().ok()
}

/// An SVG `<text>`: one line whose baseline passes through `at`, placed by `anchor`.
/// Returns the shaped line with the box it will cover.
fn text_at(
    window: &Window,
    text: &str,
    font_size: f32,
    weight: FontWeight,
    color: Rgba,
    at: Point<Pixels>,
    anchor: Anchor,
) -> (ShapedLine, Bounds<Pixels>) {
    // SVG text collapses line breaks, and a line shaper takes none.
    let text = text.replace(['\n', '\r'], " ");
    let run = TextRun {
        len: text.len(),
        font: Font {
            weight,
            ..font(MONO)
        },
        color: color.into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line =
        window
            .text_system()
            .shape_line(SharedString::from(text), px(font_size), &[run], None);
    let left = match anchor {
        Anchor::Start => at.x,
        Anchor::Middle => at.x - line.width / 2.0,
        Anchor::End => at.x - line.width,
    };
    let bounds = Bounds::new(
        point(left, at.y - line.ascent),
        size(line.width, line.ascent + line.descent),
    );
    (line, bounds)
}

struct Tooltip {
    text: SharedString,
    tokens: Tokens,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = &self.tokens;
        div()
            .max_w(px(520.0))
            .px(px(6.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .bg(t.theme.surface_3)
            .border_1()
            .border_color(t.theme.border_2)
            .text_size(px(11.0))
            .text_color(t.theme.text)
            .child(self.text.clone())
    }
}

/// The SVG `<title>`: a hint shown while the pointer rests on a node.
fn tooltip(text: &str, t: &Tokens) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let (text, tokens) = (SharedString::from(text.to_owned()), *t);
    move |_, cx| {
        let text = text.clone();
        cx.new(|_| Tooltip { text, tokens }).into()
    }
}

/// Everything the `<svg>` shows.
struct MapScene {
    nodes: Vec<MapNode>,
    hover: Option<String>,
    centre_kind: &'static str,
    centre_label: String,
    tokens: Tokens,
}

/// The `<svg>`: painted to fit its box as the viewBox does, with one transparent box
/// over each node taking the pointer.
fn map(scene: MapScene, view: Entity<ApiMapPanel>) -> impl IntoElement {
    canvas(
        move |bounds, window, cx| {
            let (scale, left, top) =
                fit(f32::from(bounds.size.width), f32::from(bounds.size.height));
            let mut ops: Vec<Op> = Vec::new();
            if scale <= 0.0 {
                return (ops, None);
            }
            let at = |x: f64, y: f64| {
                point(
                    bounds.origin.x + px(left + x as f32 * scale),
                    bounds.origin.y + px(top + y as f32 * scale),
                )
            };
            let s = |length: f64| length as f32 * scale;
            let t = &scene.tokens;
            let centre = at(CX, CY);
            let total = scene.nodes.len();
            let placed: Vec<Placed> = (0..total).map(|index| place(index, total)).collect();

            let inner = two_rings(total).then_some(INNER_RING);
            for radius in [Some(OUTER_RING), inner].into_iter().flatten() {
                if let Some(path) = ring(centre, s(radius), scale) {
                    ops.push(Op::Path(path, t.theme.border));
                }
            }

            for (node, spot) in scene.nodes.iter().zip(&placed) {
                let dimmed = scene.hover.as_ref().is_some_and(|key| *key != node.key);
                let mut builder = PathBuilder::stroke(px(s(1.5)));
                builder.move_to(centre);
                builder.line_to(at(spot.x, spot.y));
                if let Ok(path) = builder.build() {
                    let opacity = if dimmed { 0.12 } else { 0.4 };
                    ops.push(Op::Path(path, fade(node.color, opacity)));
                }
            }

            // centre card: a 184 × 40 rect whose 1px stroke straddles its edge
            ops.push(Op::Quad(quad(
                Bounds::centered_at(centre, size(px(s(185.0)), px(s(41.0)))),
                px(s(10.5)),
                t.surface,
                px(scale),
                t.theme.border_2,
                BorderStyle::Solid,
            )));
            let (kind, _) = text_at(
                window,
                scene.centre_kind,
                s(9.0),
                FontWeight::NORMAL,
                t.theme.text_dim,
                at(CX, CY - 3.0),
                Anchor::Middle,
            );
            let (label, _) = text_at(
                window,
                &clip(&scene.centre_label, 24),
                s(11.0),
                FontWeight::SEMIBOLD,
                t.theme.text,
                at(CX, CY + 11.0),
                Anchor::Middle,
            );
            for (line, y) in [(kind, CY - 3.0), (label, CY + 11.0)] {
                let origin = point(centre.x - line.width / 2.0, at(CX, y).y - line.ascent);
                ops.push(Op::Text(Box::new(line), origin));
            }

            let mut hits: Vec<Stateful<Div>> = Vec::new();
            for (index, (node, spot)) in scene.nodes.iter().zip(&placed).enumerate() {
                // The `<g>`'s opacity, applied to each shape in it.
                let dimmed = scene.hover.as_ref().is_some_and(|key| *key != node.key);
                let opacity = if dimmed { 0.3 } else { 1.0 };
                let middle = at(spot.x, spot.y);

                // r = 20 with a 1.6 stroke centered on it
                let circle = disc(
                    middle,
                    s(NODE_RADIUS + 0.8),
                    fade(t.surface, opacity),
                    s(1.6),
                    fade(node.color, opacity),
                );
                let mut area = circle.bounds;
                ops.push(Op::Quad(circle));

                let badge_size = if utf16_len(&node.badge) > 3 {
                    8.5
                } else {
                    10.0
                };
                let (badge, badge_box) = text_at(
                    window,
                    &node.badge,
                    s(badge_size),
                    FontWeight::BOLD,
                    fade(node.color, opacity),
                    at(spot.x, spot.y + 3.0),
                    Anchor::Middle,
                );
                ops.push(Op::Text(Box::new(badge), badge_box.origin));

                if node.auth {
                    // r = 6 with a 1.4 stroke
                    ops.push(Op::Quad(disc(
                        at(spot.x + 15.0, spot.y - 15.0),
                        s(6.7),
                        fade(t.chip_warn_text, opacity),
                        s(1.4),
                        fade(t.surface, opacity),
                    )));
                }

                let anchor = spot.anchor();
                let (tx, ty) = spot.label_origin();
                let lines = [
                    (&node.label, 10.5, t.theme.text, ty),
                    (&node.meta, 9.0, t.theme.text_dim, ty + 12.0),
                ];
                for (text, font_size, color, baseline) in lines {
                    let (line, line_box) = text_at(
                        window,
                        text,
                        s(font_size),
                        FontWeight::NORMAL,
                        fade(color, opacity),
                        at(tx, baseline),
                        anchor,
                    );
                    area = area.union(&line_box);
                    ops.push(Op::Text(Box::new(line), line_box.origin));
                }

                let (key, action) = (node.key.clone(), node.action.clone());
                let (hover_view, click_view) = (view.clone(), view.clone());
                hits.push(
                    div()
                        // Keyed by the node, not its place: a ring redrawn with other
                        // nodes must not inherit the hover of the ones it replaced.
                        .id(SharedString::from(format!("apimap-node:{}", node.key)))
                        .debug_selector(|| format!("apimap-node-{index}"))
                        .absolute()
                        .left(area.origin.x - bounds.origin.x)
                        .top(area.origin.y - bounds.origin.y)
                        .w(area.size.width)
                        .h(area.size.height)
                        .cursor_pointer()
                        // Where two nodes' boxes overlap, only the one drawn on top
                        // takes the pointer, as with the SVG's shapes.
                        .occlude()
                        .tooltip(tooltip(&node.title, t))
                        .on_hover(move |hovered, _, cx| {
                            hover_view.update(cx, |this, cx| this.set_hover(&key, *hovered, cx));
                        })
                        .on_click(move |_, _, cx| {
                            click_view.update(cx, |this, cx| this.activate(&action, cx));
                        }),
                );
            }

            let mut overlay = div()
                .relative()
                .w(bounds.size.width)
                .h(bounds.size.height)
                .children(hits)
                .into_any_element();
            overlay.prepaint_as_root(bounds.origin, bounds.size.into(), window, cx);
            (ops, Some(overlay))
        },
        |_, (ops, overlay): (Vec<Op>, Option<AnyElement>), window, cx| {
            for op in ops {
                match op {
                    Op::Path(path, color) => window.paint_path(path, color),
                    Op::Quad(quad) => window.paint_quad(quad),
                    Op::Text(line, origin) => {
                        let height = line.ascent + line.descent;
                        // A glyph that cannot be drawn leaves a gap; nothing to recover.
                        let _ = line.paint(origin, height, window, cx);
                    }
                }
            }
            if let Some(mut overlay) = overlay {
                overlay.paint(window, cx);
            }
        },
    )
    .size_full()
}

// ── View ────────────────────────────────────────────────────────────────────

/// What the panel asks of the rest of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiMapEvent {
    /// Open this request's detail (`openDetail` in the component).
    OpenRequest(String),
}

pub struct ApiMapPanel {
    store: Arc<Mutex<TrafficStore>>,
    /// Every endpoint seen so far, also those whose requests left the capture.
    map: EndpointMap,
    /// Most-called first; rebuilt by [`Self::refresh`].
    endpoints: Vec<ApiEndpoint>,
    /// The domain chosen in the select.
    picked: Option<String>,
    /// The host drilled into.
    focus: Option<String>,
    /// Key of the node under the pointer.
    hover: Option<String>,
    menu_open: bool,
    lang: Lang,
    theme_mode: ThemeMode,
}

impl EventEmitter<ApiMapEvent> for ApiMapPanel {}

impl ApiMapPanel {
    pub fn new(
        store: Arc<Mutex<TrafficStore>>,
        lang: Lang,
        theme_mode: ThemeMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut panel = Self {
            store,
            map: EndpointMap::default(),
            endpoints: Vec::new(),
            picked: None,
            focus: None,
            hover: None,
            menu_open: false,
            lang,
            theme_mode,
        };
        panel.refresh(cx);
        panel
    }

    /// Read the capture store again. The host calls this when traffic arrives.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        // A poisoned lock means a capture thread died mid-write: keep what is shown.
        let Ok(store) = self.store.lock() else {
            return;
        };
        self.map.apply(&store);
        drop(store);
        self.endpoints = self.map.sorted();
        cx.notify();
    }

    fn current_domain(&self) -> Option<String> {
        pick_domain(self.picked.as_deref(), &domains(&self.endpoints)).map(str::to_owned)
    }

    /// The select's `onChange`: it fires only when the value changes, so choosing the
    /// domain already shown keeps the open host.
    pub fn select_domain(&mut self, domain: &str, cx: &mut Context<Self>) {
        self.menu_open = false;
        if self.current_domain().as_deref() != Some(domain) {
            self.picked = Some(domain.to_owned());
            self.focus = None;
        }
        cx.notify();
    }

    /// Drill into a host, or with `None` go back to the ring of hosts.
    pub fn set_focus(&mut self, host: Option<String>, cx: &mut Context<Self>) {
        self.focus = host;
        cx.notify();
    }

    fn set_hover(&mut self, key: &str, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            self.hover = Some(key.to_owned());
        } else if self.hover.as_deref() == Some(key) {
            // Only the node being left clears it: GPUI may report the node entered
            // before the one left.
            self.hover = None;
        } else {
            return;
        }
        cx.notify();
    }

    fn activate(&mut self, action: &NodeAction, cx: &mut Context<Self>) {
        match action {
            NodeAction::Focus(host) => self.set_focus(Some(host.clone()), cx),
            NodeAction::OpenRequest(id) => {
                // the entry may have scrolled out of the 500-row ring buffer
                let known = self.store.lock().is_ok_and(|store| store.get(id).is_some());
                if known {
                    cx.emit(ApiMapEvent::OpenRequest(id.clone()));
                }
            }
        }
    }

    /// Open or close the domain list.
    pub fn toggle_menu(&mut self, cx: &mut Context<Self>) {
        self.menu_open = !self.menu_open;
        cx.notify();
    }

    /// Stands in for the native `<select>`: the current domain, and the list under it
    /// while open.
    fn domain_select(
        &self,
        domains: &[(String, usize)],
        domain: &str,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let option_label = |(name, count): &(String, usize)| format!("{name} ({count})");
        let current = domains
            .iter()
            .find(|(name, _)| name == domain)
            .map(option_label)
            .unwrap_or_default();
        let select = div()
            .id("apimap-domain")
            .debug_selector(|| "apimap-domain".into())
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.0))
            .max_w(px(260.0))
            .h(px(19.0))
            .px(px(4.0))
            .bg(t.surface)
            .text_color(t.theme.text)
            .border_1()
            .border_color(t.theme.border_2)
            .rounded(px(4.0))
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_menu(cx)))
            .child(
                // A select clips an over-long value at its edge, with no ellipsis.
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(current),
            )
            .child(div().flex_none().text_color(t.theme.text_dim).child("▾"));
        if !self.menu_open {
            return select;
        }

        let hover = t.theme.chrome_hover;
        let options = domains.iter().enumerate().map(|(index, option)| {
            let name = option.0.clone();
            div()
                .id(("apimap-domain-option", index))
                .debug_selector(|| format!("apimap-domain-option-{index}"))
                .flex()
                .gap(px(4.0))
                .px(px(8.0))
                .py(px(3.0))
                .whitespace_nowrap()
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.select_domain(&name, cx)),
                )
                .child(
                    div()
                        .w(px(10.0))
                        .flex_none()
                        .text_color(t.accent_text)
                        .when(option.0 == domain, |mark| mark.child("✓")),
                )
                .child(option_label(option))
        });
        let menu = div()
            .id("apimap-domain-menu")
            .absolute()
            .top(relative(1.0))
            .mt(px(2.0))
            .left_0()
            .min_w(relative(1.0))
            .max_h(px(240.0))
            .overflow_y_scroll()
            .py(px(4.0))
            .rounded(px(6.0))
            .bg(t.surface)
            .border_1()
            .border_color(t.theme.border)
            .shadow(vec![BoxShadow {
                color: t.shadow.into(),
                offset: point(px(0.0), px(4.0)),
                blur_radius: px(16.0),
                spread_radius: px(0.0),
            }])
            .text_color(t.theme.text)
            // The map under the list must not take its clicks.
            .occlude()
            // A click outside closes the list and goes no further, as with a native popup.
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Left {
                    this.menu_open = false;
                    cx.notify();
                }
                cx.stop_propagation();
            }))
            .children(options);
        select.child(deferred(menu).with_priority(1))
    }
}

impl Render for ApiMapPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::resolve(self.theme_mode, window);
        let domains = domains(&self.endpoints);
        let Some(domain) = pick_domain(self.picked.as_deref(), &domains) else {
            // `.panel-empty`
            return div()
                .debug_selector(|| "apimap-empty".into())
                .px(px(12.0))
                .py(px(24.0))
                .text_center()
                .text_size(px(11.0))
                // `line-height: normal` of the system font at 11px.
                .line_height(px(13.0))
                .text_color(t.theme.text_dim)
                .child(EMPTY);
        };
        let hosts = hosts(&self.endpoints, domain);
        let host = pick_host(self.focus.as_deref(), &hosts);
        let nodes = nodes(domain, host.as_deref(), &hosts, &t);
        let status = status_text(
            host.as_deref(),
            nodes.len(),
            hosts.len(),
            &self.endpoints,
            domain,
        );

        let legend = match &host {
            Some(_) => ["GET", "POST"]
                .into_iter()
                .map(|method| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .child(
                            div()
                                .size(px(8.0))
                                .rounded(px(2.0))
                                .bg(method_color(method, &t)),
                        )
                        .child(method)
                })
                .collect(),
            None => vec![div().child(HINT)],
        };
        let (ghost_hover, text) = (t.theme.chrome_hover, t.theme.text);
        // `.panel-toolbar`
        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(t.theme.border)
            .text_size(px(11.0))
            .line_height(px(13.0))
            .child(self.domain_select(&domains, domain, &t, cx))
            .when(host.is_some() && hosts.len() > 1, |bar| {
                bar.child(
                    // `.btn-ghost`
                    div()
                        .id("apimap-all-hosts")
                        .debug_selector(|| "apimap-all-hosts".into())
                        .flex_none()
                        .px(px(8.0))
                        .py(px(3.0))
                        .rounded(px(4.0))
                        .text_size(px(12.0))
                        .line_height(px(16.0))
                        .text_color(t.theme.text_2)
                        .cursor_pointer()
                        .hover(move |style| style.bg(ghost_hover).text_color(text))
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.set_focus(None, cx)),
                        )
                        .child(self.lang.t("panel.allHosts")),
                )
            })
            .child(div().opacity(0.6).child(status))
            // `margin-left: auto` on the legend. Taffy drops the row's gaps once an
            // item has an auto margin, so a spacer pushes the legend right instead.
            .child(div().flex_1())
            .child(
                div()
                    .debug_selector(|| "apimap-legend".into())
                    .flex()
                    .flex_none()
                    .gap(px(10.0))
                    .opacity(0.6)
                    .children(legend),
            );

        let scene = MapScene {
            centre_kind: if host.is_some() { "HOST" } else { "DOMAIN" },
            centre_label: match &host {
                Some(host) => sub_label(host, domain),
                None => domain.to_owned(),
            },
            nodes,
            hover: self.hover.clone(),
            tokens: t,
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .text_color(t.theme.text)
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(map(scene, cx.entity())),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rever_core::traffic::RequestPatch;

    fn endpoint(method: &str, origin: &str, path: &str, count: usize) -> ApiEndpoint {
        ApiEndpoint {
            key: format!("{method} {origin}{path}"),
            origin: origin.into(),
            path: path.into(),
            method: method.into(),
            count,
            last_request_id: format!("last:{method} {path}"),
            ..ApiEndpoint::default()
        }
    }

    /// Requests in arrival order: (id, method, URL, resource type, status).
    fn store_of(requests: &[(&str, &str, &str, &str, Option<u16>)]) -> TrafficStore {
        let mut store = TrafficStore::default();
        for (index, (id, method, url, kind, status)) in requests.iter().enumerate() {
            let patch = RequestPatch {
                request_id: (*id).into(),
                url: Some((*url).into()),
                method: Some((*method).into()),
                resource_type: Some((*kind).into()),
                status: *status,
                ..RequestPatch::default()
            };
            store.upsert(patch, 1_000 + index as u64);
        }
        store
    }

    fn keys(endpoints: &[ApiEndpoint]) -> Vec<&str> {
        endpoints.iter().map(|ep| ep.key.as_str()).collect()
    }

    // ── colors ──────────────────────────────────────────────────────────────

    #[test]
    fn 메서드마다_정해진_색을_쓰고_모르는_메서드는_흐린_글자색이다() {
        let t = Tokens::light();
        assert_eq!(method_color("GET", &t), rgb(0x1f9d55));
        assert_eq!(method_color("POST", &t), rgb(0xc2691a));
        assert_eq!(method_color("PUT", &t), rgb(0x2f6fed));
        assert_eq!(method_color("PATCH", &t), rgb(0x8b5cf6));
        assert_eq!(method_color("DELETE", &t), rgb(0xe5484d));
        assert_eq!(method_color("OPTIONS", &t), t.theme.text_dim);
        // The table is keyed by the exact method string.
        assert_eq!(method_color("get", &t), t.theme.text_dim);
        assert_eq!(method_color("", &Tokens::dark()), Theme::dark().text_dim);
    }

    // ── host, domain, labels ────────────────────────────────────────────────

    #[test]
    fn 호스트가_있으면_그대로_쓴다() {
        let ep = ApiEndpoint {
            host: "api.example.com".into(),
            origin: "https://other.test:8443".into(),
            ..ApiEndpoint::default()
        };
        assert_eq!(host_of(&ep), "api.example.com");
    }

    #[test]
    fn 호스트가_없으면_오리진에서_스킴과_포트를_뗀다() {
        let from = |origin: &str| host_of(&endpoint("GET", origin, "/", 1));
        assert_eq!(from("https://api.example.com"), "api.example.com");
        assert_eq!(from("http://localhost:8777"), "localhost");
        assert_eq!(from("https://shop.co.kr:8443"), "shop.co.kr");
        assert_eq!(from("http://[::1]:3000"), "[::1]");
        // Only http and https are stripped, and only a numeric port.
        assert_eq!(from("ws://a.test:9"), "ws://a.test");
        assert_eq!(from("HTTPS://a.test"), "HTTPS://a.test");
        assert_eq!(from("https://a.test:"), "a.test:");
        assert_eq!(from("https://a.test:80b"), "a.test:80b");
        assert_eq!(from("null"), "null");
        assert_eq!(from(""), "");
        // What the component's function returns under bun: the scheme goes once, the
        // port must be ASCII digits, and `$` does not match before a line break.
        assert_eq!(from("https://:80"), "");
        assert_eq!(from("a:1:2"), "a:1");
        assert_eq!(from("http://http://x:1"), "http://x");
        assert_eq!(from("https://a.test:٣"), "a.test:٣");
        assert_eq!(from("https://a.test:80\n"), "a.test:80\n");
    }

    #[test]
    fn 기준_도메인은_끝의_두_라벨이다() {
        assert_eq!(base_domain("mail.naver.com"), "naver.com");
        assert_eq!(base_domain("a.b.c.example.org"), "example.org");
        assert_eq!(base_domain("example.com"), "example.com");
        assert_eq!(base_domain("localhost"), "localhost");
        assert_eq!(base_domain(""), "");
    }

    #[test]
    fn 공용_접미사의_둘째_라벨은_도메인에_포함한다() {
        assert_eq!(base_domain("api.shop.co.kr"), "shop.co.kr");
        assert_eq!(base_domain("mail.naver.co.kr"), "naver.co.kr");
        assert_eq!(base_domain("www.example.com.au"), "example.com.au");
        // Three labels and nothing in front: the whole host is the domain.
        assert_eq!(base_domain("shop.co.kr"), "shop.co.kr");
        // A long TLD is not a country code: `co` is then the site's own name.
        assert_eq!(base_domain("api.co.info"), "co.info");
        // The set is matched as written, lowercase.
        assert_eq!(base_domain("api.shop.CO.kr"), "CO.kr");
    }

    #[test]
    fn ip_주소도_라벨_규칙_그대로_자른다() {
        // The component has no special case for addresses.
        assert_eq!(base_domain("127.0.0.1"), "0.1");
        assert_eq!(sub_label("127.0.0.1", "0.1"), "127.0");
    }

    #[test]
    fn 서브_라벨은_기준_도메인_앞부분이고_정점은_골뱅이다() {
        assert_eq!(sub_label("mail.naver.com", "naver.com"), "mail");
        assert_eq!(
            sub_label("cdn.assets.example.com", "example.com"),
            "cdn.assets"
        );
        assert_eq!(sub_label("example.com", "example.com"), "@");
        assert_eq!(sub_label("", ""), "@");
    }

    #[test]
    fn 서브_라벨은_slice처럼_길이만으로_자른다() {
        // An empty base still costs the separator's one character.
        assert_eq!(sub_label("abcdef", ""), "abcde");
        // A base longer than the host: the negative end counts from the end.
        assert_eq!(sub_label("abcdef", "xyz1234"), "abcd");
        assert_eq!(sub_label("ab", "abcdef"), "");
        assert_eq!(sub_label("abc", "abcd"), "a");
        // Lengths are UTF-16 units: "😀" is two.
        assert_eq!(sub_label("😀a.b", "b"), "😀a");
        assert_eq!(sub_label("a😀", "xx"), "");
    }

    #[test]
    fn 긴_글자는_말줄임표_뒤에_끝부분만_남긴다() {
        assert_eq!(clip("/v1/users", 26), "/v1/users");
        let exact = "a".repeat(26);
        assert_eq!(clip(&exact, 26), exact);
        let long = "/v1/recommendations/personalized/for-you/today";
        let clipped = clip(long, 26);
        assert_eq!(clipped, "…ersonalized/for-you/today");
        assert_eq!(utf16_len(&clipped), 26);
        assert_eq!(clip("abcdef", 3), "…ef");
        assert_eq!(clip("", 24), "");
    }

    #[test]
    fn 자르는_길이는_utf16_단위다() {
        // Four emoji are eight units: not over 8, so untouched.
        assert_eq!(clip("😀😀😀😀", 8), "😀😀😀😀");
        // Over 5: the last four units are two emoji.
        assert_eq!(clip("😀😀😀😀", 5), "…😀😀");
        // A cut through a pair leaves the pair out.
        assert_eq!(clip("😀😀😀😀", 4), "…😀");
        // slice(-0) keeps everything.
        assert_eq!(clip("abc", 1), "…abc");
        // n = 0 makes the slice start at 1 (bun: clip('abc', 0) === '…bc').
        assert_eq!(clip("abc", 0), "…bc");
        assert_eq!(clip("", 0), "");
    }

    #[test]
    fn 빈_라벨이_낀_호스트도_split과_slice_그대로_자른다() {
        // Expected values are what the component's functions return under bun.
        let cases = [
            ("a.test.", "test."),
            ("[::1]", "[::1]"),
            ("..", "."),
            ("a..b", ".b"),
            ("x.co.kr", "x.co.kr"),
            ("co.kr", "co.kr"),
            (".co.kr", ".co.kr"),
            // An empty TLD is "short", so `com` before it counts as a public suffix.
            ("a.b.com.", "b.com."),
            ("x.com.com", "x.com.com"),
            ("www.gov.uk", "www.gov.uk"),
            ("x.y.gov.uk", "y.gov.uk"),
            // "Short" is at most three UTF-16 units: "😀" is two, "😀x" three.
            ("a.b.co.한국어", "b.co.한국어"),
            ("a.b.co.😀x", "b.co.😀x"),
            ("a.b.co.😀xy", "co.😀xy"),
        ];
        for (host, base) in cases {
            assert_eq!(base_domain(host), base, "{host}");
        }
        assert_eq!(sub_label("a.test.", "test."), "a");
    }

    // ── endpoints from the store ────────────────────────────────────────────

    #[test]
    fn 요청을_메서드와_오리진과_경로_틀로_묶는다() {
        let store = store_of(&[
            (
                "r1",
                "GET",
                "https://api.example.com/users/12",
                "Fetch",
                Some(200),
            ),
            (
                "r2",
                "GET",
                "https://api.example.com/users/34?full=1",
                "XHR",
                Some(404),
            ),
            (
                "r3",
                "POST",
                "https://api.example.com/users/12",
                "Fetch",
                Some(201),
            ),
        ]);
        let endpoints = endpoints_of(&store);
        assert_eq!(
            endpoints,
            vec![
                ApiEndpoint {
                    key: "GET https://api.example.com/users/:id".into(),
                    origin: "https://api.example.com".into(),
                    host: String::new(),
                    path: "/users/:id".into(),
                    method: "GET".into(),
                    count: 2,
                    last_request_id: "r2".into(),
                    last_seen: 1_001,
                    statuses: vec![200, 404],
                    auth: false,
                },
                ApiEndpoint {
                    key: "POST https://api.example.com/users/:id".into(),
                    origin: "https://api.example.com".into(),
                    host: String::new(),
                    path: "/users/:id".into(),
                    method: "POST".into(),
                    count: 1,
                    last_request_id: "r3".into(),
                    last_seen: 1_002,
                    statuses: vec![201],
                    auth: false,
                },
            ]
        );
    }

    #[test]
    fn xhr와_fetch가_아닌_요청과_읽을_수_없는_url은_뺀다() {
        let store = store_of(&[
            ("r1", "GET", "https://a.test/", "Document", Some(200)),
            ("r2", "GET", "https://a.test/app.js", "Script", Some(200)),
            ("r3", "GET", "not a url", "Fetch", Some(200)),
            ("r4", "GET", "https://a.test/api", "Fetch", None),
        ]);
        let endpoints = endpoints_of(&store);
        assert_eq!(keys(&endpoints), ["GET https://a.test/api"]);
        // A pending request has no status yet.
        assert!(endpoints.iter().all(|ep| ep.statuses.is_empty()));
        assert!(endpoints_of(&TrafficStore::default()).is_empty());
    }

    #[test]
    fn 많이_불린_순서이고_같으면_먼저_본_것이_앞이다() {
        let store = store_of(&[
            ("r1", "GET", "https://a.test/first", "Fetch", Some(200)),
            ("r2", "GET", "https://a.test/second", "Fetch", Some(200)),
            ("r3", "GET", "https://a.test/busy", "Fetch", Some(200)),
            ("r4", "GET", "https://a.test/busy", "Fetch", Some(200)),
            // Seen again last, and still behind `first`: the order must not follow
            // whichever endpoint was called most recently.
            ("r5", "GET", "https://a.test/third", "Fetch", Some(200)),
        ]);
        assert_eq!(
            keys(&endpoints_of(&store)),
            [
                "GET https://a.test/busy",
                "GET https://a.test/first",
                "GET https://a.test/second",
                "GET https://a.test/third",
            ]
        );
    }

    #[test]
    fn 상태_코드는_처음_본_순서로_한_번씩만_담는다() {
        let store = store_of(&[
            ("r1", "GET", "https://a.test/x", "Fetch", Some(500)),
            ("r2", "GET", "https://a.test/x", "Fetch", Some(200)),
            ("r3", "GET", "https://a.test/x", "Fetch", Some(500)),
        ]);
        let endpoints = endpoints_of(&store);
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].statuses, [500, 200]);
        assert_eq!(endpoints[0].count, 3);
        assert_eq!(endpoints[0].last_request_id, "r3");
    }

    #[test]
    fn 엔드포인트는_삼백_개까지만_두고_가장_오래_안_보인_것부터_버린다() {
        let urls: Vec<String> = (0..=MAX_ENDPOINTS)
            .map(|n| format!("https://a.test/e{n}"))
            .collect();
        let requests: Vec<(&str, &str, &str, &str, Option<u16>)> = urls
            .iter()
            .map(|url| (url.as_str(), "GET", url.as_str(), "Fetch", None))
            .collect();
        let mut store = store_of(&requests);
        // The oldest endpoint went when the 301st arrived. Called again, it is a new
        // endpoint, and now the second oldest goes.
        let again = RequestPatch {
            request_id: "again".into(),
            url: Some("https://a.test/e0".into()),
            method: Some("GET".into()),
            resource_type: Some("Fetch".into()),
            ..RequestPatch::default()
        };
        store.upsert(again, 9_000);
        let endpoints = endpoints_of(&store);
        assert_eq!(endpoints.len(), MAX_ENDPOINTS);
        let all = keys(&endpoints);
        // What the renderer's store holds after the same events (run under bun): the
        // returning endpoint starts over at one call, behind the others.
        assert_eq!(all.first(), Some(&"GET https://a.test/e2"));
        assert_eq!(all.last(), Some(&"GET https://a.test/e0"));
        assert_eq!(endpoints.last().map(|ep| ep.count), Some(1));
        assert_eq!(
            endpoints.last().map(|ep| ep.last_request_id.as_str()),
            Some("again")
        );
        assert!(!all.contains(&"GET https://a.test/e1"));
        assert!(all.contains(&"GET https://a.test/e300"));

        // Exactly the limit: nothing is dropped.
        let store = store_of(&requests[..MAX_ENDPOINTS]);
        assert_eq!(endpoints_of(&store).len(), MAX_ENDPOINTS);
    }

    #[test]
    fn 로그인_흐름처럼_보이는_경로에_인증_표시를_한다() {
        let store = store_of(&[
            ("r1", "POST", "https://a.test/oauth/token", "Fetch", None),
            ("r2", "POST", "https://a.test/api/Login", "Fetch", None),
            (
                "r3",
                "GET",
                "https://a.test/v1/SESSION/refresh",
                "Fetch",
                None,
            ),
            ("r4", "GET", "https://a.test/authors", "Fetch", None),
            ("r5", "GET", "https://a.test/products", "Fetch", None),
            // The hint is looked for in the path, not in the query or the host.
            (
                "r6",
                "GET",
                "https://auth.test/items?token=1",
                "Fetch",
                None,
            ),
        ]);
        let auth: Vec<(String, bool)> = endpoints_of(&store)
            .into_iter()
            .map(|ep| (ep.path, ep.auth))
            .collect();
        assert_eq!(
            auth,
            [
                ("/oauth/token".to_owned(), true),
                ("/api/Login".to_owned(), true),
                ("/v1/SESSION/refresh".to_owned(), true),
                // "authors" contains "auth": the component's pattern is a substring match.
                ("/authors".to_owned(), true),
                ("/products".to_owned(), false),
                ("/items".to_owned(), false),
            ]
        );
    }

    #[test]
    fn 같은_밀리초에_본_엔드포인트는_먼저_들어온_것부터_버린다() {
        let mut store = TrafficStore::default();
        for n in 0..MAX_ENDPOINTS + 2 {
            store.upsert(fetch(&format!("r{n}"), &format!("https://a.test/e{n}")), 5);
        }
        let endpoints = endpoints_of(&store);
        // bun: 300 left, from e2 to e301.
        assert_eq!(endpoints.len(), MAX_ENDPOINTS);
        let all = keys(&endpoints);
        assert_eq!(all.first(), Some(&"GET https://a.test/e2"));
        assert_eq!(all.last(), Some(&"GET https://a.test/e301"));
    }

    fn fetch(id: &str, url: &str) -> RequestPatch {
        RequestPatch {
            request_id: id.into(),
            url: Some(url.into()),
            method: Some("GET".into()),
            resource_type: Some("Fetch".into()),
            ..RequestPatch::default()
        }
    }

    fn status(id: &str, status: u16) -> RequestPatch {
        RequestPatch {
            request_id: id.into(),
            status: Some(status),
            ..RequestPatch::default()
        }
    }

    #[test]
    fn 요청이_버퍼에서_밀려나도_엔드포인트는_남는다() {
        let mut store = TrafficStore::default();
        let mut map = EndpointMap::default();
        store.upsert(fetch("api", "https://a.test/api"), 1);
        map.apply(&store);
        for n in 0..MAX_ENTRIES as u64 + 100 {
            let document = RequestPatch {
                resource_type: Some("Document".into()),
                ..fetch(&format!("d{n}"), &format!("https://a.test/doc{n}"))
            };
            store.upsert(document, 2 + n);
            map.apply(&store);
        }
        assert_eq!(store.len(), MAX_ENTRIES);
        assert!(store.get("api").is_none());
        assert_eq!(keys(&map.sorted()), ["GET https://a.test/api"]);
    }

    #[test]
    fn 다시_읽어도_같은_요청을_두_번_세지_않는다() {
        let mut store = TrafficStore::default();
        let mut map = EndpointMap::default();
        store.upsert(fetch("a", "https://a.test/x"), 1);
        map.apply(&store);
        map.apply(&store);
        store.upsert(fetch("b", "https://a.test/x"), 2);
        map.apply(&store);
        map.apply(&store);
        let endpoints = map.sorted();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].count, 2);
        assert_eq!(endpoints[0].last_request_id, "b");
        assert_eq!(endpoints[0].last_seen, 2);
    }

    #[test]
    fn 상태_코드는_응답이_온_순서로_담는다() {
        let mut store = TrafficStore::default();
        let mut map = EndpointMap::default();
        store.upsert(fetch("a", "https://a.test/x"), 1);
        store.upsert(fetch("b", "https://a.test/x"), 2);
        map.apply(&store);
        // The later request is answered first (bun: statuses [404, 200]).
        for (id, code) in [("b", 404), ("a", 200), ("a", 200)] {
            store.upsert(status(id, code), 3);
            map.apply(&store);
        }
        assert_eq!(map.sorted()[0].statuses, [404, 200]);
        assert_eq!(map.sorted()[0].count, 2);
    }

    #[test]
    fn 캡처를_비우면_엔드포인트도_비운다() {
        let mut store = TrafficStore::default();
        let mut map = EndpointMap::default();
        store.upsert(fetch("a", "https://a.test/x"), 1);
        map.apply(&store);
        store.clear();
        map.apply(&store);
        assert!(map.sorted().is_empty());

        // Also when the counted requests had scrolled out before the clear, and when
        // new traffic arrived before the map looked again.
        store.upsert(fetch("b", "https://a.test/y"), 2);
        map.apply(&store);
        for n in 0..MAX_ENTRIES as u64 {
            let document = RequestPatch {
                resource_type: Some("Document".into()),
                ..fetch(&format!("d{n}"), "https://a.test/")
            };
            store.upsert(document, 3 + n);
        }
        map.apply(&store);
        assert_eq!(keys(&map.sorted()), ["GET https://a.test/y"]);
        store.clear();
        store.upsert(fetch("c", "https://a.test/z"), 9_000);
        map.apply(&store);
        assert_eq!(keys(&map.sorted()), ["GET https://a.test/z"]);
    }

    #[test]
    fn 나중에_xhr로_밝혀진_요청도_한_번_센다() {
        // The capture store names a request `Other` until its type is known.
        let mut store = TrafficStore::default();
        let mut map = EndpointMap::default();
        let untyped = RequestPatch {
            resource_type: None,
            ..fetch("a", "https://a.test/x")
        };
        store.upsert(untyped, 1);
        map.apply(&store);
        assert!(map.sorted().is_empty());
        store.upsert(fetch("a", "https://a.test/x"), 2);
        map.apply(&store);
        map.apply(&store);
        assert_eq!(map.sorted().len(), 1);
        assert_eq!(map.sorted()[0].count, 1);
    }

    // ── domains and hosts ───────────────────────────────────────────────────

    fn sample() -> Vec<ApiEndpoint> {
        vec![
            endpoint("GET", "https://api.example.com", "/products", 9),
            endpoint("POST", "https://api.example.com", "/login", 4),
            endpoint("GET", "https://mail.naver.co.kr", "/list", 3),
            endpoint("GET", "https://www.example.com", "/config", 2),
            endpoint("GET", "http://localhost:8777", "/fixtures", 2),
            endpoint("GET", "https://api.example.com", "/orders", 1),
        ]
    }

    #[test]
    fn 도메인은_엔드포인트가_많은_순서이고_같으면_먼저_나온_순서다() {
        assert_eq!(
            domains(&sample()),
            [
                ("example.com".to_owned(), 4),
                ("naver.co.kr".to_owned(), 1),
                ("localhost".to_owned(), 1),
            ]
        );
        assert!(domains(&[]).is_empty());
    }

    #[test]
    fn 고른_도메인이_남아_있으면_그것을_아니면_첫_도메인을_쓴다() {
        let all = domains(&sample());
        assert_eq!(pick_domain(Some("localhost"), &all), Some("localhost"));
        assert_eq!(pick_domain(None, &all), Some("example.com"));
        // Cleared traffic took the picked domain away.
        assert_eq!(pick_domain(Some("gone.test"), &all), Some("example.com"));
        assert_eq!(pick_domain(Some(""), &all), Some("example.com"));
        assert_eq!(pick_domain(Some("localhost"), &[]), None);
        assert_eq!(pick_domain(None, &[]), None);
    }

    #[test]
    fn 호스트는_도메인_안에서_엔드포인트가_많은_순서로_묶는다() {
        let endpoints = sample();
        let groups = hosts(&endpoints, "example.com");
        let summary: Vec<(&str, usize, usize)> = groups
            .iter()
            .map(|group| (group.host.as_str(), group.eps.len(), group.hits))
            .collect();
        assert_eq!(
            summary,
            [("api.example.com", 3, 14), ("www.example.com", 1, 2)]
        );
        // Inside a host the endpoints keep the list's order: most-called first.
        let paths: Vec<&str> = groups[0].eps.iter().map(|ep| ep.path.as_str()).collect();
        assert_eq!(paths, ["/products", "/login", "/orders"]);
        assert!(hosts(&endpoints, "nowhere.test").is_empty());
    }

    #[test]
    fn 이름이_빈_도메인에는_호스트가_없다() {
        // An origin with nothing after the scheme has the empty host and domain.
        let endpoints = vec![endpoint("GET", "https://", "/x", 1)];
        assert_eq!(domains(&endpoints), [(String::new(), 1)]);
        let groups = hosts(&endpoints, "");
        assert!(groups.is_empty());
        assert_eq!(pick_host(None, &groups), None);
        assert!(nodes("", None, &groups, &Tokens::light()).is_empty());
    }

    #[test]
    fn 호스트_순서는_요청_수가_아니라_엔드포인트_수를_따른다() {
        let endpoints = vec![
            endpoint("GET", "https://busy.a.test", "/one", 100),
            endpoint("GET", "https://wide.a.test", "/x", 1),
            endpoint("GET", "https://wide.a.test", "/y", 1),
            endpoint("GET", "https://late.a.test", "/z", 1),
        ];
        let order: Vec<String> = hosts(&endpoints, "a.test")
            .into_iter()
            .map(|group| group.host)
            .collect();
        // `busy` and `late` tie on one endpoint and keep their first-seen order.
        assert_eq!(order, ["wide.a.test", "busy.a.test", "late.a.test"]);
    }

    #[test]
    fn 포커스한_호스트가_있으면_그_호스트를_연다() {
        let endpoints = sample();
        let groups = hosts(&endpoints, "example.com");
        assert_eq!(
            pick_host(Some("www.example.com"), &groups).as_deref(),
            Some("www.example.com")
        );
        assert_eq!(pick_host(None, &groups), None);
        // A host of another domain is not in this ring.
        assert_eq!(pick_host(Some("mail.naver.co.kr"), &groups), None);
        assert_eq!(pick_host(Some(""), &groups), None);
        assert_eq!(pick_host(Some("x"), &[]), None);
    }

    #[test]
    fn 호스트가_하나뿐인_도메인은_바로_엔드포인트를_보여준다() {
        let endpoints = sample();
        let groups = hosts(&endpoints, "localhost");
        assert_eq!(pick_host(None, &groups).as_deref(), Some("localhost"));
        assert_eq!(
            pick_host(Some("gone"), &groups).as_deref(),
            Some("localhost")
        );
    }

    // ── nodes ───────────────────────────────────────────────────────────────

    #[test]
    fn 호스트_노드는_엔드포인트_수와_서브_라벨을_보여준다() {
        let t = Tokens::light();
        let mut endpoints = sample();
        endpoints[1].auth = true;
        let groups = hosts(&endpoints, "example.com");
        let ring = nodes("example.com", None, &groups, &t);
        assert_eq!(
            ring,
            vec![
                MapNode {
                    key: "api.example.com".into(),
                    badge: "3".into(),
                    color: t.accent_text,
                    label: "api".into(),
                    meta: "3 endpoints · 14 reqs".into(),
                    auth: true,
                    title: "api.example.com".into(),
                    action: NodeAction::Focus("api.example.com".into()),
                },
                MapNode {
                    key: "www.example.com".into(),
                    badge: "1".into(),
                    color: t.accent_text,
                    label: "www".into(),
                    // The component does not inflect the nouns.
                    meta: "1 endpoints · 2 reqs".into(),
                    auth: false,
                    title: "www.example.com".into(),
                    action: NodeAction::Focus("www.example.com".into()),
                },
            ]
        );
    }

    #[test]
    fn 엔드포인트_노드는_메서드와_경로와_횟수를_보여준다() {
        let t = Tokens::dark();
        let mut endpoints = vec![
            endpoint("DELETE", "https://api.a.test", "/v1/cart/items/:id", 2),
            endpoint(
                "GET",
                "https://api.a.test",
                "/v1/recommendations/personalized/for-you",
                1,
            ),
        ];
        endpoints[0].statuses = vec![204, 404];
        endpoints[0].auth = true;
        let groups = hosts(&endpoints, "a.test");
        let ring = nodes("a.test", Some("api.a.test"), &groups, &t);
        assert_eq!(
            ring,
            vec![
                MapNode {
                    key: "DELETE https://api.a.test/v1/cart/items/:id".into(),
                    badge: "DELE".into(),
                    color: rgb(0xe5484d),
                    label: "/v1/cart/items/:id".into(),
                    meta: "×2 · 204/404".into(),
                    auth: true,
                    title: "DELETE https://api.a.test/v1/cart/items/:id".into(),
                    action: NodeAction::OpenRequest("last:DELETE /v1/cart/items/:id".into()),
                },
                MapNode {
                    key: "GET https://api.a.test/v1/recommendations/personalized/for-you".into(),
                    badge: "GET".into(),
                    color: rgb(0x1f9d55),
                    label: "…ions/personalized/for-you".into(),
                    // No response yet: the count stands alone.
                    meta: "×1".into(),
                    auth: false,
                    title: "GET https://api.a.test/v1/recommendations/personalized/for-you".into(),
                    action: NodeAction::OpenRequest(
                        "last:GET /v1/recommendations/personalized/for-you".into()
                    ),
                },
            ]
        );
    }

    #[test]
    fn 엔드포인트는_스무_개_호스트는_열여섯_개까지만_그린다() {
        let t = Tokens::light();
        let endpoints: Vec<ApiEndpoint> = (0..25)
            .map(|n| endpoint("GET", "https://api.a.test", &format!("/p{n}"), 30 - n))
            .collect();
        let groups = hosts(&endpoints, "a.test");
        let ring = nodes("a.test", Some("api.a.test"), &groups, &t);
        assert_eq!(ring.len(), MAX_ENDPOINT_NODES);
        assert_eq!(ring.last().map(|node| node.label.as_str()), Some("/p19"));

        let endpoints: Vec<ApiEndpoint> = (0..18)
            .map(|n| endpoint("GET", &format!("https://h{n}.a.test"), "/", 1))
            .collect();
        let groups = hosts(&endpoints, "a.test");
        assert_eq!(groups.len(), 18);
        let ring = nodes("a.test", None, &groups, &t);
        assert_eq!(ring.len(), MAX_HOST_NODES);
        assert_eq!(ring.last().map(|node| node.label.as_str()), Some("h15"));
    }

    #[test]
    fn 열린_호스트가_목록에_없으면_노드가_없다() {
        let endpoints = sample();
        let groups = hosts(&endpoints, "example.com");
        assert!(
            nodes(
                "example.com",
                Some("gone.example.com"),
                &groups,
                &Tokens::light()
            )
            .is_empty()
        );
        assert!(nodes("example.com", None, &[], &Tokens::light()).is_empty());
    }

    #[test]
    fn 긴_메서드는_네_글자만_배지에_쓴다() {
        let endpoints = vec![endpoint("PROPFIND", "https://dav.a.test", "/", 1)];
        let groups = hosts(&endpoints, "a.test");
        let ring = nodes("a.test", Some("dav.a.test"), &groups, &Tokens::light());
        assert_eq!(ring[0].badge, "PROP");
        assert_eq!(ring[0].color, Theme::light().text_dim);
    }

    #[test]
    fn 요약_문구는_호스트를_열었을_때와_아닐_때가_다르다() {
        let endpoints = sample();
        assert_eq!(
            status_text(Some("api.example.com"), 3, 2, &endpoints, "example.com"),
            "api.example.com · 3 endpoints"
        );
        assert_eq!(
            status_text(None, 2, 2, &endpoints, "example.com"),
            "2 subdomains · 4 endpoints"
        );
        assert_eq!(
            status_text(None, 1, 1, &endpoints, "naver.co.kr"),
            "1 subdomains · 1 endpoints"
        );
    }

    // ── geometry ────────────────────────────────────────────────────────────

    fn radius_of(spot: &Placed) -> f64 {
        ((spot.x - CX).powi(2) + (spot.y - CY).powi(2))
            .sqrt()
            .round()
    }

    #[test]
    fn 노드가_여섯_개까지는_반지름_130_열_개까지는_155다() {
        for total in 1..=6 {
            assert!((0..total).all(|index| radius_of(&place(index, total)) == 130.0));
        }
        for total in 7..=10 {
            assert!((0..total).all(|index| radius_of(&place(index, total)) == 155.0));
        }
    }

    #[test]
    fn 열_개가_넘으면_두_고리에_번갈아_놓는다() {
        assert!(!two_rings(10));
        assert!(two_rings(11));
        let radii: Vec<f64> = (0..11).map(|index| radius_of(&place(index, 11))).collect();
        assert_eq!(
            radii,
            [
                175.0, 112.0, 175.0, 112.0, 175.0, 112.0, 175.0, 112.0, 175.0, 112.0, 175.0
            ]
        );
    }

    #[test]
    fn 첫_노드는_열두_시에_있고_시계_방향으로_돈다() {
        let top = place(0, 4);
        assert!((top.x - CX).abs() < 1e-9);
        assert!((top.y - (CY - 130.0)).abs() < 1e-9);
        let right = place(1, 4);
        assert!((right.x - (CX + 130.0)).abs() < 1e-9);
        assert!((right.y - CY).abs() < 1e-9);
        let bottom = place(2, 4);
        assert!((bottom.y - (CY + 130.0)).abs() < 1e-9);
        let left = place(3, 4);
        assert!((left.x - (CX - 130.0)).abs() < 1e-9);
    }

    #[test]
    fn 라벨은_위아래_노드에서는_가운데_양옆에서는_바깥쪽으로_맞춘다() {
        let anchors: Vec<Anchor> = (0..4).map(|index| place(index, 4).anchor()).collect();
        assert_eq!(
            anchors,
            [Anchor::Middle, Anchor::Start, Anchor::Middle, Anchor::End]
        );
        // |cos| below 0.25 is still "beside the axis".
        let near_top = Placed {
            x: 0.0,
            y: 0.0,
            cos: 0.2499,
        };
        assert_eq!(near_top.anchor(), Anchor::Middle);
        let edge = Placed {
            x: 0.0,
            y: 0.0,
            cos: 0.25,
        };
        assert_eq!(edge.anchor(), Anchor::Start);
        let other_edge = Placed {
            x: 0.0,
            y: 0.0,
            cos: -0.25,
        };
        assert_eq!(other_edge.anchor(), Anchor::End);
    }

    #[test]
    fn 라벨_자리는_원을_피해_잡는다() {
        let above = Placed {
            x: 400.0,
            y: 80.0,
            cos: 0.0,
        };
        // The label and its meta line both go above the circle.
        assert_eq!(above.label_origin(), (400.0, 40.0));
        let below = Placed {
            x: 400.0,
            y: 340.0,
            cos: 0.0,
        };
        assert_eq!(below.label_origin(), (400.0, 374.0));
        // Level with the centre counts as below.
        let level = Placed {
            x: 400.0,
            y: CY,
            cos: 0.1,
        };
        assert_eq!(level.label_origin(), (400.0, CY + 34.0));
        let right = Placed {
            x: 530.0,
            y: 210.0,
            cos: 1.0,
        };
        assert_eq!(right.label_origin(), (556.0, 214.0));
        let left = Placed {
            x: 270.0,
            y: 210.0,
            cos: -1.0,
        };
        assert_eq!(left.label_origin(), (244.0, 214.0));
    }

    #[test]
    fn 뷰박스는_비율을_지키며_가운데에_맞춘다() {
        // Exactly the viewBox.
        assert_eq!(fit(800.0, 420.0), (1.0, 0.0, 0.0));
        // Wider than the viewBox: the height limits, and the map is centered across.
        assert_eq!(fit(1200.0, 420.0), (1.0, 200.0, 0.0));
        // Taller: the width limits.
        assert_eq!(fit(400.0, 420.0), (0.5, 0.0, 105.0));
        assert_eq!(fit(1600.0, 840.0), (2.0, 0.0, 0.0));
        // A collapsed panel draws nothing.
        assert_eq!(fit(800.0, 0.0).0, 0.0);
        assert_eq!(fit(0.0, 0.0), (0.0, 0.0, 0.0));
    }
}

/// The view in a window: clicks, hover and the domain list.
#[cfg(test)]
mod view_tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use rever_core::traffic::RequestPatch;

    /// A store with one Fetch request per URL, in order; ids are `r0`, `r1`, …
    fn store_of(urls: &[&str]) -> Arc<Mutex<TrafficStore>> {
        let mut store = TrafficStore::default();
        for (index, url) in urls.iter().enumerate() {
            let patch = RequestPatch {
                request_id: format!("r{index}"),
                url: Some((*url).into()),
                method: Some("GET".into()),
                resource_type: Some("Fetch".into()),
                status: Some(200),
                ..RequestPatch::default()
            };
            store.upsert(patch, 1_000 + index as u64);
        }
        Arc::new(Mutex::new(store))
    }

    fn open<'a>(
        store: &Arc<Mutex<TrafficStore>>,
        cx: &'a mut TestAppContext,
    ) -> (Entity<ApiMapPanel>, &'a mut VisualTestContext) {
        let store = store.clone();
        let (panel, cx) = cx
            .add_window_view(move |_, cx| ApiMapPanel::new(store, Lang::En, ThemeMode::Light, cx));
        cx.run_until_parked();
        (panel, cx)
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"));
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    /// Two hosts under example.com (api with two endpoints, www with one), and one
    /// endpoint on other.test.
    const TWO_DOMAINS: [&str; 4] = [
        "https://api.example.com/users",
        "https://api.example.com/orders",
        "https://www.example.com/config",
        "https://other.test/ping",
    ];

    #[gpui::test]
    fn 트래픽이_없으면_안내_문구만_보여준다(cx: &mut TestAppContext) {
        let store = store_of(&[]);
        let (_, cx) = open(&store, cx);
        assert!(cx.debug_bounds("apimap-empty").is_some());
        assert!(cx.debug_bounds("apimap-domain").is_none());
        assert!(cx.debug_bounds("apimap-node-0").is_none());
    }

    #[gpui::test]
    fn 호스트_노드를_누르면_그_호스트를_열고_버튼으로_돌아온다(
        cx: &mut TestAppContext,
    ) {
        let store = store_of(&TWO_DOMAINS);
        let (panel, cx) = open(&store, cx);
        let mut opened = cx.events::<ApiMapEvent, _>(&panel);
        // Outside a host there is nothing to go back to.
        assert!(cx.debug_bounds("apimap-all-hosts").is_none());

        // Node 0 is the host with the most endpoints.
        click("apimap-node-0", cx);
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.focus.as_deref(), Some("api.example.com"));
        });
        // Opening a host is the panel's own business.
        assert!(opened.try_recv().is_err());

        click("apimap-all-hosts", cx);
        panel.read_with(cx, |panel, _| assert_eq!(panel.focus, None));
    }

    #[gpui::test]
    fn 엔드포인트_노드를_누르면_마지막_요청을_열어_달라고_알린다(
        cx: &mut TestAppContext,
    ) {
        // One host: its endpoints are shown straight away.
        let store = store_of(&["https://solo.test/items/1", "https://solo.test/items/2"]);
        let (panel, cx) = open(&store, cx);
        let mut opened = cx.events::<ApiMapEvent, _>(&panel);
        assert!(cx.debug_bounds("apimap-node-1").is_none());
        click("apimap-node-0", cx);
        // Both requests are /items/:id; the later one is the endpoint's last.
        assert_eq!(
            opened.try_recv().ok(),
            Some(ApiMapEvent::OpenRequest("r1".into()))
        );
        panel.read_with(cx, |panel, _| assert_eq!(panel.focus, None));
    }

    #[gpui::test]
    fn 버퍼에서_밀려난_요청은_열어_달라고_하지_않는다(cx: &mut TestAppContext) {
        let store = store_of(&["https://solo.test/items"]);
        let (panel, cx) = open(&store, cx);
        let mut opened = cx.events::<ApiMapEvent, _>(&panel);
        // The request leaves the store while the map still shows its endpoint.
        store.lock().unwrap().clear();
        click("apimap-node-0", cx);
        assert!(opened.try_recv().is_err());
    }

    #[gpui::test]
    fn 요청이_버퍼에서_밀려나도_노드는_남고_눌러도_열지_않는다(
        cx: &mut TestAppContext,
    ) {
        let store = store_of(&["https://solo.test/items"]);
        let (panel, cx) = open(&store, cx);
        let mut opened = cx.events::<ApiMapEvent, _>(&panel);
        for n in 0..MAX_ENTRIES {
            let patch = RequestPatch {
                request_id: format!("d{n}"),
                url: Some("https://solo.test/".into()),
                method: Some("GET".into()),
                resource_type: Some("Document".into()),
                ..RequestPatch::default()
            };
            store.lock().unwrap().upsert(patch, 2_000);
        }
        assert!(store.lock().unwrap().get("r0").is_none());
        panel.update(cx, |panel, cx| panel.refresh(cx));
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| assert_eq!(panel.endpoints.len(), 1));
        click("apimap-node-0", cx);
        assert!(opened.try_recv().is_err());
    }

    #[gpui::test]
    fn 목록에서_도메인을_고르면_그_도메인으로_바꾸고_열린_호스트를_닫는다(
        cx: &mut TestAppContext,
    ) {
        let store = store_of(&TWO_DOMAINS);
        let (panel, cx) = open(&store, cx);
        click("apimap-node-0", cx);
        // The list is not there until the select is clicked.
        click("apimap-domain", cx);
        panel.read_with(cx, |panel, _| assert!(panel.menu_open));

        // Choosing the domain already shown changes nothing, as with a <select>.
        click("apimap-domain-option-0", cx);
        panel.read_with(cx, |panel, _| {
            assert!(!panel.menu_open);
            assert_eq!(panel.picked, None);
            assert_eq!(panel.focus.as_deref(), Some("api.example.com"));
        });

        click("apimap-domain", cx);
        click("apimap-domain-option-1", cx);
        panel.read_with(cx, |panel, _| {
            assert!(!panel.menu_open);
            assert_eq!(panel.picked.as_deref(), Some("other.test"));
            assert_eq!(panel.focus, None);
            assert_eq!(panel.current_domain().as_deref(), Some("other.test"));
        });
    }

    #[gpui::test]
    fn 목록_밖을_누르면_목록만_닫는다(cx: &mut TestAppContext) {
        let store = store_of(&TWO_DOMAINS);
        let (panel, cx) = open(&store, cx);
        click("apimap-domain", cx);
        // The click lands on a host node, but the open list swallows it.
        click("apimap-node-0", cx);
        panel.read_with(cx, |panel, _| {
            assert!(!panel.menu_open);
            assert_eq!(panel.focus, None);
        });
    }

    #[gpui::test]
    fn 노드에_포인터를_올리면_그_노드만_남기고_흐리게_한다(
        cx: &mut TestAppContext,
    ) {
        let store = store_of(&TWO_DOMAINS);
        let (panel, cx) = open(&store, cx);
        let node = cx.debug_bounds("apimap-node-1").expect("the second host");
        cx.simulate_mouse_move(node.center(), None, Modifiers::none());
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.hover.as_deref(), Some("www.example.com"));
        });

        let toolbar = cx.debug_bounds("apimap-legend").expect("the toolbar");
        cx.simulate_mouse_move(toolbar.center(), None, Modifiers::none());
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| assert_eq!(panel.hover, None));
    }

    #[gpui::test]
    fn 다른_노드의_떠남_알림은_지금_노드의_강조를_지우지_않는다(
        cx: &mut TestAppContext,
    ) {
        let store = store_of(&TWO_DOMAINS);
        let (panel, cx) = open(&store, cx);
        panel.update(cx, |panel, cx| {
            panel.set_hover("b", true, cx);
            panel.set_hover("a", false, cx);
            assert_eq!(panel.hover.as_deref(), Some("b"));
            panel.set_hover("b", false, cx);
            assert_eq!(panel.hover, None);
        });
    }

    #[gpui::test]
    fn 새_트래픽은_refresh를_불러야_지도에_나타난다(cx: &mut TestAppContext) {
        let store = store_of(&["https://solo.test/items"]);
        let (panel, cx) = open(&store, cx);
        let patch = RequestPatch {
            request_id: "late".into(),
            url: Some("https://solo.test/orders".into()),
            method: Some("POST".into()),
            resource_type: Some("XHR".into()),
            ..RequestPatch::default()
        };
        store.lock().unwrap().upsert(patch, 9_000);
        panel.read_with(cx, |panel, _| assert_eq!(panel.endpoints.len(), 1));
        panel.update(cx, |panel, cx| panel.refresh(cx));
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| assert_eq!(panel.endpoints.len(), 2));
        assert!(cx.debug_bounds("apimap-node-1").is_some());
    }
}
