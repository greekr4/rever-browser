//! Port of `src/renderer/src/components/panels/CookiesPanel.tsx` — the storage panel: the active tab's cookies, localStorage and sessionStorage, with the cookie settings above the cookie table.
//!
//! Everything the component asked `window.rev` for goes through [`StorageHost`]. The
//! decisions (labels, the parameters of each call, entry order) are plain functions;
//! [`CookiesPanel`] lays their results out as the component does.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use chrono::{Local, TimeZone, Timelike};
use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, Context, Div, Entity, Focusable, FontWeight, Hsla,
    KeyDownEvent, MouseButton, MouseDownEvent, PromptButton, PromptLevel, Rgba, SharedString,
    Stateful, Subscription, Task, Window, anchored, deferred, div, point, prelude::*, px, relative,
    rgba,
};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::text_input::{Submitted, TextInput};
use rever_ui_kit::theme::ThemeMode;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::ui::{MONO, Role, Tokens, button, check_mark, clickable, tooltip};

// Inline English in the component (not dictionary keys), kept as it is there.
const STICKY_LABEL: &str = "Sticky session cookies";
const STICKY_HINT: &str = "Session-only cookies persist for 30 days after app exit";
const SNAPSHOT_NOW: &str = "Snapshot now";
const SNAPSHOT_TITLE: &str = "Dump session cookies to disk immediately";
const IMPORT_TITLE: &str =
    "Decrypt and inject into the active tab's session (may ask for Keychain access)";
const INVALID_DATE: &str = "Invalid Date";

/// The whole panel re-reads its data this often while it is open.
pub const REFRESH_MS: u64 = 3000;
/// Characters of a cookie value shown in the table.
pub const COOKIE_VALUE_LIMIT: usize = 80;
/// Characters of a storage value shown in the table.
pub const STORAGE_VALUE_LIMIT: usize = 120;

// ── What crosses to the host ────────────────────────────────────────────────

/// A cookie as `storage.cookies()` lists it (a CDP `Network.Cookie`; fields the panel
/// does not show are ignored).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CookieRow {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    /// Seconds since the epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
}

/// What `storage.cookies()` resolves to.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CookieList {
    pub cookies: Vec<CookieRow>,
    /// The active page's `location.origin`; `None` without an active tab.
    pub origin: Option<String>,
}

/// The argument of `storage.cookieSet`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CookieSetParams {
    pub name: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
}

/// The argument of `storage.cookieDelete`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CookieDeleteParams {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// What `storage.persistenceGet()` resolves to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Persistence {
    pub enabled: bool,
    pub snapshot_count: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserProfileInfo {
    pub id: String,
    pub name: String,
}

/// A browser cookies can be imported from. `id` is the preload's `BrowserId` (chrome,
/// edge, brave, arc, chromium, vivaldi, firefox, safari); the panel only passes it back.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserInfo {
    pub id: String,
    pub name: String,
    pub profiles: Vec<BrowserProfileInfo>,
}

/// The argument of `storage.browserImport`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserImportOptions {
    pub browser: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared: Option<bool>,
}

/// What `storage.browserImport` resolves to.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserImportResult {
    pub ok: bool,
    pub imported: u64,
    pub skipped: u64,
    pub undecryptable: u64,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A page dialog the app answered.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DialogRecord {
    /// Milliseconds since the epoch.
    pub ts: f64,
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
    pub url: String,
}

/// What `dialog.getSettings()` resolves to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DialogSettings {
    pub auto_dismiss: bool,
    pub history: Vec<DialogRecord>,
}

impl Default for DialogSettings {
    /// The component's initial state, shown until the first refresh answers.
    fn default() -> Self {
        Self {
            auto_dismiss: true,
            history: Vec::new(),
        }
    }
}

/// Which of the page's two storages a call is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageArea {
    Local,
    Session,
}

impl StorageArea {
    /// The storage's JavaScript name.
    pub fn name(self) -> &'static str {
        match self {
            StorageArea::Local => "localStorage",
            StorageArea::Session => "sessionStorage",
        }
    }
}

/// A call to the host: the promise of the Electron app. `Err` is the rejected promise;
/// what the component did after that `await` then does not happen.
pub type Call<T> = Task<Result<T, String>>;

/// What the component asked `window.rev` for, one method per call. The four pairs
/// `storage.local*` / `storage.session*` are one method each, told apart by the
/// [`StorageArea`].
pub trait StorageHost {
    /// `storage.cookies()`: the cookies of the active page's origin.
    fn cookies(&self, cx: &mut App) -> Call<CookieList>;
    /// `storage.cookieSet(params)`; resolves to its `success`.
    fn cookie_set(&self, params: CookieSetParams, cx: &mut App) -> Call<bool>;
    /// `storage.cookieDelete(params)`.
    fn cookie_delete(&self, params: CookieDeleteParams, cx: &mut App) -> Call<bool>;
    /// `storage.localGet()` / `sessionGet()`: the entries, in any order (the panel
    /// orders them as a JavaScript object would, see [`record_entries`]).
    fn storage_get(&self, area: StorageArea, cx: &mut App) -> Call<Vec<(String, String)>>;
    /// `storage.localSet(key, value)` / `sessionSet`.
    fn storage_set(
        &self,
        area: StorageArea,
        key: String,
        value: String,
        cx: &mut App,
    ) -> Call<bool>;
    /// `storage.localDelete(key)` / `sessionDelete`.
    fn storage_delete(&self, area: StorageArea, key: String, cx: &mut App) -> Call<bool>;
    /// `storage.localClear()` / `sessionClear`.
    fn storage_clear(&self, area: StorageArea, cx: &mut App) -> Call<bool>;
    /// `storage.persistenceGet()`.
    fn persistence_get(&self, cx: &mut App) -> Call<Persistence>;
    /// `storage.persistenceSet(enabled)`; resolves to the `enabled` now in effect.
    fn persistence_set(&self, enabled: bool, cx: &mut App) -> Call<bool>;
    /// `storage.persistenceSnapshot()`; resolves to the new `snapshotCount`.
    fn persistence_snapshot(&self, cx: &mut App) -> Call<u64>;
    /// `storage.browsers()`: installed browsers with importable profiles.
    fn browsers(&self, cx: &mut App) -> Call<Vec<BrowserInfo>>;
    /// `storage.browserImport(options)`.
    fn browser_import(
        &self,
        options: BrowserImportOptions,
        cx: &mut App,
    ) -> Call<BrowserImportResult>;
    /// `downloads.getSettings()`; resolves to its `askWhereToSave`.
    fn download_ask(&self, cx: &mut App) -> Call<bool>;
    /// `downloads.setAsk(enabled)`; resolves to the `askWhereToSave` now in effect.
    fn set_download_ask(&self, enabled: bool, cx: &mut App) -> Call<bool>;
    /// `dialog.getSettings()`.
    fn dialog_settings(&self, cx: &mut App) -> Call<DialogSettings>;
    /// `dialog.setAutoDismiss(enabled)`; resolves to the `autoDismiss` now in effect.
    fn set_dialog_auto_dismiss(&self, enabled: bool, cx: &mut App) -> Call<bool>;
    /// `dialog.clearHistory()`.
    fn clear_dialog_history(&self, cx: &mut App) -> Call<bool>;
}

// ── Labels and parameters ───────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    Cookies,
    Local,
    Session,
}

impl Section {
    /// The storage a non-cookie section shows.
    pub fn area(self) -> Option<StorageArea> {
        match self {
            Section::Cookies => None,
            Section::Local => Some(StorageArea::Local),
            Section::Session => Some(StorageArea::Session),
        }
    }

    fn tab_key(self) -> &'static str {
        match self {
            Section::Cookies => "cookies.tab.cookies",
            Section::Local => "cookies.tab.local",
            Section::Session => "cookies.tab.session",
        }
    }

    fn tab_id(self) -> &'static str {
        match self {
            Section::Cookies => "storage-tab-cookies",
            Section::Local => "storage-tab-local",
            Section::Session => "storage-tab-session",
        }
    }
}

/// `s.length > n ? s.slice(0, n) + '…' : s`, counting UTF-16 units as JavaScript does.
/// A character the cut would split in half is left out whole.
pub fn truncate(text: &str, limit: usize) -> String {
    if text.encode_utf16().count() <= limit {
        return text.to_string();
    }
    let mut units = 0;
    let mut cut: String = text
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= limit
        })
        .collect();
    cut.push('…');
    cut
}

/// `white-space: normal` lays line breaks and tabs out as spaces. GPUI would start a
/// new line, and its single-line shaper rejects a line break outright.
fn flat(text: &str) -> String {
    text.replace(['\n', '\r', '\t'], " ")
}

/// What an `<input>` keeps of a value: line breaks are stripped.
fn input_value(text: &str) -> String {
    text.replace(['\n', '\r'], "")
}

/// Days since 1970-01-01 to (year, month, day) in the proleptic Gregorian calendar.
/// By hand because a JavaScript date reaches year ±275760, past what chrono holds.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

/// The expires column: `session` for a cookie without an expiry, else the UTC minute,
/// `new Date(expires * 1000).toISOString().slice(0, 16).replace('T', ' ')`.
///
/// "Without an expiry" is JavaScript truthiness: missing, 0 or NaN. CDP reports a
/// session cookie as -1, which is truthy, so such a cookie reads `1969-12-31 23:59`
/// here exactly as in the Electron app.
pub fn expires_label(expires: Option<f64>) -> String {
    let Some(seconds) = expires.filter(|seconds| *seconds != 0.0 && !seconds.is_nan()) else {
        return "session".to_string();
    };
    let ms = seconds * 1000.0;
    // Outside what a Date holds, toISOString throws and takes the component down; a
    // label is kinder.
    if !ms.is_finite() || ms.abs() > 8.64e15 {
        return INVALID_DATE.to_string();
    }
    let ms = ms.trunc() as i64;
    let (days, ms_of_day) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    let (year, month, day) = civil_from_days(days);
    let (hour, minute) = (ms_of_day / 3_600_000, ms_of_day / 60_000 % 60);
    let iso = if (0..=9999).contains(&year) {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}")
    } else {
        // toISOString writes such years with a sign and six digits; sixteen characters
        // then end at the hour.
        let sign = if year < 0 { '-' } else { '+' };
        format!(
            "{sign}{:06}-{month:02}-{day:02}T{hour:02}:{minute:02}",
            year.abs()
        )
    };
    iso.get(..16).unwrap_or(&iso).replacen('T', " ", 1)
}

/// The tags of the flags column, in the component's order. Each shows when its field
/// is truthy.
pub fn cookie_flags(cookie: &CookieRow) -> Vec<String> {
    let mut flags = Vec::new();
    if cookie.secure == Some(true) {
        flags.push("Secure".to_string());
    }
    if cookie.http_only == Some(true) {
        flags.push("HttpOnly".to_string());
    }
    if let Some(same_site) = cookie.same_site.as_ref().filter(|s| !s.is_empty()) {
        flags.push(same_site.clone());
    }
    flags
}

/// `value || undefined`.
fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}

/// What the ✕ of a cookie row sends.
pub fn delete_params(cookie: &CookieRow, origin: Option<&str>) -> CookieDeleteParams {
    CookieDeleteParams {
        name: cookie.name.clone(),
        url: origin.map(str::to_string),
        domain: non_empty(&cookie.domain),
        path: non_empty(&cookie.path),
    }
}

/// What the editor's Save sends. `expires` is never among it: a cookie saved from the
/// editor becomes a session cookie, as in the Electron app.
pub fn set_params(cookie: &CookieRow, origin: Option<&str>) -> CookieSetParams {
    CookieSetParams {
        name: cookie.name.clone(),
        value: cookie.value.clone(),
        url: origin.map(str::to_string),
        domain: non_empty(&cookie.domain),
        path: non_empty(&cookie.path),
        secure: cookie.secure.filter(|on| *on),
        http_only: cookie.http_only.filter(|on| *on),
        same_site: cookie.same_site.clone(),
        expires: None,
    }
}

/// The cookie "+ cookie" starts from: the origin's host as its domain. `None` when the
/// origin is not a URL (`about:blank` reports the string "null"): `new URL` throws
/// there and the button does nothing.
pub fn new_cookie(origin: Option<&str>) -> Option<CookieRow> {
    let domain = match origin.filter(|origin| !origin.is_empty()) {
        Some(origin) => Url::parse(origin)
            .ok()?
            .host_str()
            .unwrap_or_default()
            .to_string(),
        None => String::new(),
    };
    Some(CookieRow {
        domain,
        path: "/".to_string(),
        ..CookieRow::default()
    })
}

pub fn empty_cookies_message(origin: Option<&str>) -> String {
    format!("No cookies for {}", origin.unwrap_or("this origin"))
}

pub fn editor_title(is_new: bool, name: &str) -> String {
    if is_new {
        "New cookie".to_string()
    } else {
        format!("Edit {name}")
    }
}

pub fn add_entry_label(area: StorageArea) -> String {
    format!("+ {} entry", area.name())
}

pub fn clear_confirm(area: StorageArea) -> String {
    format!("Clear all {}?", area.name())
}

/// A key JavaScript treats as an array index: canonical digits below 2^32 - 1.
fn array_index(key: &str) -> Option<u32> {
    let canonical = key == "0" || (!key.starts_with('0') && !key.is_empty());
    if !canonical || !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index != u32::MAX)
}

