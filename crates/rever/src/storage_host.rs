//! The storage panel's [`StorageHost`], backed by the engine. Only the cookie
//! surface (list/set/delete) and the Chrome cookie import are wired — that is
//! the ported feature; the localStorage / persistence / downloads / dialog tabs
//! of the panel are not plumbed yet and return defaults or "not yet".
//!
//! Engine calls go through the same `Arc<dyn Browser>` bridge the MCP tools use,
//! driven from `cx.background_spawn` (the bridge future is `Send`).

use std::sync::Arc;

use gpui::{App, Task, prelude::*};
use rever_tools_api::tool::{Browser, Cookie};
use rever_ui_storage::cookies_panel::{
    BrowserImportOptions, BrowserImportResult, BrowserInfo, BrowserProfileInfo, Call,
    CookieDeleteParams, CookieList, CookieRow, CookieSetParams, DialogSettings, Persistence,
    StorageArea, StorageHost,
};

const NOT_YET: &str = "not available yet (localStorage / persistence / downloads / dialogs aren't wired into the storage panel)";

pub(crate) struct StorageBridge {
    browser: Arc<dyn Browser>,
}

impl StorageBridge {
    pub(crate) fn new(browser: Arc<dyn Browser>) -> Self {
        Self { browser }
    }
}

fn cookie_to_row(c: &Cookie) -> CookieRow {
    CookieRow {
        name: c.name.clone(),
        value: c.value.clone(),
        domain: c.domain.clone(),
        path: c.path.clone(),
        expires: c.expires,
        secure: Some(c.secure),
        http_only: Some(c.http_only),
        same_site: c.same_site.clone(),
    }
}

/// `https://<host><path>` for a cookie's domain (leading dot stripped).
fn cookie_url(domain: &str, path: &str) -> String {
    let host = domain.trim_start_matches('.');
    let path = if path.is_empty() { "/" } else { path };
    format!("https://{host}{path}")
}

/// Whether this Mac has a Chrome whose cookies can be read.
pub(crate) fn chrome_installed() -> bool {
    std::env::var_os("HOME").is_some_and(|home| {
        std::path::Path::new(&home)
            .join("Library/Application Support/Google/Chrome/Default/Cookies")
            .exists()
    })
}

/// Copy Chrome's cookies into the engine's jar. Reading Chrome's key makes macOS
/// ask for the Keychain.
pub(crate) async fn import_chrome(
    browser: Arc<dyn Browser>,
    profile: Option<String>,
    hosts: Option<Vec<String>>,
) -> Result<BrowserImportResult, String> {
    let summary = rever_cookie_import::import_chrome_macos(profile.as_deref(), hosts.as_deref())?;
    let (mut imported, mut skipped) = (0u64, 0u64);
    for c in &summary.cookies {
        let url = cookie_url(&c.host, &c.path);
        let cookie = Cookie {
            name: c.name.clone(),
            value: c.value.clone(),
            domain: c.host.clone(),
            path: c.path.clone(),
            expires: c.expires,
            http_only: c.http_only,
            secure: c.secure,
            same_site: None,
        };
        match browser.set_cookie(&url, cookie).await {
            Ok(()) => imported += 1,
            Err(_) => skipped += 1,
        }
    }
    Ok(BrowserImportResult {
        ok: true,
        imported,
        skipped,
        undecryptable: summary.undecryptable,
        total: summary.total,
        error: None,
    })
}

impl StorageHost for StorageBridge {
    fn cookies(&self, cx: &mut App) -> Call<CookieList> {
        let browser = self.browser.clone();
        cx.background_spawn(async move {
            let url = browser.page().await?.url;
            let origin = url::Url::parse(&url)
                .ok()
                .map(|u| u.origin().ascii_serialization());
            let cookies = browser.cookies(&url).await?;
            Ok(CookieList {
                cookies: cookies.iter().map(cookie_to_row).collect(),
                origin: origin.or(Some(url)),
            })
        })
    }