/// The entries in the order `Object.entries` gives for the object the Electron app
/// received: integer-like keys first, ascending, then the others as they came. A key
/// listed twice keeps its first place and its last value, as an object would.
pub fn record_entries(entries: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut places: HashMap<String, usize> = HashMap::new();
    let mut unique: Vec<(String, String)> = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        match places.get(&key).and_then(|place| unique.get_mut(*place)) {
            Some(entry) => entry.1 = value,
            None => {
                places.insert(key.clone(), unique.len());
                unique.push((key, value));
            }
        }
    }
    // Stable, so the non-index keys keep the order they came in.
    unique.sort_by_key(|(key, _)| match array_index(key) {
        Some(index) => (0, index),
        None => (1, 0),
    });
    unique
}

/// `profiles.find(p => p.id === 'Default')?.id ?? profiles[0]?.id ?? ''`
pub fn default_profile_id(profiles: &[BrowserProfileInfo]) -> String {
    profiles
        .iter()
        .find(|profile| profile.id == "Default")
        .or(profiles.first())
        .map(|profile| profile.id.clone())
        .unwrap_or_default()
}

/// `String.prototype.trim`: its whitespace set is not Rust's (U+FEFF counts, U+0085 does not).
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| {
        matches!(
            c,
            '\u{9}'..='\u{d}'
                | ' '
                | '\u{a0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
        )
    })
}

/// The domains field: comma-separated, trimmed, blanks dropped. Empty means all hosts.
pub fn parse_hosts(text: &str) -> Vec<String> {
    text.split(',')
        .map(js_trim)
        .filter(|host| !host.is_empty())
        .map(str::to_string)
        .collect()
}

/// The line under the import bar. `Err` is the rejected call.
pub fn import_result_text(outcome: Result<BrowserImportResult, String>) -> String {
    let result = match outcome {
        Ok(result) => result,
        Err(message) => return format!("⚠ {message}"),
    };
    if !result.ok {
        return format!("⚠ {}", result.error.as_deref().unwrap_or("import failed"));
    }
    let mut text = format!("✓ imported {}/{}", result.imported, result.total);
    if result.undecryptable > 0 {
        text.push_str(&format!(" · {} app-bound skipped", result.undecryptable));
    }
    if result.skipped > 0 {
        text.push_str(&format!(" · {} skipped", result.skipped));
    }
    text
}

/// Whether the import line is drawn as an error.
pub fn import_failed(result: &str) -> bool {
    result.starts_with('⚠')
}

/// What a `<select>` shows: the label of the option with this value, or of the first
/// option when none has it.
pub fn select_label(options: &[(String, String)], value: &str) -> String {
    options
        .iter()
        .find(|(option, _)| option == value)
        .or(options.first())
        .map(|(_, label)| label.clone())
        .unwrap_or_default()
}

/// The SameSite `<select>`: (value, label).
pub fn same_site_options() -> Vec<(String, String)> {
    [
        ("", "(none)"),
        ("Strict", "Strict"),
        ("Lax", "Lax"),
        ("None", "None"),
    ]
    .into_iter()
    .map(|(value, label)| (value.to_string(), label.to_string()))
    .collect()
}

/// The color class of a dialog record's type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogTone {
    Warn,
    Info,
    Magenta,
    Dim,
}

pub fn dialog_tone(kind: &str) -> DialogTone {
    match kind {
        "alert" => DialogTone::Warn,
        "confirm" => DialogTone::Info,
        "prompt" => DialogTone::Magenta,
        _ => DialogTone::Dim,
    }
}

/// `new Date(ts).toLocaleTimeString()` on the clock of `zone`. The Electron app
/// formats by the system locale; here the interface language decides between the
/// en-US and the ko-KR form.
pub fn time_label<Tz: TimeZone>(ts: f64, lang: Lang, zone: &Tz) -> String {
    if !ts.is_finite() || ts.abs() > 8.64e15 {
        return INVALID_DATE.to_string();
    }
    let Some(time) = zone.timestamp_millis_opt(ts.trunc() as i64).single() else {
        return INVALID_DATE.to_string();
    };
    let (afternoon, hour) = time.hour12();
    let clock = format!("{hour}:{:02}:{:02}", time.minute(), time.second());
    match (lang, afternoon) {
        // ICU puts a narrow no-break space before the day period.
        (Lang::En, false) => format!("{clock}\u{202f}AM"),
        (Lang::En, true) => format!("{clock}\u{202f}PM"),
        (Lang::Ko, false) => format!("오전 {clock}"),
        (Lang::Ko, true) => format!("오후 {clock}"),
    }
}

// ── View ────────────────────────────────────────────────────────────────────

/// The value being edited in place in a storage row.
struct Editing {
    key: String,
    input: Entity<TextInput>,
    _submit: Subscription,
    _deactivated: Subscription,
}

/// The two fields of the "add entry" row.
struct Adding {
    key: Entity<TextInput>,
    value: Entity<TextInput>,
}

/// The cookie dialog: the text fields, and the flags the checkboxes and the select hold.
struct CookieEditor {
    is_new: bool,
    /// The name the dialog was opened with: an existing cookie's name is not editable.
    name: String,
    name_input: Entity<TextInput>,
    value_input: Entity<TextInput>,
    domain_input: Entity<TextInput>,
    path_input: Entity<TextInput>,
    secure: Option<bool>,
    http_only: Option<bool>,
    same_site: Option<String>,
}

/// `BrowserImportBar`: its state lives only while the cookies section is shown.
struct ImportBar {
    browsers: Vec<BrowserInfo>,
    browser_id: String,
    profile: String,
    hosts: Entity<TextInput>,
    busy: bool,
    result: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectId {
    Browser,
    Profile,
    SameSite,
}

impl SelectId {
    /// The dictionary key of the select's `title`.
    fn title_key(self) -> Option<&'static str> {
        match self {
            SelectId::Browser => Some("cookies.importBrowserTitle"),
            SelectId::Profile => Some("cookies.importProfileTitle"),
            SelectId::SameSite => None,
        }
    }

    fn element_id(self) -> &'static str {
        match self {
            SelectId::Browser => "import-browser",
            SelectId::Profile => "import-profile",
            SelectId::SameSite => "editor-same-site",
        }
    }
}

pub struct CookiesPanel {
    host: Rc<dyn StorageHost>,
    lang: Lang,
    theme_mode: ThemeMode,
    section: Section,
    cookies: Vec<CookieRow>,
    origin: Option<String>,
    local: Vec<(String, String)>,
    session: Vec<(String, String)>,
    editing: Option<Editing>,
    adding: Option<Adding>,
    editor: Option<CookieEditor>,
    sticky: Persistence,
    ask_where_to_save: bool,
    dialog: DialogSettings,
    import: Option<ImportBar>,
    /// Counts the import bar's mounts, so an answer for a bar that is gone is dropped.
    import_seq: u64,
    open_select: Option<SelectId>,
}

impl CookiesPanel {
    pub fn new(
        host: Rc<dyn StorageHost>,
        lang: Lang,
        theme_mode: ThemeMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut panel = Self {
            host,
            lang,
            theme_mode,
            section: Section::Cookies,
            cookies: Vec::new(),
            origin: None,
            local: Vec::new(),
            session: Vec::new(),
            editing: None,
            adding: None,
            editor: None,
            sticky: Persistence::default(),
            ask_where_to_save: false,
            dialog: DialogSettings::default(),
            import: None,
            import_seq: 0,
            open_select: None,
        };
        let sticky = panel.host.persistence_get(cx);
        panel.after(sticky, cx, |this, sticky, _| this.sticky = sticky);
        let ask = panel.host.download_ask(cx);
        panel.after(ask, cx, |this, ask, _| this.ask_where_to_save = ask);
        panel.mount_import(cx);
        panel.refresh(cx);
        // One timer for the whole panel. It only runs while the panel exists (the host
        // keeps it only while it is open), so nothing polls in the background.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(REFRESH_MS))
                    .await;
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        panel
    }

    pub fn section(&self) -> Section {
        self.section
    }

    /// Run `then` with the call's value once it resolves. A failed call is the rejected
    /// promise: what followed the `await` in the component does not run.
    fn after<T: 'static>(
        &self,
        call: Call<T>,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            if let Ok(value) = call.await {
                let _ = this.update(cx, |this, cx| {
                    then(this, value, cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Re-read cookies, both storages and the dialog settings. All four or nothing, as
    /// with `Promise.all`.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let cookies = self.host.cookies(cx);
        let local = self.host.storage_get(StorageArea::Local, cx);
        let session = self.host.storage_get(StorageArea::Session, cx);
        let dialog = self.host.dialog_settings(cx);
        cx.spawn(async move |this, cx| {
            let (Ok(cookies), Ok(local), Ok(session), Ok(dialog)) =
                (cookies.await, local.await, session.await, dialog.await)
            else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.cookies = cookies.cookies;
                this.origin = cookies.origin;
                this.local = record_entries(local);
                this.session = record_entries(session);
                this.dialog = dialog;
                cx.notify();
            });
        })
        .detach();
    }

    pub fn set_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
        self.section = section;
        self.open_select = None;
        // The import bar is part of the cookies section only: it starts over each time.
        if section == Section::Cookies {
            self.mount_import(cx);
        } else {
            self.import = None;
        }
        cx.notify();
    }

    fn entries(&self, area: StorageArea) -> &[(String, String)] {
        match area {
            StorageArea::Local => &self.local,
            StorageArea::Session => &self.session,
        }
    }

    // ── Cookie settings ─────────────────────────────────────────────────────

    fn toggle_sticky(&mut self, cx: &mut Context<Self>) {
        let call = self.host.persistence_set(!self.sticky.enabled, cx);
        self.after(call, cx, |this, enabled, _| this.sticky.enabled = enabled);
    }

    fn snapshot_now(&mut self, cx: &mut Context<Self>) {
        let call = self.host.persistence_snapshot(cx);
        self.after(call, cx, |this, count, _| {
            this.sticky.snapshot_count = count
        });
    }

    fn toggle_download_ask(&mut self, cx: &mut Context<Self>) {
        let call = self.host.set_download_ask(!self.ask_where_to_save, cx);
        self.after(call, cx, |this, ask, _| this.ask_where_to_save = ask);
    }

    fn toggle_auto_dismiss(&mut self, cx: &mut Context<Self>) {
        let call = self
            .host
            .set_dialog_auto_dismiss(!self.dialog.auto_dismiss, cx);
        self.after(call, cx, |this, on, _| this.dialog.auto_dismiss = on);
    }

    fn clear_dialog_history(&mut self, cx: &mut Context<Self>) {
        let call = self.host.clear_dialog_history(cx);
        self.after(call, cx, |this, _, _| this.dialog.history.clear());
    }

    // ── Browser import ──────────────────────────────────────────────────────

    fn mount_import(&mut self, cx: &mut Context<Self>) {
        self.import_seq += 1;
        let seq = self.import_seq;
        let placeholder = self.lang.t("cookies.domainsPlaceholder");
        self.import = Some(ImportBar {
            browsers: Vec::new(),
            browser_id: String::new(),
            profile: String::new(),
            hosts: cx.new(|cx| TextInput::new(placeholder, cx)),
            busy: false,
            result: None,
        });
        let call = self.host.browsers(cx);
        self.after(call, cx, move |this, browsers, _| {
            let Some(bar) = this.import.as_mut().filter(|_| this.import_seq == seq) else {
                return;
            };
            if let Some(first) = browsers.first() {
                bar.browser_id = first.id.clone();
                bar.profile = default_profile_id(&first.profiles);
            }
            bar.browsers = browsers;
        });
    }

    /// The Import button.
    pub fn run_import(&mut self, cx: &mut Context<Self>) {
        let seq = self.import_seq;
        let Some(bar) = self.import.as_mut() else {
            return;
        };
        if bar.browser_id.is_empty() || bar.busy {
            return;
        }
        bar.busy = true;
        bar.result = None;
        let options = BrowserImportOptions {
            browser: bar.browser_id.clone(),
            profile: Some(bar.profile.clone()),
            hosts: Some(parse_hosts(bar.hosts.read(cx).text())),
            shared: None,
        };
        let call = self.host.browser_import(options, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = call.await;
            let _ = this.update(cx, |this, cx| {
                let Some(bar) = this.import.as_mut().filter(|_| this.import_seq == seq) else {
                    return;
                };
                bar.result = Some(import_result_text(outcome));
                bar.busy = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// Open one of the selects without a click: `browser`, `profile` or `samesite`.
    /// For the preview, which has no pointer.
    #[doc(hidden)]
    pub fn show_select(&mut self, which: &str, cx: &mut Context<Self>) {
        self.open_select = match which {
            "browser" => Some(SelectId::Browser),
            "profile" => Some(SelectId::Profile),
            "samesite" => Some(SelectId::SameSite),
            _ => None,
        };
        cx.notify();
    }

    fn pick(&mut self, select: SelectId, value: &str, cx: &mut Context<Self>) {
        self.open_select = None;
        match select {
            SelectId::Browser => {
                if let Some(bar) = self.import.as_mut() {
                    let profiles = bar
                        .browsers
                        .iter()
                        .find(|browser| browser.id == value)
                        .map(|browser| browser.profiles.as_slice())
                        .unwrap_or_default();
                    bar.profile = default_profile_id(profiles);
                    bar.browser_id = value.to_string();
                }
            }
            SelectId::Profile => {
                if let Some(bar) = self.import.as_mut() {
                    bar.profile = value.to_string();
                }
            }
            SelectId::SameSite => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.same_site = non_empty(value);
                }
            }
        }
        cx.notify();
    }

    // ── Cookies ─────────────────────────────────────────────────────────────

    fn delete_cookie(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(cookie) = self.cookies.get(index) else {
            return;
        };
        let params = delete_params(cookie, self.origin.as_deref());
        let call = self.host.cookie_delete(params, cx);
        self.after(call, cx, |this, _, cx| this.refresh(cx));
    }

    /// The ✎ of a cookie row.
    pub fn edit_cookie(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(cookie) = self.cookies.get(index).cloned() {
            self.open_editor(cookie, false, cx);
        }
    }

    /// The "+ cookie" button.
    pub fn add_cookie(&mut self, cx: &mut Context<Self>) {
        if let Some(cookie) = new_cookie(self.origin.as_deref()) {
            self.open_editor(cookie, true, cx);
        }
    }

    fn open_editor(&mut self, cookie: CookieRow, is_new: bool, cx: &mut Context<Self>) {
        let field = |text: &str, cx: &mut Context<Self>| {
            let text = input_value(text);
            cx.new(|cx| {
                let mut input = TextInput::new("", cx);
                input.set_text(text, cx);
                input
            })
        };
        self.editor = Some(CookieEditor {
            is_new,
            name_input: field(&cookie.name, cx),
            value_input: field(&cookie.value, cx),
            domain_input: field(&cookie.domain, cx),
            path_input: field(&cookie.path, cx),
            name: cookie.name,
            secure: cookie.secure,
            http_only: cookie.http_only,
            same_site: cookie.same_site,
        });
        self.open_select = None;
        cx.notify();
    }

    fn close_editor(&mut self, cx: &mut Context<Self>) {
        self.editor = None;
        self.open_select = None;
        cx.notify();
    }

    fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        let read = |input: &Entity<TextInput>| input.read(cx).text().to_string();
        let cookie = CookieRow {
            name: if editor.is_new {
                read(&editor.name_input)
            } else {
                editor.name.clone()
            },
            value: read(&editor.value_input),
            domain: read(&editor.domain_input),
            path: read(&editor.path_input),
            expires: None,
            secure: editor.secure,
            http_only: editor.http_only,
            same_site: editor.same_site.clone(),
        };
        let params = set_params(&cookie, self.origin.as_deref());
        let call = self.host.cookie_set(params, cx);
        self.after(call, cx, |this, _, cx| {
            this.close_editor(cx);
            this.refresh(cx);
        });
    }

    // ── Storage entries ─────────────────────────────────────────────────────

    /// The ✎ of a storage row, or a double click on its value.
    pub fn start_editing(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(area) = self.section.area() else {
            return;
        };
        let Some((_, value)) = self.entries(area).iter().find(|(entry, _)| entry == key) else {
            return;
        };
        let value = input_value(value);
        let input = cx.new(|cx| {
            let mut input = TextInput::new("", cx);
            input.set_text(value, cx);
            input
        });
        // autoFocus
        window.focus(&input.focus_handle(cx));
        let submit = cx.subscribe(&input, |this, _, _: &Submitted, cx| this.commit_editing(cx));
        // A field also blurs when its window stops being the active one.
        let deactivated = cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                this.commit_editing(cx);
            }
        });
        self.editing = Some(Editing {
            key: key.to_string(),
            input,
            _submit: submit,
            _deactivated: deactivated,
        });
        cx.notify();
    }

    /// The field's `onBlur`: write the value, even an unchanged one, then re-read.
    fn commit_editing(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        cx.notify();
        let Some(area) = self.section.area() else {
            return;
        };
        let value = editing.input.read(cx).text().to_string();
        let call = self.host.storage_set(area, editing.key, value, cx);
        self.after(call, cx, |this, _, cx| this.refresh(cx));
    }

    fn delete_entry(&mut self, key: String, cx: &mut Context<Self>) {
        let Some(area) = self.section.area() else {
            return;
        };
        let call = self.host.storage_delete(area, key, cx);
        self.after(call, cx, |this, _, cx| this.refresh(cx));
    }

    /// The "+ localStorage entry" button.
    pub fn start_adding(&mut self, cx: &mut Context<Self>) {
        self.adding = Some(Adding {
            key: cx.new(|cx| TextInput::new("key", cx)),
            value: cx.new(|cx| TextInput::new("value", cx)),
        });
        cx.notify();
    }

    fn add_entry(&mut self, cx: &mut Context<Self>) {
        let (Some(adding), Some(area)) = (self.adding.as_ref(), self.section.area()) else {
            return;
        };
        let key = adding.key.read(cx).text().to_string();
        if key.is_empty() {
            return;
        }
        let value = adding.value.read(cx).text().to_string();
        let call = self.host.storage_set(area, key, value, cx);
        self.after(call, cx, |this, _, cx| {
            this.adding = None;
            this.refresh(cx);
        });
    }

    /// The "clear all" button: ask first, as the component does with `confirm()`.
    fn request_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(area) = self.section.area() else {
            return;
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &clear_confirm(area),
            None,
            // Spelled out: GPUI's own conversion from "OK" labels the button "Ok".
            &[PromptButton::ok("OK"), PromptButton::cancel("Cancel")],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let Ok(call) = this.update(cx, |this, cx| this.host.storage_clear(area, cx)) else {
                return;
            };
            if call.await.is_ok() {
                let _ = this.update(cx, |this, cx| this.refresh(cx));
            }
        })
        .detach();
    }
}

// ── Layout ──────────────────────────────────────────────────────────────────

/// How a table column takes its width. The component's `<table>` sizes its columns by
/// their content; GPUI has no table layout, so the widths are fixed shares.
#[derive(Clone, Copy)]
enum Col {
    /// A share of the table's width.
    Share(f32),
    /// What the other columns leave.
    Fill,
    Fixed(f32),
}

/// name, value, domain, path, flags (room for the three tags on one line), expires
/// (sixteen characters), the two buttons.
const COOKIE_COLS: [Col; 7] = [
    Col::Share(0.13),
    Col::Fill,
    Col::Share(0.13),
    Col::Share(0.06),
    Col::Fixed(170.0),
    Col::Fixed(122.0),
    Col::Fixed(58.0),
];
/// key, value, the two buttons.
const STORAGE_COLS: [Col; 3] = [Col::Share(0.28), Col::Fill, Col::Fixed(58.0)];

/// A `<td>`: 4px 7px padding, content at the top.
fn cell(col: Col) -> Div {
    let cell = div().min_w_0().px(px(7.0)).py(px(4.0));
    match col {
        Col::Share(share) => cell.flex_none().w(relative(share)),
        Col::Fill => cell.flex_1(),
        Col::Fixed(width) => cell.flex_none().w(px(width)),
    }
}

/// The `<thead>`: it stays above the scrolling rows, as `position: sticky` keeps it.
fn table_head(cols: &[Col], titles: &[&'static str], t: &Tokens) -> Div {
    div()
        .flex()
        .flex_none()
        .bg(t.theme.bg_bar)
        .border_b_1()
        .border_color(t.theme.border)
        .children(cols.iter().zip(titles).map(|(col, title)| {
            cell(*col)
                .py(px(5.0))
                .font_weight(FontWeight::MEDIUM)
                .opacity(0.75)
                .child(*title)
        }))
}

/// `valStyle`: the value as a `<code>` chip that wraps anywhere.
fn value_chip(text: String, t: &Tokens) -> Div {
    div()
        .max_w_full()
        .px(px(4.0))
        .py(px(1.0))
        .rounded(px(2.0))
        .bg(t.theme.bg)
        .child(text)
}

/// The box of a text field: `addInput` (6px padding) and `editorInput` (8px). The
/// field's own line is 20px; the box is the component's 23px.
fn field_box(id: &'static str, pad: f32, t: &Tokens) -> Stateful<Div> {
    clickable(id)
        .h(px(23.0))
        .px(px(pad))
        .flex()
        .items_center()
        .overflow_hidden()
        .bg(t.theme.bg)
        .text_color(t.theme.text)
        .border_1()
        .border_color(t.theme.border_2)
        .rounded(px(3.0))
        .text_size(px(11.0))
        .font_family(MONO)
}

/// A text field in its box. The padding is part of an `<input>`: a press there focuses
/// the field too.
fn input_box(id: &'static str, pad: f32, input: &Entity<TextInput>, t: &Tokens) -> Stateful<Div> {
    let focus = input.clone();
    field_box(id, pad, t)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            window.focus(&focus.focus_handle(cx))
        })
        .child(input.clone())
}

/// The settings bars above the cookie table.
fn bar(tinted: bool, t: &Tokens) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(6.0))
        .mb(px(8.0))
        .border_1()
        .border_color(t.theme.border)
        .rounded(px(4.0))
        // rgba(110, 231, 183, 0.05)
        .bg(if tinted {
            rgba(0x6ee7b70d)
        } else {
            t.theme.bg_bar
        })
        .text_size(px(11.0))
        .line_height(px(13.0))
}

/// A `<label>` around a checkbox and its bold text: a click anywhere on it toggles.
fn toggle_label(
    id: &'static str,
    checked: bool,
    text: impl Into<SharedString>,
    color: Rgba,
    t: &Tokens,
) -> Stateful<Div> {
    clickable(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.0))
        .cursor_pointer()
        .child(check_mark(checked, t))
        .child(
            div()
                .font_weight(FontWeight::BOLD)
                .text_color(color)
                .child(text.into()),
        )
}

/// A hint after a label: it gives way and wraps when the bar is narrow.
fn hint(text: impl Into<SharedString>, opacity: f32) -> Div {
    div().min_w_0().opacity(opacity).child(text.into())
}