    fn cookie_set(&self, params: CookieSetParams, cx: &mut App) -> Call<bool> {
        let browser = self.browser.clone();
        cx.background_spawn(async move {
            let host = |u: &str| url::Url::parse(u).ok()?.host_str().map(str::to_owned);
            let domain = params
                .domain
                .clone()
                .or_else(|| params.url.as_deref().and_then(host))
                .ok_or("a cookie needs a domain or url")?;
            let path = params.path.clone().unwrap_or_else(|| "/".to_owned());
            let url = params
                .url
                .clone()
                .unwrap_or_else(|| cookie_url(&domain, &path));
            let cookie = Cookie {
                name: params.name,
                value: params.value,
                domain,
                path,
                expires: params.expires,
                http_only: params.http_only.unwrap_or(false),
                secure: params.secure.unwrap_or(false),
                same_site: params.same_site,
            };
            browser.set_cookie(&url, cookie).await?;
            Ok(true)
        })
    }

    fn cookie_delete(&self, params: CookieDeleteParams, cx: &mut App) -> Call<bool> {
        let browser = self.browser.clone();
        cx.background_spawn(async move {
            let url = match (&params.url, &params.domain) {
                (Some(url), _) => url.clone(),
                (None, Some(domain)) => cookie_url(domain, params.path.as_deref().unwrap_or("/")),
                (None, None) => return Err("a cookie delete needs a url or domain".to_owned()),
            };
            browser.delete_cookie(&url, &params.name).await?;
            Ok(true)
        })
    }

    fn storage_get(&self, _: StorageArea, _: &mut App) -> Call<Vec<(String, String)>> {
        // shortcut: localStorage/sessionStorage via Browser::evaluate not wired;
        // the cookie tab + import are the ported feature. Upgrade when the port
        // reaches the storage tabs.
        Task::ready(Ok(Vec::new()))
    }

    fn storage_set(&self, _: StorageArea, _: String, _: String, _: &mut App) -> Call<bool> {
        Task::ready(Err(NOT_YET.to_owned()))
    }

    fn storage_delete(&self, _: StorageArea, _: String, _: &mut App) -> Call<bool> {
        Task::ready(Err(NOT_YET.to_owned()))
    }

    fn storage_clear(&self, _: StorageArea, _: &mut App) -> Call<bool> {
        Task::ready(Err(NOT_YET.to_owned()))
    }

    fn persistence_get(&self, _: &mut App) -> Call<Persistence> {
        Task::ready(Ok(Persistence::default()))
    }

    fn persistence_set(&self, _: bool, _: &mut App) -> Call<bool> {
        Task::ready(Err(NOT_YET.to_owned()))
    }

    fn persistence_snapshot(&self, _: &mut App) -> Call<u64> {
        Task::ready(Err(NOT_YET.to_owned()))
    }

    fn browsers(&self, _: &mut App) -> Call<Vec<BrowserInfo>> {
        // Profiles beyond Default are not enumerated yet.
        let chrome = chrome_installed().then(|| BrowserInfo {
            id: "chrome".to_owned(),
            name: "Chrome".to_owned(),
            profiles: vec![BrowserProfileInfo {
                id: "Default".to_owned(),
                name: "Default".to_owned(),
            }],
        });
        Task::ready(Ok(chrome.into_iter().collect()))
    }

    fn browser_import(
        &self,
        options: BrowserImportOptions,
        cx: &mut App,
    ) -> Call<BrowserImportResult> {
        if options.browser != "chrome" {
            return Task::ready(Err(format!(
                "only Chrome import is supported yet, not \"{}\"",
                options.browser
            )));
        }
        cx.background_spawn(import_chrome(
            self.browser.clone(),
            options.profile,
            options.hosts,
        ))
    }

    fn download_ask(&self, _: &mut App) -> Call<bool> {
        Task::ready(Ok(false))
    }

    fn set_download_ask(&self, _: bool, _: &mut App) -> Call<bool> {
        Task::ready(Err(NOT_YET.to_owned()))
    }

    fn dialog_settings(&self, _: &mut App) -> Call<DialogSettings> {
        Task::ready(Ok(DialogSettings::default()))
    }

    fn set_dialog_auto_dismiss(&self, _: bool, _: &mut App) -> Call<bool> {
        Task::ready(Err(NOT_YET.to_owned()))
    }

    fn clear_dialog_history(&self, _: &mut App) -> Call<bool> {
        Task::ready(Err(NOT_YET.to_owned()))
    }
}