impl CookiesPanel {
    fn header(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let theme = t.theme;
        let lang = self.lang;
        let current = self.section;
        let tab = |section: Section, count: usize| {
            let pressed = current == section;
            // `.seg > button`
            clickable(section.tab_id())
                .flex()
                .gap(px(3.0))
                .px(px(6.0))
                .pt(px(3.0))
                .pb(px(1.0))
                .border_b_2()
                .border_color(if pressed {
                    theme.accent
                } else {
                    rgba(0x00000000)
                })
                .line_height(px(16.0))
                .text_color(if pressed { theme.text } else { theme.text_dim })
                .cursor_pointer()
                .hover(move |style| style.text_color(theme.text))
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.set_section(section, cx)),
                )
                .child(lang.t(section.tab_key()))
                .child(div().opacity(0.6).child(count.to_string()))
        };
        let tabs = div()
            .flex()
            .flex_none()
            .gap(px(2.0))
            .child(tab(Section::Cookies, self.cookies.len()))
            .child(tab(Section::Local, self.local.len()))
            .child(tab(Section::Session, self.session.len()));
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .mb(px(10.0))
            .child(tabs)
            .child(div().flex_1())
            .children(self.origin.as_ref().map(|origin| {
                div()
                    .min_w_0()
                    .mr(px(6.0))
                    .opacity(0.55)
                    .truncate()
                    .child(flat(origin))
            }))
            .child(
                button("storage-refresh", "↻", Role::Ghost, true, false, t)
                    .tooltip(tooltip(lang.t("cookies.refresh"), t))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.refresh(cx))),
            )
    }

    fn sticky_bar(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let enabled = self.sticky.enabled;
        let color = if enabled { t.status_ok } else { t.theme.text_2 };
        bar(enabled, t)
            .child(
                toggle_label("sticky-toggle", enabled, STICKY_LABEL, color, t)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_sticky(cx))),
            )
            .child(hint(STICKY_HINT, 0.6))
            .child(div().flex_1())
            .when(self.sticky.snapshot_count > 0, |bar| {
                bar.child(
                    div()
                        .flex_none()
                        .opacity(0.55)
                        .child(format!("{} snapshots saved", self.sticky.snapshot_count)),
                )
            })
            .child(
                button(
                    "sticky-snapshot",
                    SNAPSHOT_NOW,
                    Role::Plain,
                    false,
                    false,
                    t,
                )
                .tooltip(tooltip(SNAPSHOT_TITLE, t))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.snapshot_now(cx))),
            )
    }

    fn download_bar(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        bar(false, t)
            .child(
                toggle_label(
                    "download-ask-toggle",
                    self.ask_where_to_save,
                    self.lang.t("settings.downloadAsk"),
                    t.theme.text_2,
                    t,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_download_ask(cx))),
            )
            .child(hint(self.lang.t("settings.downloadAskHint"), 0.6))
    }

    fn dialog_bar(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let auto = self.dialog.auto_dismiss;
        let color = if auto { t.status_ok } else { t.theme.text_2 };
        let recent = self.dialog.history.len();
        bar(auto, t)
            .child(
                toggle_label(
                    "dialog-auto-toggle",
                    auto,
                    self.lang.t("settings.dialogAuto"),
                    color,
                    t,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_auto_dismiss(cx))),
            )
            .child(hint(self.lang.t("settings.dialogAutoHint"), 0.6))
            .child(div().flex_1())
            .when(recent > 0, |bar| {
                bar.child(
                    div()
                        .flex_none()
                        .opacity(0.55)
                        .child(format!("{recent} recent")),
                )
                .child(
                    button("dialog-clear", "clear", Role::Plain, false, false, t).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.clear_dialog_history(cx)),
                    ),
                )
            })
    }

    /// The answered dialogs, newest first, in a box that scrolls past 110px.
    fn dialog_history(&self, t: &Tokens) -> Option<Stateful<Div>> {
        if self.dialog.history.is_empty() {
            return None;
        }
        let theme = t.theme;
        let rows = self.dialog.history.iter().rev().map(|record| {
            let color = match dialog_tone(&record.kind) {
                DialogTone::Warn => theme.status_warn,
                DialogTone::Info => t.status_info,
                DialogTone::Magenta => t.status_magenta,
                DialogTone::Dim => theme.text_dim,
            };
            div()
                .flex()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(3.0))
                .border_b_1()
                .border_color(theme.border_2)
                .child(
                    div()
                        .flex_none()
                        .min_w(px(60.0))
                        .font_weight(FontWeight::BOLD)
                        .text_color(color)
                        .child(flat(&record.kind.to_uppercase())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(theme.text_2)
                        .child(flat(&record.message)),
                )
                .child(
                    div()
                        .flex_none()
                        .opacity(0.5)
                        .child(time_label(record.ts, self.lang, &Local)),
                )
        });
        Some(
            div()
                .id("dialog-history")
                .flex_none()
                .max_h(px(110.0))
                .overflow_y_scroll()
                .mb(px(8.0))
                .border_1()
                .border_color(theme.border)
                .rounded(px(4.0))
                .bg(theme.bg)
                .font_family(MONO)
                .text_size(px(10.5))
                .line_height(px(12.0))
                .children(rows),
        )
    }

    /// A `<select>`: the box with the current label, and its options while it is open.
    ///
    /// The options of the two import selects float over what is below (`deferred`).
    /// The dialog is itself drawn deferred and GPUI cannot nest that, so the SameSite
    /// options open upward instead, over the fields drawn before them.
    fn select(
        &self,
        select: SelectId,
        value: &str,
        options: &[(String, String)],
        disabled: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = t.theme;
        let open = self.open_select == Some(select) && !disabled;
        let trigger = clickable(select.element_id())
            .flex()
            .items_center()
            .gap(px(4.0))
            .px(px(4.0))
            .py(px(2.0))
            .rounded(px(3.0))
            .border_1()
            .border_color(theme.border_2)
            .bg(theme.bg)
            .text_color(theme.text)
            .whitespace_nowrap()
            .child(flat(&select_label(options, value)))
            .child(
                div()
                    .font_family(MONO)
                    .text_size(px(9.0))
                    .opacity(0.7)
                    .child("▾"),
            )
            .when_some(select.title_key(), |trigger, title| {
                trigger.tooltip(tooltip(self.lang.t(title), t))
            })
            .when(!disabled, |trigger| {
                trigger.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.open_select = (this.open_select != Some(select)).then_some(select);
                    cx.notify();
                }))
            });
        let wrap = div().relative().flex_none().child(trigger);
        if !open {
            return wrap;
        }

        let hover = theme.chrome_hover;
        let rows = options.iter().enumerate().map(|(index, (option, label))| {
            let picked = option.clone();
            clickable((select.element_id(), index))
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(8.0))
                .py(px(3.0))
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.pick(select, &picked, cx)),
                )
                .child(
                    div()
                        .w(px(10.0))
                        .flex_none()
                        .when(option == value, |mark| mark.child("✓")),
                )
                .child(flat(label))
        });
        let upward = select == SelectId::SameSite;
        let menu = div()
            .absolute()
            .left_0()
            .map(|menu| {
                if upward {
                    menu.bottom(relative(1.0)).mb(px(2.0))
                } else {
                    menu.top(relative(1.0)).mt(px(2.0))
                }
            })
            .py(px(4.0))
            .rounded(px(4.0))
            .bg(t.surface)
            .border_1()
            .border_color(theme.border_2)
            .shadow(vec![BoxShadow {
                color: Hsla::from(t.shadow),
                offset: point(px(0.0), px(4.0)),
                blur_radius: px(16.0),
                spread_radius: px(0.0),
            }])
            .text_color(theme.text)
            .whitespace_nowrap()
            .occlude()
            // A click outside closes the options and goes no further.
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Left {
                    this.open_select = None;
                    cx.notify();
                }
                cx.stop_propagation();
            }))
            .children(rows);
        if upward {
            wrap.child(menu)
        } else {
            wrap.child(deferred(menu).with_priority(1))
        }
    }

    /// `BrowserImportBar`. Hidden on platforms / setups where no supported browser is
    /// detected.
    fn import_bar(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<Div> {
        let bar_state = self
            .import
            .as_ref()
            .filter(|bar| !bar.browsers.is_empty())?;
        let lang = self.lang;
        let busy = bar_state.busy;
        let browsers: Vec<(String, String)> = bar_state
            .browsers
            .iter()
            .map(|browser| (browser.id.clone(), browser.name.clone()))
            .collect();
        let profiles: Vec<(String, String)> = bar_state
            .browsers
            .iter()
            .find(|browser| browser.id == bar_state.browser_id)
            .map(|browser| {
                browser
                    .profiles
                    .iter()
                    .map(|profile| (profile.id.clone(), profile.name.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let hosts = if busy {
            // A disabled field: what it holds, not editable.
            let text = bar_state.hosts.read(cx).text().to_string();
            field_box("import-hosts", 6.0, t).child(if text.is_empty() {
                div()
                    .opacity(0.45)
                    .truncate()
                    .child(lang.t("cookies.domainsPlaceholder"))
            } else {
                div().truncate().child(text)
            })
        } else {
            input_box("import-hosts", 6.0, &bar_state.hosts, t)
        }
        .flex_1()
        .min_w(px(160.0))
        .tooltip(tooltip(lang.t("cookies.domainsTitle"), t));
        let run = button(
            "import-run",
            lang.t(if busy {
                "cookies.importing"
            } else {
                "cookies.import"
            }),
            Role::Plain,
            false,
            busy,
            t,
        )
        .tooltip(tooltip(IMPORT_TITLE, t))
        .when(!busy, |run| {
            run.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.run_import(cx)))
        });

        Some(
            bar(false, t)
                .flex_wrap()
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::BOLD)
                        .text_color(t.theme.text_2)
                        .child(lang.t("cookies.importFrom")),
                )
                .child(self.select(
                    SelectId::Browser,
                    &bar_state.browser_id,
                    &browsers,
                    busy,
                    t,
                    cx,
                ))
                .when(profiles.len() > 1, |bar| {
                    bar.child(self.select(
                        SelectId::Profile,
                        &bar_state.profile,
                        &profiles,
                        busy,
                        t,
                        cx,
                    ))
                })
                .child(hosts)
                .child(run)
                .children(bar_state.result.as_ref().map(|result| {
                    div()
                        .debug_selector(|| "import-result".into())
                        .w_full()
                        .opacity(0.8)
                        .text_color(if import_failed(result) {
                            t.theme.status_error
                        } else {
                            t.status_ok
                        })
                        .child(flat(result))
                })),
        )
    }

    fn cookie_rows(&self, t: &Tokens, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = t.theme;
        if self.cookies.is_empty() {
            return vec![
                div()
                    .px(px(12.0))
                    .py(px(24.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .text_center()
                    .text_color(theme.text_dim)
                    .child(flat(&empty_cookies_message(self.origin.as_deref())))
                    .into_any_element(),
            ];
        }
        self.cookies
            .iter()
            .enumerate()
            .map(|(index, cookie)| {
                let [name, value, domain, path, flags, expires, actions] = COOKIE_COLS;
                let tags = cookie_flags(cookie).into_iter().map(|flag| {
                    // `Tag`
                    div()
                        .px(px(4.0))
                        .rounded(px(3.0))
                        .border_1()
                        .border_color(t.border_3)
                        .bg(theme.surface_2)
                        .text_size(px(10.0))
                        .child(flat(&flag))
                });
                div()
                    .flex()
                    .items_start()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        cell(name)
                            .font_weight(FontWeight::BOLD)
                            .child(flat(&cookie.name)),
                    )
                    .child(cell(value).flex().child(value_chip(
                        flat(&truncate(&cookie.value, COOKIE_VALUE_LIMIT)),
                        t,
                    )))
                    .child(cell(domain).child(flat(&cookie.domain)))
                    .child(cell(path).child(flat(&cookie.path)))
                    .child(cell(flags).flex().flex_wrap().gap(px(3.0)).children(tags))
                    .child(cell(expires).child(expires_label(cookie.expires)))
                    .child(
                        cell(actions)
                            .flex()
                            .child(
                                button(("cookie-edit", index), "✎", Role::Ghost, true, false, t)
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.edit_cookie(index, cx)
                                    })),
                            )
                            .child(
                                button(("cookie-delete", index), "✕", Role::Danger, true, false, t)
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.delete_cookie(index, cx)
                                    })),
                            ),
                    )
                    .into_any_element()
            })
            .collect()
    }

    fn storage_rows(
        &self,
        area: StorageArea,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = t.theme;
        self.entries(area)
            .iter()
            .enumerate()
            .map(|(index, (key, value))| {
                let [key_col, value_col, actions] = STORAGE_COLS;
                let editing = self.editing.as_ref().filter(|editing| &editing.key == key);
                let value_cell = match editing {
                    Some(editing) => cell(value_col).child(
                        div()
                            .debug_selector(|| "entry-input".into())
                            .w_full()
                            .p(px(2.0))
                            .overflow_hidden()
                            .bg(theme.bg)
                            .text_color(theme.text)
                            .border_1()
                            .border_color(t.border_3)
                            // The field's blur: GPUI moves no focus on a click
                            // elsewhere, so the click itself ends the edit.
                            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                this.commit_editing(cx)
                            }))
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                if event.keystroke.key == "escape" {
                                    this.editing = None;
                                    cx.notify();
                                }
                            }))
                            .child(editing.input.clone()),
                    ),
                    None => {
                        let edit_key = key.clone();
                        cell(value_col).flex().child(
                            clickable(("entry-value", index))
                                .max_w_full()
                                .cursor_text()
                                .on_click(cx.listener(
                                    move |this, event: &ClickEvent, window, cx| {
                                        if event.click_count() == 2 {
                                            this.start_editing(&edit_key, window, cx);
                                        }
                                    },
                                ))
                                .child(value_chip(flat(&truncate(value, STORAGE_VALUE_LIMIT)), t)),
                        )
                    }
                };
                let (edit_key, delete_key) = (key.clone(), key.clone());
                div()
                    .flex()
                    .items_start()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(cell(key_col).font_weight(FontWeight::BOLD).child(flat(key)))
                    .child(value_cell)
                    .child(
                        cell(actions)
                            .flex()
                            .child(
                                button(("entry-edit", index), "✎", Role::Ghost, true, false, t)
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.start_editing(&edit_key, window, cx)
                                        },
                                    )),
                            )
                            .child(
                                button(("entry-delete", index), "✕", Role::Danger, true, false, t)
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.delete_entry(delete_key.clone(), cx)
                                    })),
                            ),
                    )
                    .into_any_element()
            })
            .collect()
    }

    fn table(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let (head, rows) = match self.section.area() {
            None => (
                table_head(
                    &COOKIE_COLS,
                    &["name", "value", "domain", "path", "flags", "expires", ""],
                    t,
                ),
                self.cookie_rows(t, cx),
            ),
            Some(area) => (
                table_head(&STORAGE_COLS, &["key", "value", ""], t),
                self.storage_rows(area, t, cx),
            ),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_1()
            .border_color(t.theme.border)
            .rounded(px(4.0))
            .font_family(MONO)
            .text_size(px(11.0))
            .line_height(px(13.0))
            .child(head)
            .child(
                div()
                    .id("storage-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows),
            )
    }

    /// The row under the table: what can be added to this section.
    fn add_row(&self, t: &Tokens, cx: &mut Context<Self>) -> Div {
        let row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .mt(px(8.0));
        let Some(area) = self.section.area() else {
            return row.child(
                button("cookie-add", "+ cookie", Role::Plain, false, false, t)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.add_cookie(cx))),
            );
        };
        match &self.adding {
            Some(adding) => row
                .child(input_box("entry-add-key", 6.0, &adding.key, t).w(px(160.0)))
                .child(
                    input_box("entry-add-value", 6.0, &adding.value, t)
                        .flex_1()
                        .min_w_0(),
                )
                .child(
                    button("entry-add", "add", Role::Primary, false, false, t)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.add_entry(cx))),
                )
                .child(
                    button("entry-add-cancel", "×", Role::Ghost, true, false, t).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.adding = None;
                            cx.notify();
                        }),
                    ),
                ),
            None => row
                .child(
                    button(
                        "entry-add-start",
                        add_entry_label(area),
                        Role::Plain,
                        false,
                        false,
                        t,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_adding(cx))),
                )
                .child(
                    button("entry-clear", "clear all", Role::Danger, false, false, t).on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.request_clear(window, cx)
                        }),
                    ),
                ),
        }
    }

    /// `CookieEditor`: a dialog over the whole window.
    fn editor_modal(
        &self,
        t: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let editor = self.editor.as_ref()?;
        let theme = t.theme;
        let viewport = window.viewport_size();

        // `Field`
        let field = |label: &'static str, input: AnyElement| {
            div()
                .mb(px(8.0))
                .child(
                    div()
                        .mb(px(2.0))
                        .text_size(px(10.0))
                        .line_height(px(12.0))
                        .opacity(0.6)
                        .child(label),
                )
                .child(input)
        };
        let input = |id: &'static str, entity: &Entity<TextInput>| {
            input_box(id, 8.0, entity, t).w_full().into_any_element()
        };
        let name = if editor.is_new {
            input("editor-name", &editor.name_input)
        } else {
            // A disabled field: the name of an existing cookie is not editable.
            field_box("editor-name", 8.0, t)
                .w_full()
                .child(div().truncate().child(flat(&editor.name)))
                .into_any_element()
        };
        // The component's <textarea>: 70px at least. The kit's field is one line.
        let value = input_box("editor-value", 8.0, &editor.value_input, t)
            .w_full()
            .h(px(70.0))
            .items_start()
            .py(px(4.0))
            .into_any_element();

        let check = |id: &'static str, checked: bool, label: &'static str| {
            clickable(id)
                .flex()
                .items_center()
                .gap(px(3.0))
                .cursor_pointer()
                .child(check_mark(checked, t))
                .child(label)
        };
        let same_site_options = same_site_options();
        let flags = div()
            .flex()
            .items_center()
            .gap(px(12.0))
            .text_size(px(11.0))
            .line_height(px(13.0))
            .child(
                check("editor-secure", editor.secure == Some(true), "Secure").on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(editor) = this.editor.as_mut() {
                            editor.secure = Some(editor.secure != Some(true));
                            cx.notify();
                        }
                    }),
                ),
            )
            .child(
                check(
                    "editor-http-only",
                    editor.http_only == Some(true),
                    "HttpOnly",
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if let Some(editor) = this.editor.as_mut() {
                        editor.http_only = Some(editor.http_only != Some(true));
                        cx.notify();
                    }
                })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child("SameSite")
                    .child(self.select(
                        SelectId::SameSite,
                        editor.same_site.as_deref().unwrap_or_default(),
                        &same_site_options,
                        false,
                        t,
                        cx,
                    )),
            );
        let buttons = div()
            .flex()
            .justify_end()
            .gap(px(6.0))
            .mt(px(14.0))
            .child(
                button(
                    "editor-cancel",
                    self.lang.t("common.cancel"),
                    Role::Ghost,
                    false,
                    false,
                    t,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_editor(cx))),
            )
            .child(
                button("editor-save", "Save", Role::Primary, false, false, t)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save_editor(cx))),
            );

        let dialog = div()
            .debug_selector(|| "cookie-editor".into())
            .w(px(460.0))
            .p(px(18.0))
            .rounded(px(6.0))
            .bg(theme.bg_bar)
            .border_1()
            .border_color(theme.border_2)
            .text_color(theme.text)
            // A click inside is not a click on the backdrop.
            .occlude()
            .child(
                div()
                    .mb(px(12.0))
                    .text_size(px(13.0))
                    .line_height(px(15.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(flat(&editor_title(editor.is_new, &editor.name))),
            )
            .child(field("name", name))
            .child(field("value", value))
            .child(field(
                "domain",
                input("editor-domain", &editor.domain_input),
            ))
            .child(field("path", input("editor-path", &editor.path_input)))
            .child(flags)
            .child(buttons);

        let backdrop = clickable("cookie-editor-backdrop")
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            // rgba(0, 0, 0, 0.55)
            .bg(rgba(0x0000008c))
            .occlude()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_editor(cx)))
            .child(dialog);
        // `position: fixed; inset: 0`: over the window, not only over the panel.
        Some(
            deferred(anchored().position(point(px(0.0), px(0.0))).child(backdrop))
                .with_priority(2)
                .into_any_element(),
        )
    }
}

impl Render for CookiesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::resolve(self.theme_mode, window);
        let mut root = div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .p(px(10.0))
            .text_size(px(12.0))
            .line_height(px(14.0))
            .text_color(t.theme.text_2)
            .child(self.header(&t, cx));
        if self.section == Section::Cookies {
            root = root
                .child(self.sticky_bar(&t, cx))
                .children(self.import_bar(&t, cx))
                .child(self.download_bar(&t, cx))
                .child(self.dialog_bar(&t, cx))
                .children(self.dialog_history(&t));
        }
        root.child(self.table(&t, cx))
            .child(self.add_row(&t, cx))
            .children(self.editor_modal(&t, window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Utc};
    use serde_json::json;

    fn cookie(name: &str, domain: &str, path: &str) -> CookieRow {
        CookieRow {
            name: name.into(),
            value: "v".into(),
            domain: domain.into(),
            path: path.into(),
            ..CookieRow::default()
        }
    }

    fn pairs(entries: &[(&str, &str)]) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    fn keys(entries: &[(String, String)]) -> Vec<&str> {
        entries.iter().map(|(key, _)| key.as_str()).collect()
    }

    // ── truncate ────────────────────────────────────────────────────────────

    #[test]
    fn 제한_이하의_값은_그대로_둔다() {
        assert_eq!(truncate("", 80), "");
        assert_eq!(truncate("abc", 80), "abc");
        assert_eq!(truncate(&"a".repeat(80), 80), "a".repeat(80));
    }

    #[test]
    fn 제한을_넘는_값은_잘라내고_말줄임표를_붙인다() {
        assert_eq!(
            truncate(&"a".repeat(81), 80),
            format!("{}…", "a".repeat(80))
        );
        assert_eq!(truncate("abcdef", 3), "abc…");
        assert_eq!(truncate("abc", 0), "…");
    }

    #[test]
    fn 길이는_utf16_단위로_센다() {
        // 한글은 한 단위, 이모지는 두 단위다.
        assert_eq!(truncate("가나다라", 4), "가나다라");
        assert_eq!(truncate("가나다라마", 4), "가나다라…");
        assert_eq!(truncate("😀😀", 4), "😀😀");
        assert_eq!(truncate("😀😀a", 4), "😀😀…");
    }

    #[test]
    fn 자르는_자리에_걸친_글자는_통째로_뺀다() {
        // JS는 반쪽 대리 문자를 남기지만 Rust 문자열은 그럴 수 없다.
        assert_eq!(truncate("a😀b", 2), "a…");
    }

    // ── expires_label ───────────────────────────────────────────────────────

    #[test]
    fn 만료가_없거나_거짓_값이면_session이다() {
        assert_eq!(expires_label(None), "session");
        assert_eq!(expires_label(Some(0.0)), "session");
        assert_eq!(expires_label(Some(-0.0)), "session");
        assert_eq!(expires_label(Some(f64::NAN)), "session");
    }

    #[test]
    fn 만료_시각은_utc_분까지_쓴다() {
        assert_eq!(expires_label(Some(1_700_000_000.0)), "2023-11-14 22:13");
        // 초와 그 아래는 버린다.
        assert_eq!(expires_label(Some(1_700_000_039.9)), "2023-11-14 22:13");
        assert_eq!(expires_label(Some(0.001)), "1970-01-01 00:00");
        // 윤일
        assert_eq!(expires_label(Some(1_709_164_800.0)), "2024-02-29 00:00");
        assert_eq!(expires_label(Some(951_782_400.0)), "2000-02-29 00:00");
        assert_eq!(expires_label(Some(1_709_251_199.0)), "2024-02-29 23:59");
    }

    #[test]
    fn cdp의_세션_쿠키_표시인_음수_1은_참이라_1969년으로_보인다() {
        // new Date(-1000).toISOString() === "1969-12-31T23:59:59.000Z"
        assert_eq!(expires_label(Some(-1.0)), "1969-12-31 23:59");
    }

    #[test]
    fn 네_자리를_벗어난_해는_부호와_여섯_자리라_시까지만_남는다() {
        // new Date(253402300800000).toISOString() === "+010000-01-01T00:00:00.000Z"
        assert_eq!(expires_label(Some(253_402_300_800.0)), "+010000-01-01 00");
        // new Date(-62167305600000).toISOString() === "-000001-12-31T00:00:00.000Z"
        assert_eq!(expires_label(Some(-62_167_305_600.0)), "-000001-12-31 00");
        assert_eq!(expires_label(Some(-62_167_219_200.0)), "0000-01-01 00:00");
        // Date가 담는 마지막 순간
        assert_eq!(expires_label(Some(8.64e12)), "+275760-09-13 00");
    }

    #[test]
    fn date가_담지_못하는_시각은_invalid_date로_쓴다() {
        assert_eq!(expires_label(Some(8.64e12 + 1.0)), "Invalid Date");
        assert_eq!(expires_label(Some(f64::INFINITY)), "Invalid Date");
        assert_eq!(expires_label(Some(-1e300)), "Invalid Date");
    }

    // ── cookie rows ─────────────────────────────────────────────────────────

    #[test]
    fn 플래그는_참인_것만_정해진_순서로_나온다() {
        let mut row = cookie("a", "a.test", "/");
        assert!(cookie_flags(&row).is_empty());
        row.secure = Some(false);
        row.http_only = Some(true);
        row.same_site = Some("Lax".into());
        assert_eq!(cookie_flags(&row), ["HttpOnly", "Lax"]);
        row.secure = Some(true);
        assert_eq!(cookie_flags(&row), ["Secure", "HttpOnly", "Lax"]);
        // 빈 sameSite는 거짓이다.
        row.same_site = Some(String::new());
        assert_eq!(cookie_flags(&row), ["Secure", "HttpOnly"]);
    }

    #[test]
    fn 삭제_요청은_이름과_출처를_넘기고_빈_도메인과_경로는_뺀다() {
        assert_eq!(
            delete_params(&cookie("sid", ".a.test", "/app"), Some("https://a.test")),
            CookieDeleteParams {
                name: "sid".into(),
                url: Some("https://a.test".into()),
                domain: Some(".a.test".into()),
                path: Some("/app".into()),
            }
        );
        assert_eq!(
            delete_params(&cookie("sid", "", ""), None),
            CookieDeleteParams {
                name: "sid".into(),
                ..CookieDeleteParams::default()
            }
        );
        // `origin ?? undefined`: 빈 출처는 그대로 넘어간다.
        assert_eq!(
            delete_params(&cookie("sid", "", ""), Some("")).url,
            Some(String::new())
        );
    }

    #[test]
    fn 저장_요청은_거짓인_플래그와_빈_칸을_빼고_만료는_넘기지_않는다() {
        let mut row = cookie("sid", "a.test", "/");
        row.value = "abc".into();
        row.expires = Some(1_700_000_000.0);
        row.secure = Some(true);
        row.http_only = Some(false);
        row.same_site = Some("Strict".into());
        let params = set_params(&row, Some("https://a.test"));
        assert_eq!(
            params,
            CookieSetParams {
                name: "sid".into(),
                value: "abc".into(),
                url: Some("https://a.test".into()),
                domain: Some("a.test".into()),
                path: Some("/".into()),
                secure: Some(true),
                http_only: None,
                same_site: Some("Strict".into()),
                expires: None,
            }
        );
        assert_eq!(
            serde_json::to_value(&params).unwrap(),
            json!({
                "name": "sid", "value": "abc", "url": "https://a.test",
                "domain": "a.test", "path": "/", "secure": true, "sameSite": "Strict"
            })
        );

        let bare = set_params(&cookie("n", "", ""), None);
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            json!({ "name": "n", "value": "v" })
        );
    }

    #[test]
    fn 새_쿠키는_출처의_호스트를_도메인으로_하고_경로는_루트다() {
        let fresh = new_cookie(Some("https://www.example.com:8443")).unwrap();
        assert_eq!(fresh.domain, "www.example.com");
        assert_eq!(fresh.path, "/");
        assert_eq!((fresh.name.as_str(), fresh.value.as_str()), ("", ""));
        assert_eq!(fresh.expires, None);
        assert_eq!(
            new_cookie(Some("http://127.0.0.1:8777")).unwrap().domain,
            "127.0.0.1"
        );
        assert_eq!(
            new_cookie(Some("https://한글.kr")).unwrap().domain,
            "xn--bj0bj06e.kr"
        );
        assert_eq!(new_cookie(Some("file://")).unwrap().domain, "");
    }

    #[test]
    fn 출처가_없으면_새_쿠키의_도메인은_비어_있다() {
        assert_eq!(new_cookie(None).unwrap().domain, "");
        assert_eq!(new_cookie(Some("")).unwrap().domain, "");
    }

    #[test]
    fn 출처가_url이_아니면_새_쿠키를_만들지_않는다() {
        // about:blank의 location.origin은 문자열 "null"이고 new URL("null")은 던진다.
        assert_eq!(new_cookie(Some("null")), None);
        assert_eq!(new_cookie(Some("not a url")), None);
    }

    #[test]
    fn 문구는_컴포넌트에_적힌_영어_그대로다() {
        assert_eq!(
            empty_cookies_message(Some("https://a.test")),
            "No cookies for https://a.test"
        );
        assert_eq!(empty_cookies_message(None), "No cookies for this origin");
        // `origin ?? …`: 빈 출처는 빈 채로 쓴다.
        assert_eq!(empty_cookies_message(Some("")), "No cookies for ");
        assert_eq!(editor_title(true, "ignored"), "New cookie");
        assert_eq!(editor_title(false, "sid"), "Edit sid");
        assert_eq!(add_entry_label(StorageArea::Local), "+ localStorage entry");
        assert_eq!(
            add_entry_label(StorageArea::Session),
            "+ sessionStorage entry"
        );
        assert_eq!(clear_confirm(StorageArea::Local), "Clear all localStorage?");
        assert_eq!(
            clear_confirm(StorageArea::Session),
            "Clear all sessionStorage?"
        );
    }

    #[test]
    fn 섹션은_저장소_영역으로_이어진다() {
        assert_eq!(Section::default(), Section::Cookies);
        assert_eq!(Section::Cookies.area(), None);
        assert_eq!(Section::Local.area(), Some(StorageArea::Local));
        assert_eq!(Section::Session.area(), Some(StorageArea::Session));
    }

    // ── record_entries ──────────────────────────────────────────────────────

    #[test]
    fn 저장소_항목은_들어온_순서를_지킨다() {
        let entries = record_entries(pairs(&[("b", "1"), ("a", "2"), ("c", "3")]));
        assert_eq!(keys(&entries), ["b", "a", "c"]);
    }

    #[test]
    fn 정수_꼴_키는_자바스크립트_객체처럼_오름차순으로_앞에_온다() {
        let entries = record_entries(pairs(&[
            ("b", ""),
            ("10", ""),
            ("a", ""),
            ("2", ""),
            ("0", ""),
        ]));
        assert_eq!(keys(&entries), ["0", "2", "10", "b", "a"]);
    }

    #[test]
    fn 정수처럼_보여도_배열_색인이_아닌_키는_문자열_키다() {
        let entries = record_entries(pairs(&[
            ("01", ""),
            ("-1", ""),
            ("1.5", ""),
            ("4294967295", ""),
            ("", ""),
            ("+1", ""),
            ("4294967294", ""),
            ("1", ""),
        ]));
        assert_eq!(
            keys(&entries),
            ["1", "4294967294", "01", "-1", "1.5", "4294967295", "", "+1"]
        );
    }

    #[test]
    fn 같은_키가_두_번_오면_첫_자리에_마지막_값을_둔다() {
        let entries = record_entries(pairs(&[("a", "1"), ("b", "2"), ("a", "3")]));
        assert_eq!(entries, pairs(&[("a", "3"), ("b", "2")]));
        assert!(record_entries(Vec::new()).is_empty());
    }

    // ── browser import ──────────────────────────────────────────────────────

    fn profiles(ids: &[&str]) -> Vec<BrowserProfileInfo> {
        ids.iter()
            .map(|id| BrowserProfileInfo {
                id: id.to_string(),
                name: format!("name of {id}"),
            })
            .collect()
    }

    #[test]
    fn 기본_프로필은_default이고_없으면_첫_프로필이다() {
        assert_eq!(
            default_profile_id(&profiles(&["Profile 1", "Default"])),
            "Default"
        );
        assert_eq!(
            default_profile_id(&profiles(&["Profile 1", "Profile 2"])),
            "Profile 1"
        );
        assert_eq!(default_profile_id(&[]), "");
        // 대소문자까지 같아야 한다.
        assert_eq!(default_profile_id(&profiles(&["x", "default"])), "x");
    }

    #[test]
    fn 도메인_칸은_쉼표로_나누고_다듬어_빈_것을_버린다() {
        assert_eq!(
            parse_hosts(" github.com , ,google.com,, "),
            ["github.com", "google.com"]
        );
        assert!(parse_hosts("").is_empty());
        assert!(parse_hosts(" , ").is_empty());
        assert_eq!(parse_hosts("a b"), ["a b"]);
        // JS trim은 U+FEFF를 공백으로 보고 U+0085는 아니다.
        assert_eq!(parse_hosts("\u{feff}a\u{3000},\u{85}"), ["a", "\u{85}"]);
    }

    fn imported(
        imported: u64,
        total: u64,
        undecryptable: u64,
        skipped: u64,
    ) -> BrowserImportResult {
        BrowserImportResult {
            ok: true,
            imported,
            skipped,
            undecryptable,
            total,
            error: None,
        }
    }

    #[test]
    fn 가져오기_결과는_개수와_건너뛴_수를_적는다() {
        assert_eq!(
            import_result_text(Ok(imported(12, 15, 0, 0))),
            "✓ imported 12/15"
        );
        assert_eq!(
            import_result_text(Ok(imported(12, 15, 2, 0))),
            "✓ imported 12/15 · 2 app-bound skipped"
        );
        assert_eq!(
            import_result_text(Ok(imported(12, 15, 0, 1))),
            "✓ imported 12/15 · 1 skipped"
        );
        assert_eq!(
            import_result_text(Ok(imported(12, 15, 2, 1))),
            "✓ imported 12/15 · 2 app-bound skipped · 1 skipped"
        );
        assert_eq!(
            import_result_text(Ok(imported(0, 0, 0, 0))),
            "✓ imported 0/0"
        );
    }

    #[test]
    fn 가져오기_실패는_경고_표시와_오류를_적는다() {
        let failed = |error: Option<&str>| BrowserImportResult {
            ok: false,
            error: error.map(str::to_string),
            ..BrowserImportResult::default()
        };
        assert_eq!(
            import_result_text(Ok(failed(Some("Keychain denied")))),
            "⚠ Keychain denied"
        );
        assert_eq!(import_result_text(Ok(failed(None))), "⚠ import failed");
        // `??`: 빈 오류는 빈 채로 쓴다.
        assert_eq!(import_result_text(Ok(failed(Some("")))), "⚠ ");
        assert_eq!(import_result_text(Err("boom".into())), "⚠ boom");
    }

    #[test]
    fn 결과가_경고로_시작하면_오류_색이다() {
        assert!(import_failed("⚠ boom"));
        assert!(!import_failed("✓ imported 1/1"));
        assert!(!import_failed(""));
    }

    // ── select ──────────────────────────────────────────────────────────────

    #[test]
    fn select는_값이_같은_옵션의_라벨을_보여준다() {
        let options = same_site_options();
        assert_eq!(select_label(&options, "Lax"), "Lax");
        assert_eq!(select_label(&options, ""), "(none)");
    }

    #[test]
    fn 맞는_옵션이_없으면_첫_옵션을_보여준다() {
        assert_eq!(select_label(&same_site_options(), "lax"), "(none)");
        assert_eq!(select_label(&[], "x"), "");
    }

    #[test]
    fn samesite_옵션은_none_strict_lax_none_순이다() {
        let options = same_site_options();
        let values: Vec<&str> = options.iter().map(|(value, _)| value.as_str()).collect();
        assert_eq!(values, ["", "Strict", "Lax", "None"]);
    }

    // ── dialog history ──────────────────────────────────────────────────────

    #[test]
    fn 대화상자_종류마다_색이_다르다() {
        assert_eq!(dialog_tone("alert"), DialogTone::Warn);
        assert_eq!(dialog_tone("confirm"), DialogTone::Info);
        assert_eq!(dialog_tone("prompt"), DialogTone::Magenta);
        assert_eq!(dialog_tone("beforeunload"), DialogTone::Dim);
        assert_eq!(dialog_tone("ALERT"), DialogTone::Dim);
        assert_eq!(dialog_tone(""), DialogTone::Dim);
    }

    fn kst() -> FixedOffset {
        FixedOffset::east_opt(9 * 3600).unwrap()
    }

    #[test]
    fn 시각은_현지_12시간제로_쓴다() {
        // 2023-11-14T22:13:20Z = 서울 15일 07:13:20
        let ts = 1_700_000_000_000.0;
        assert_eq!(time_label(ts, Lang::En, &kst()), "7:13:20\u{202f}AM");
        assert_eq!(time_label(ts, Lang::Ko, &kst()), "오전 7:13:20");
        assert_eq!(time_label(ts, Lang::En, &Utc), "10:13:20\u{202f}PM");
        assert_eq!(time_label(ts, Lang::Ko, &Utc), "오후 10:13:20");
        // 밀리초는 버린다.
        assert_eq!(time_label(ts + 999.9, Lang::En, &Utc), "10:13:20\u{202f}PM");
    }

    #[test]
    fn 자정과_정오는_12시다() {
        assert_eq!(time_label(0.0, Lang::En, &Utc), "12:00:00\u{202f}AM");
        assert_eq!(
            time_label(43_200_000.0, Lang::En, &Utc),
            "12:00:00\u{202f}PM"
        );
        assert_eq!(time_label(43_200_000.0, Lang::Ko, &Utc), "오후 12:00:00");
    }

    #[test]
    fn 표현할_수_없는_시각은_invalid_date다() {
        assert_eq!(time_label(f64::NAN, Lang::En, &Utc), "Invalid Date");
        assert_eq!(time_label(8.64e15 + 1.0, Lang::En, &Utc), "Invalid Date");
        assert_eq!(time_label(-1e300, Lang::Ko, &Utc), "Invalid Date");
    }

    // ── JSON ────────────────────────────────────────────────────────────────

    #[test]
    fn cdp_쿠키를_읽고_모르는_필드는_넘긴다() {
        let list: CookieList = serde_json::from_value(json!({
            "cookies": [{
                "name": "sid", "value": "abc", "domain": ".a.test", "path": "/",
                "expires": 1700000000.5, "size": 6, "httpOnly": true, "secure": false,
                "session": false, "sameSite": "Lax", "priority": "Medium"
            }, { "name": "bare" }],
            "origin": null
        }))
        .unwrap();
        assert_eq!(list.origin, None);
        assert_eq!(
            list.cookies,
            [
                CookieRow {
                    name: "sid".into(),
                    value: "abc".into(),
                    domain: ".a.test".into(),
                    path: "/".into(),
                    expires: Some(1_700_000_000.5),
                    secure: Some(false),
                    http_only: Some(true),
                    same_site: Some("Lax".into()),
                },
                CookieRow {
                    name: "bare".into(),
                    ..CookieRow::default()
                },
            ]
        );
    }

    #[test]
    fn 설정과_가져오기_값은_일렉트론_앱의_필드_이름을_쓴다() {
        let persistence: Persistence =
            serde_json::from_value(json!({ "enabled": true, "snapshotCount": 4 })).unwrap();
        assert_eq!(
            persistence,
            Persistence {
                enabled: true,
                snapshot_count: 4
            }
        );

        let dialog: DialogSettings = serde_json::from_value(json!({
            "autoDismiss": false,
            "history": [{ "ts": 1700000000000u64, "type": "alert", "message": "hi", "url": "https://a.test/" }]
        }))
        .unwrap();
        assert!(!dialog.auto_dismiss);
        assert_eq!(dialog.history.len(), 1);
        assert_eq!(dialog.history[0].kind, "alert");
        assert_eq!(dialog.history[0].ts, 1_700_000_000_000.0);

        let options = BrowserImportOptions {
            browser: "chrome".into(),
            profile: Some("Default".into()),
            hosts: Some(vec!["github.com".into()]),
            shared: None,
        };
        assert_eq!(
            serde_json::to_value(&options).unwrap(),
            json!({ "browser": "chrome", "profile": "Default", "hosts": ["github.com"] })
        );

        let result: BrowserImportResult = serde_json::from_value(json!({
            "ok": true, "imported": 3, "skipped": 1, "undecryptable": 2, "total": 6
        }))
        .unwrap();
        assert_eq!(result, imported(3, 6, 2, 1));

        let browser: BrowserInfo = serde_json::from_value(json!({
            "id": "arc", "name": "Arc", "profiles": [{ "id": "Default", "name": "Me" }]
        }))
        .unwrap();
        assert_eq!(browser.profiles[0].name, "Me");
    }

    #[test]
    fn 첫_응답_전에는_대화상자_자동_응답이_켜진_것으로_본다() {
        let initial = DialogSettings::default();
        assert!(initial.auto_dismiss);
        assert!(initial.history.is_empty());
    }

    #[test]
    fn 줄바꿈은_화면에서는_공백이_되고_입력_칸에서는_사라진다() {
        assert_eq!(flat("a\nb\r\nc\td"), "a b  c d");
        assert_eq!(input_value("a\nb\r\nc\td"), "abc\td");
    }
}

/// The view in a window, against the in-memory host: clicks, typing, the confirm prompt.
#[cfg(test)]
mod view_tests {
    use super::*;
    use crate::memory_host::{MemoryState, MemoryStorageHost};
    use gpui::{Modifiers, MouseUpEvent, TestAppContext, VisualTestContext};

    fn cookie(name: &str, domain: &str) -> CookieRow {
        CookieRow {
            name: name.into(),
            value: format!("value of {name}"),
            domain: domain.into(),
            path: "/".into(),
            expires: Some(1_700_000_000.0),
            secure: Some(true),
            http_only: None,
            same_site: Some("Lax".into()),
        }
    }

    fn pairs(entries: &[(&str, &str)]) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    fn browser(id: &str, profiles: &[&str]) -> BrowserInfo {
        BrowserInfo {
            id: id.into(),
            name: id.to_uppercase(),
            profiles: profiles
                .iter()
                .map(|profile| BrowserProfileInfo {
                    id: profile.to_string(),
                    name: profile.to_string(),
                })
                .collect(),
        }
    }

    fn fixture() -> MemoryState {
        MemoryState {
            cookies: vec![cookie("sid", "a.test"), cookie("theme", ".a.test")],
            origin: Some("https://a.test".into()),
            local: pairs(&[("alpha", "1"), ("beta", "2")]),
            session: pairs(&[("gamma", "3")]),
            persistence: Persistence {
                enabled: false,
                snapshot_count: 2,
            },
            ask_where_to_save: true,
            dialog: DialogSettings {
                auto_dismiss: false,
                history: vec![DialogRecord {
                    ts: 1_700_000_000_000.0,
                    kind: "alert".into(),
                    message: "hi".into(),
                    url: "https://a.test/".into(),
                }],
            },
            browsers: vec![
                browser("chrome", &["Profile 1", "Default"]),
                browser("firefox", &["abc.default"]),
            ],
            importable: vec![
                ("chrome".into(), cookie("from_chrome", ".github.com")),
                ("chrome".into(), cookie("other", ".example.org")),
                ("firefox".into(), cookie("from_firefox", ".github.com")),
            ],
            ..MemoryState::default()
        }
    }

    fn open(
        state: MemoryState,
        cx: &mut TestAppContext,
    ) -> (
        Rc<MemoryStorageHost>,
        Entity<CookiesPanel>,
        &mut VisualTestContext,
    ) {
        cx.update(rever_ui_kit::text_input::bind_keys);
        let host = Rc::new(MemoryStorageHost::new(state));
        let (panel, cx) = cx.add_window_view({
            let host = host.clone();
            move |_, cx| CookiesPanel::new(host, Lang::En, ThemeMode::Light, cx)
        });
        cx.run_until_parked();
        (host, panel, cx)
    }

    /// Click where `selector` was last painted.
    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"));
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    fn double_click(selector: &'static str, cx: &mut VisualTestContext) {
        let position = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"))
            .center();
        cx.simulate_event(MouseDownEvent {
            position,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 2,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            position,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 2,
        });
        cx.run_until_parked();
    }

    fn names(cookies: &[CookieRow]) -> Vec<String> {
        cookies.iter().map(|cookie| cookie.name.clone()).collect()
    }

    // ── loading ─────────────────────────────────────────────────────────────

    #[gpui::test]
    fn 열리면_쿠키와_저장소와_설정을_호스트에서_읽는다(
        cx: &mut TestAppContext,
    ) {
        let (_, panel, cx) = open(fixture(), cx);
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.section(), Section::Cookies);
            assert_eq!(names(&panel.cookies), ["sid", "theme"]);
            assert_eq!(panel.origin.as_deref(), Some("https://a.test"));
            assert_eq!(panel.local, pairs(&[("alpha", "1"), ("beta", "2")]));
            assert_eq!(panel.session, pairs(&[("gamma", "3")]));
            assert_eq!(panel.sticky.snapshot_count, 2);
            assert!(panel.ask_where_to_save);
            assert!(!panel.dialog.auto_dismiss);
            assert_eq!(panel.dialog.history.len(), 1);
            let bar = panel.import.as_ref().unwrap();
            // The first browser, and its `Default` profile though it is listed second.
            assert_eq!(bar.browser_id, "chrome");
            assert_eq!(bar.profile, "Default");
        });
    }

    #[gpui::test]
    fn 삼_초마다_다시_읽는다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        host.state.borrow_mut().cookies.pop();
        host.state
            .borrow_mut()
            .local
            .push(("new".into(), "9".into()));
        cx.executor()
            .advance_clock(Duration::from_millis(REFRESH_MS - 1));
        cx.run_until_parked();
        assert_eq!(panel.read_with(cx, |panel, _| panel.cookies.len()), 2);
        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert_eq!(names(&panel.cookies), ["sid"]);
            assert_eq!(panel.local.len(), 3);
        });
        // And again, three seconds later.
        host.state.borrow_mut().cookies.clear();
        cx.executor()
            .advance_clock(Duration::from_millis(REFRESH_MS));
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| panel.cookies.is_empty()));
    }

    #[gpui::test]
    fn 새로고침_버튼은_바로_다시_읽는다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        host.state.borrow_mut().origin = Some("https://b.test".into());
        click("storage-refresh", cx);
        assert_eq!(
            panel.read_with(cx, |panel, _| panel.origin.clone()),
            Some("https://b.test".to_string())
        );
    }

    #[gpui::test]
    fn 읽기가_하나라도_실패하면_이전_목록을_그대로_둔다(
        cx: &mut TestAppContext,
    ) {
        let (host, panel, cx) = open(fixture(), cx);
        host.state.borrow_mut().cookies.clear();
        host.state.borrow_mut().fail = Some("no active browser target".into());
        click("storage-refresh", cx);
        assert_eq!(panel.read_with(cx, |panel, _| panel.cookies.len()), 2);
    }

    #[gpui::test]
    fn 탭을_누르면_섹션이_바뀐다(cx: &mut TestAppContext) {
        let (_, panel, cx) = open(fixture(), cx);
        assert!(cx.debug_bounds("cookie-add").is_some());
        click("storage-tab-local", cx);
        assert_eq!(
            panel.read_with(cx, |panel, _| panel.section()),
            Section::Local
        );
        assert!(cx.debug_bounds("entry-edit-1").is_some());
        click("storage-tab-session", cx);
        assert_eq!(
            panel.read_with(cx, |panel, _| panel.section()),
            Section::Session
        );
        click("storage-tab-cookies", cx);
        assert_eq!(
            panel.read_with(cx, |panel, _| panel.section()),
            Section::Cookies
        );
    }

    // ── cookie settings ─────────────────────────────────────────────────────

    #[gpui::test]
    fn 설정_체크박스는_호스트에_쓰고_돌아온_값을_보여준다(
        cx: &mut TestAppContext,
    ) {
        let (host, panel, cx) = open(fixture(), cx);
        click("sticky-toggle", cx);
        assert!(host.state.borrow().persistence.enabled);
        assert!(panel.read_with(cx, |panel, _| panel.sticky.enabled));
        click("sticky-toggle", cx);
        assert!(!host.state.borrow().persistence.enabled);

        click("download-ask-toggle", cx);
        assert!(!host.state.borrow().ask_where_to_save);
        assert!(!panel.read_with(cx, |panel, _| panel.ask_where_to_save));

        click("dialog-auto-toggle", cx);
        assert!(host.state.borrow().dialog.auto_dismiss);
        assert!(panel.read_with(cx, |panel, _| panel.dialog.auto_dismiss));
    }

    #[gpui::test]
    fn 호출이_실패하면_체크박스는_그대로다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        host.state.borrow_mut().fail = Some("nope".into());
        click("sticky-toggle", cx);
        click("download-ask-toggle", cx);
        panel.read_with(cx, |panel, _| {
            assert!(!panel.sticky.enabled);
            assert!(panel.ask_where_to_save);
        });
    }

    #[gpui::test]
    fn snapshot_now는_저장된_스냅숏_수를_갱신한다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        click("sticky-snapshot", cx);
        assert_eq!(host.state.borrow().persistence.snapshot_count, 3);
        assert_eq!(
            panel.read_with(cx, |panel, _| panel.sticky.snapshot_count),
            3
        );
    }

    #[gpui::test]
    fn clear는_대화상자_기록을_비운다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        click("dialog-clear", cx);
        assert!(host.state.borrow().dialog.history.is_empty());
        assert!(panel.read_with(cx, |panel, _| panel.dialog.history.is_empty()));
    }

    // ── cookies ─────────────────────────────────────────────────────────────

    #[gpui::test]
    fn 쿠키의_삭제_버튼은_그_쿠키를_지우고_다시_읽는다(
        cx: &mut TestAppContext,
    ) {
        let (host, panel, cx) = open(fixture(), cx);
        click("cookie-delete-0", cx);
        assert_eq!(names(&host.state.borrow().cookies), ["theme"]);
        assert_eq!(
            panel.read_with(cx, |panel, _| names(&panel.cookies)),
            ["theme"]
        );
    }

    #[gpui::test]
    fn 삭제가_실패하면_다시_읽지_않는다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        host.state.borrow_mut().origin = Some("https://changed.test".into());
        host.state.borrow_mut().fail = Some("no active browser target".into());
        click("cookie-delete-0", cx);
        host.state.borrow_mut().fail = None;
        cx.run_until_parked();
        // A refresh would have brought the new origin.
        assert_eq!(
            panel.read_with(cx, |panel, _| panel.origin.clone()),
            Some("https://a.test".to_string())
        );
    }

    #[gpui::test]
    fn 쿠키를_편집해_저장하면_만료_없이_다시_쓴다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        click("cookie-edit-0", cx);
        assert!(panel.read_with(cx, |panel, _| panel.editor.is_some()));
        click("editor-value", cx);
        cx.simulate_input("!");
        click("editor-secure", cx);
        click("editor-http-only", cx);
        click("editor-save", cx);

        assert!(panel.read_with(cx, |panel, _| panel.editor.is_none()));
        let state = host.state.borrow();
        assert_eq!(state.cookies.len(), 2);
        let saved = &state.cookies[0];
        assert_eq!(saved.name, "sid");
        assert_eq!(saved.value, "value of sid!");
        // Unchecked is not sent; checked is.
        assert_eq!(saved.secure, None);
        assert_eq!(saved.http_only, Some(true));
        assert_eq!(saved.same_site.as_deref(), Some("Lax"));
        // The fake stores a cookie set without `expires` the way CDP reports one.
        assert_eq!(saved.expires, Some(-1.0));
        drop(state);
        assert_eq!(
            panel.read_with(cx, |panel, _| panel.cookies[0].value.clone()),
            "value of sid!"
        );
    }

    #[gpui::test]
    fn 있는_쿠키의_이름은_고칠_수_없다(cx: &mut TestAppContext) {
        let (host, _, cx) = open(fixture(), cx);
        click("cookie-edit-0", cx);
        click("editor-name", cx);
        cx.simulate_input("zzz");
        click("editor-save", cx);
        assert_eq!(names(&host.state.borrow().cookies), ["sid", "theme"]);
    }

    #[gpui::test]
    fn 새_쿠키는_출처의_호스트를_도메인으로_해서_만든다(
        cx: &mut TestAppContext,
    ) {
        let (host, panel, cx) = open(fixture(), cx);
        click("cookie-add", cx);
        click("editor-name", cx);
        cx.simulate_input("fresh");
        click("editor-value", cx);
        cx.simulate_input("42");
        click("editor-save", cx);

        let state = host.state.borrow();
        let saved = state.cookies.last().unwrap();
        assert_eq!(
            (
                saved.name.as_str(),
                saved.value.as_str(),
                saved.domain.as_str(),
                saved.path.as_str()
            ),
            ("fresh", "42", "a.test", "/")
        );
        assert_eq!(
            (saved.secure, saved.http_only, &saved.same_site),
            (None, None, &None)
        );
        drop(state);
        assert_eq!(panel.read_with(cx, |panel, _| panel.cookies.len()), 3);
    }

    #[gpui::test]
    fn 출처가_url이_아니면_cookie_추가_버튼은_아무것도_하지_않는다(
        cx: &mut TestAppContext,
    ) {
        let mut state = fixture();
        state.origin = Some("null".into());
        let (_, panel, cx) = open(state, cx);
        click("cookie-add", cx);
        assert!(panel.read_with(cx, |panel, _| panel.editor.is_none()));
    }

    #[gpui::test]
    fn samesite를_고르면_그_값으로_저장한다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        click("cookie-edit-0", cx);
        click("editor-same-site", cx);
        // Options: (none), Strict, Lax, None.
        click("editor-same-site-1", cx);
        assert_eq!(panel.read_with(cx, |panel, _| panel.open_select), None);
        click("editor-save", cx);
        assert_eq!(
            host.state.borrow().cookies[0].same_site.as_deref(),
            Some("Strict")
        );

        click("cookie-edit-0", cx);
        click("editor-same-site", cx);
        click("editor-same-site-0", cx);
        click("editor-save", cx);
        assert_eq!(host.state.borrow().cookies[0].same_site, None);
    }

    #[gpui::test]
    fn 취소와_바깥_클릭은_저장하지_않고_닫는다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        click("cookie-edit-0", cx);
        click("editor-value", cx);
        cx.simulate_input("!");
        click("editor-cancel", cx);
        assert!(panel.read_with(cx, |panel, _| panel.editor.is_none()));

        click("cookie-edit-0", cx);
        // Inside the dialog, away from any control: it stays open.
        let dialog = cx.debug_bounds("cookie-editor").unwrap();
        cx.simulate_click(dialog.origin + point(px(4.0), px(4.0)), Modifiers::none());
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| panel.editor.is_some()));
        // On the backdrop: it closes.
        let backdrop = cx.debug_bounds("cookie-editor-backdrop").unwrap();
        cx.simulate_click(backdrop.origin + point(px(4.0), px(4.0)), Modifiers::none());
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| panel.editor.is_none()));
        assert_eq!(host.state.borrow().cookies[0].value, "value of sid");
    }

    #[gpui::test]
    fn 저장이_실패하면_편집_창은_열린_채로_남는다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        click("cookie-edit-0", cx);
        host.state.borrow_mut().fail = Some("no active browser target".into());
        click("editor-save", cx);
        assert!(panel.read_with(cx, |panel, _| panel.editor.is_some()));
    }

    #[gpui::test]
    fn 편집_창이_열려_있으면_아래의_버튼은_눌리지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (host, _, cx) = open(fixture(), cx);
        let delete = cx.debug_bounds("cookie-delete-1").unwrap().center();
        click("cookie-edit-0", cx);
        cx.simulate_click(delete, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(host.state.borrow().cookies.len(), 2);
    }

    // ── storage entries ─────────────────────────────────────────────────────

    #[gpui::test]
    fn 값을_고치고_엔터를_누르면_저장하고_다시_읽는다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        click("storage-tab-local", cx);
        click("entry-edit-0", cx);
        // The field took focus by itself, with the cursor after the value.
        cx.simulate_input("23");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            host.state.borrow().local,
            pairs(&[("alpha", "123"), ("beta", "2")])
        );
        panel.read_with(cx, |panel, _| {
            assert!(panel.editing.is_none());
            assert_eq!(panel.local[0].1, "123");
        });
        // sessionStorage was not touched.
        assert_eq!(host.state.borrow().session, pairs(&[("gamma", "3")]));
    }

    #[gpui::test]
    fn 세션_저장소의_값은_세션_저장소에_쓴다(cx: &mut TestAppContext) {
        let (host, _, cx) = open(fixture(), cx);
        click("storage-tab-session", cx);
        click("entry-edit-0", cx);
        cx.simulate_input("!");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(host.state.borrow().session, pairs(&[("gamma", "3!")]));
        assert_eq!(host.state.borrow().local.len(), 2);
    }

    #[gpui::test]
    fn 값을_두_번_누르면_편집이_시작된다(cx: &mut TestAppContext) {
        let (_, panel, cx) = open(fixture(), cx);
        click("storage-tab-local", cx);
        click("entry-value-1", cx);
        assert!(panel.read_with(cx, |panel, _| panel.editing.is_none()));
        double_click("entry-value-1", cx);
        assert_eq!(
            panel.read_with(cx, |panel, _| panel.editing.as_ref().map(|e| e.key.clone())),
            Some("beta".to_string())
        );
    }

    #[gpui::test]
    fn esc는_저장하지_않고_편집을_끝낸다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        click("storage-tab-local", cx);
        click("entry-edit-0", cx);
        cx.simulate_input("zzz");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| panel.editing.is_none()));
        assert_eq!(host.state.borrow().local[0].1, "1");
    }

    #[gpui::test]
    fn 편집_칸_밖을_누르면_저장한다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        click("storage-tab-local", cx);
        click("entry-edit-0", cx);
        cx.simulate_input("0");
        // Inside the field: still editing.
        click("entry-input", cx);
        assert!(panel.read_with(cx, |panel, _| panel.editing.is_some()));
        click("entry-value-1", cx);
        assert!(panel.read_with(cx, |panel, _| panel.editing.is_none()));
        assert_eq!(host.state.borrow().local[0].1, "10");
    }

    #[gpui::test]
    fn 창이_비활성이_되면_편집_중인_값을_저장한다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        click("storage-tab-local", cx);
        click("entry-edit-0", cx);
        cx.simulate_input("9");
        // Becoming active again is not a blur.
        assert!(panel.read_with(cx, |panel, _| panel.editing.is_some()));
        cx.deactivate_window();
        cx.run_until_parked();
        assert!(panel.read_with(cx, |panel, _| panel.editing.is_none()));
        assert_eq!(host.state.borrow().local[0].1, "19");
    }

    #[gpui::test]
    fn 편집_중에_다른_탭을_누르면_편집하던_저장소에_쓴다(
        cx: &mut TestAppContext,
    ) {
        let mut state = fixture();
        // The same key in both storages: only the one being edited may change.
        state.session.push(("alpha".into(), "s".into()));
        let (host, panel, cx) = open(state, cx);
        click("storage-tab-local", cx);
        click("entry-edit-0", cx);
        cx.simulate_input("5");
        click("storage-tab-session", cx);
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.section(), Section::Session);
            assert!(panel.editing.is_none());
        });
        assert_eq!(host.state.borrow().local[0].1, "15");
        assert_eq!(
            host.state.borrow().session,
            pairs(&[("gamma", "3"), ("alpha", "s")])
        );
    }

    #[gpui::test]
    fn 추가_칸은_탭을_바꿔도_남고_add는_보이는_저장소에_쓴다(
        cx: &mut TestAppContext,
    ) {
        let (host, panel, cx) = open(fixture(), cx);
        click("storage-tab-local", cx);
        click("entry-add-start", cx);
        click("entry-add-key", cx);
        cx.simulate_input("token");
        click("storage-tab-session", cx);
        assert!(panel.read_with(cx, |panel, _| panel.adding.is_some()));
        click("entry-add", cx);
        assert_eq!(
            host.state.borrow().session.last(),
            Some(&("token".to_string(), String::new()))
        );
        assert_eq!(host.state.borrow().local.len(), 2);
    }

    #[gpui::test]
    fn 바꾸지_않아도_편집을_끝내면_값을_다시_쓴다(cx: &mut TestAppContext) {
        let (host, _, cx) = open(fixture(), cx);
        click("storage-tab-local", cx);
        click("entry-edit-0", cx);
        // The page changed the value meanwhile; the field still holds the old one.
        host.state.borrow_mut().local[0].1 = "changed by the page".into();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(host.state.borrow().local[0].1, "1");
    }

    #[gpui::test]
    fn 항목의_삭제_버튼은_그_키를_지운다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        click("storage-tab-local", cx);
        click("entry-delete-0", cx);
        assert_eq!(host.state.borrow().local, pairs(&[("beta", "2")]));
        assert_eq!(panel.read_with(cx, |panel, _| panel.local.len()), 1);
        assert_eq!(host.state.borrow().session.len(), 1);
    }

    #[gpui::test]
    fn 키와_값을_적고_add를_누르면_항목이_생긴다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        click("storage-tab-local", cx);
        click("entry-add-start", cx);
        click("entry-add-key", cx);
        cx.simulate_input("token");
        click("entry-add-value", cx);
        cx.simulate_input("abc");
        click("entry-add", cx);
        assert_eq!(
            host.state.borrow().local.last(),
            Some(&("token".to_string(), "abc".to_string()))
        );
        panel.read_with(cx, |panel, _| {
            assert!(panel.adding.is_none());
            assert_eq!(panel.local.len(), 3);
        });
    }

    #[gpui::test]
    fn 키가_비어_있으면_add는_아무것도_하지_않는다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        click("storage-tab-session", cx);
        click("entry-add-start", cx);
        click("entry-add-value", cx);
        cx.simulate_input("abc");
        click("entry-add", cx);
        assert_eq!(host.state.borrow().session.len(), 1);
        assert!(panel.read_with(cx, |panel, _| panel.adding.is_some()));
        // × puts the two buttons back.
        click("entry-add-cancel", cx);
        assert!(panel.read_with(cx, |panel, _| panel.adding.is_none()));
    }

    #[gpui::test]
    fn clear_all은_확인을_받은_뒤에만_그_저장소를_비운다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        click("storage-tab-local", cx);
        click("entry-clear", cx);
        assert_eq!(
            cx.pending_prompt().map(|(message, _)| message),
            Some("Clear all localStorage?".to_string())
        );
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert_eq!(host.state.borrow().local.len(), 2);

        click("entry-clear", cx);
        cx.simulate_prompt_answer("OK");
        cx.run_until_parked();
        assert!(host.state.borrow().local.is_empty());
        assert!(panel.read_with(cx, |panel, _| panel.local.is_empty()));
        assert_eq!(host.state.borrow().session.len(), 1);

        click("storage-tab-session", cx);
        click("entry-clear", cx);
        assert_eq!(
            cx.pending_prompt().map(|(message, _)| message),
            Some("Clear all sessionStorage?".to_string())
        );
        cx.simulate_prompt_answer("OK");
        cx.run_until_parked();
        assert!(host.state.borrow().session.is_empty());
    }

    // ── browser import ──────────────────────────────────────────────────────

    #[gpui::test]
    fn 가져오기는_고른_브라우저와_프로필과_도메인을_넘기고_결과를_적는다(
        cx: &mut TestAppContext,
    ) {
        let (host, panel, cx) = open(fixture(), cx);
        click("import-hosts", cx);
        cx.simulate_input(" github , ,nowhere ");
        click("import-run", cx);
        assert_eq!(
            host.state.borrow().last_import,
            Some(BrowserImportOptions {
                browser: "chrome".into(),
                profile: Some("Default".into()),
                hosts: Some(vec!["github".into(), "nowhere".into()]),
                shared: None,
            })
        );
        panel.read_with(cx, |panel, _| {
            let bar = panel.import.as_ref().unwrap();
            assert!(!bar.busy);
            assert_eq!(bar.result.as_deref(), Some("✓ imported 1/2 · 1 skipped"));
        });
        assert!(cx.debug_bounds("import-result").is_some());
        assert!(names(&host.state.borrow().cookies).contains(&"from_chrome".to_string()));
    }

    #[gpui::test]
    fn 가져오는_동안에는_버튼이_눌리지_않는다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        host.state.borrow_mut().hold_import = true;
        click("import-run", cx);
        panel.read_with(cx, |panel, _| {
            let bar = panel.import.as_ref().unwrap();
            assert!(bar.busy);
            assert_eq!(bar.result, None);
        });
        host.state.borrow_mut().last_import = None;
        click("import-run", cx);
        assert_eq!(host.state.borrow().last_import, None);
        // The selects are disabled too.
        click("import-browser", cx);
        assert_eq!(panel.read_with(cx, |panel, _| panel.open_select), None);
    }

    #[gpui::test]
    fn 가져오기_호출이_실패하면_오류를_적고_다시_누를_수_있다(
        cx: &mut TestAppContext,
    ) {
        let (host, panel, cx) = open(fixture(), cx);
        host.state.borrow_mut().fail = Some("Keychain access denied".into());
        click("import-run", cx);
        panel.read_with(cx, |panel, _| {
            let bar = panel.import.as_ref().unwrap();
            assert!(!bar.busy);
            assert_eq!(bar.result.as_deref(), Some("⚠ Keychain access denied"));
        });
    }

    #[gpui::test]
    fn 브라우저를_바꾸면_프로필은_그_브라우저의_기본_프로필이_된다(
        cx: &mut TestAppContext,
    ) {
        let (host, panel, cx) = open(fixture(), cx);
        // Chrome has two profiles, so its profile select is there.
        click("import-profile", cx);
        click("import-profile-0", cx);
        assert_eq!(
            panel.read_with(cx, |panel, _| panel
                .import
                .as_ref()
                .unwrap()
                .profile
                .clone()),
            "Profile 1"
        );
        click("import-browser", cx);
        click("import-browser-1", cx);
        panel.read_with(cx, |panel, _| {
            let bar = panel.import.as_ref().unwrap();
            assert_eq!(bar.browser_id, "firefox");
            assert_eq!(bar.profile, "abc.default");
            assert_eq!(panel.open_select, None);
        });
        click("import-run", cx);
        assert_eq!(
            host.state
                .borrow()
                .last_import
                .as_ref()
                .map(|o| o.browser.clone()),
            Some("firefox".to_string())
        );
    }

    #[gpui::test]
    fn 열린_select는_바깥을_누르면_닫힌다(cx: &mut TestAppContext) {
        let (_, panel, cx) = open(fixture(), cx);
        click("import-browser", cx);
        assert!(panel.read_with(cx, |panel, _| panel.open_select.is_some()));
        click("sticky-toggle", cx);
        panel.read_with(cx, |panel, _| {
            assert_eq!(panel.open_select, None);
            // The click went no further than closing the options.
            assert!(!panel.sticky.enabled);
        });
    }

    /// Click where `selector` was painted before it went away. The window keeps the
    /// bounds of what it no longer draws, so "gone" is checked by the click doing nothing.
    fn click_where_it_was(selector: &'static str, cx: &mut VisualTestContext) {
        click(selector, cx);
    }

    #[gpui::test]
    fn 설정_줄은_쿠키_섹션에만_있다(cx: &mut TestAppContext) {
        let (host, panel, cx) = open(fixture(), cx);
        click("storage-tab-local", cx);
        click_where_it_was("sticky-toggle", cx);
        click_where_it_was("download-ask-toggle", cx);
        click_where_it_was("dialog-auto-toggle", cx);
        click_where_it_was("dialog-clear", cx);
        click_where_it_was("import-run", cx);
        click_where_it_was("cookie-add", cx);
        let state = host.state.borrow();
        assert!(!state.persistence.enabled);
        assert!(state.ask_where_to_save);
        assert!(!state.dialog.auto_dismiss);
        assert_eq!(state.dialog.history.len(), 1);
        assert_eq!(state.last_import, None);
        assert!(panel.read_with(cx, |panel, _| panel.editor.is_none()));
    }

    #[gpui::test]
    fn 기록이_없으면_clear_버튼도_없다(cx: &mut TestAppContext) {
        let mut state = fixture();
        state.dialog.history.clear();
        let (_, _, cx) = open(state, cx);
        assert!(cx.debug_bounds("dialog-auto-toggle").is_some());
        assert!(cx.debug_bounds("dialog-clear").is_none());
    }

    #[gpui::test]
    fn 프로필_select는_프로필이_둘_이상일_때만_있다(cx: &mut TestAppContext) {
        let (_, panel, cx) = open(fixture(), cx);
        click("import-profile", cx);
        assert_eq!(
            panel.read_with(cx, |panel, _| panel.open_select),
            Some(SelectId::Profile)
        );
        click("import-profile-0", cx);
        click("import-browser", cx);
        // Firefox has one profile.
        click("import-browser-1", cx);
        assert_eq!(
            panel.read_with(cx, |panel, _| panel
                .import
                .as_ref()
                .unwrap()
                .browser_id
                .clone()),
            "firefox"
        );
        click_where_it_was("import-profile", cx);
        assert_eq!(panel.read_with(cx, |panel, _| panel.open_select), None);
    }

    #[gpui::test]
    fn 찾은_브라우저가_없으면_가져오기_줄을_그리지_않는다(
        cx: &mut TestAppContext,
    ) {
        let mut state = fixture();
        state.browsers.clear();
        let (_, _, cx) = open(state, cx);
        assert!(cx.debug_bounds("import-run").is_none());
        assert!(cx.debug_bounds("sticky-toggle").is_some());
    }

    #[gpui::test]
    fn 쿠키_섹션을_떠났다_오면_가져오기_줄은_처음부터_다시다(
        cx: &mut TestAppContext,
    ) {
        let (_, panel, cx) = open(fixture(), cx);
        click("import-hosts", cx);
        cx.simulate_input("github");
        click("import-run", cx);
        click("storage-tab-local", cx);
        assert!(panel.read_with(cx, |panel, _| panel.import.is_none()));
        click("storage-tab-cookies", cx);
        panel.read_with(cx, |panel, cx| {
            let bar = panel.import.as_ref().unwrap();
            assert_eq!(bar.result, None);
            assert_eq!(bar.hosts.read(cx).text(), "");
            assert_eq!(bar.browser_id, "chrome");
        });
    }
}
