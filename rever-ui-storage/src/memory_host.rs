//! An in-memory [`StorageHost`]: a cookie jar and two storages that really take the
//! panel's edits. The preview and the view tests run the panel against it.

use std::cell::RefCell;

use gpui::{App, Task};

use crate::cookies_panel::{
    BrowserImportOptions, BrowserImportResult, BrowserInfo, Call, CookieDeleteParams, CookieList,
    CookieRow, CookieSetParams, DialogSettings, Persistence, StorageArea, StorageHost,
};

#[derive(Default)]
pub struct MemoryState {
    pub cookies: Vec<CookieRow>,
    pub origin: Option<String>,
    pub local: Vec<(String, String)>,
    pub session: Vec<(String, String)>,
    pub persistence: Persistence,
    pub ask_where_to_save: bool,
    pub dialog: DialogSettings,
    pub browsers: Vec<BrowserInfo>,
    /// The cookies each browser holds: (browser id, cookie).
    pub importable: Vec<(String, CookieRow)>,
    /// The last import the panel asked for.
    pub last_import: Option<BrowserImportOptions>,
    /// An import never answers while this is set.
    pub hold_import: bool,
    /// Every call fails with this message while it is set.
    pub fail: Option<String>,
}

impl MemoryState {
    fn entries(&mut self, area: StorageArea) -> &mut Vec<(String, String)> {
        match area {
            StorageArea::Local => &mut self.local,
            StorageArea::Session => &mut self.session,
        }
    }

    /// Replace the cookie with the same name, domain and path, or add it.
    fn put_cookie(&mut self, cookie: CookieRow) {
        let same = |held: &CookieRow| {
            held.name == cookie.name && held.domain == cookie.domain && held.path == cookie.path
        };
        match self.cookies.iter_mut().find(|held| same(held)) {
            Some(held) => *held = cookie,
            None => self.cookies.push(cookie),
        }
    }
}

#[derive(Default)]
pub struct MemoryStorageHost {
    pub state: RefCell<MemoryState>,
}

impl MemoryStorageHost {
    pub fn new(state: MemoryState) -> Self {
        Self {
            state: RefCell::new(state),
        }
    }

    fn call<T: 'static>(&self, run: impl FnOnce(&mut MemoryState) -> T) -> Call<T> {
        let mut state = self.state.borrow_mut();
        Task::ready(match state.fail.clone() {
            Some(message) => Err(message),
            None => Ok(run(&mut state)),
        })
    }
}

impl StorageHost for MemoryStorageHost {
    fn cookies(&self, _: &mut App) -> Call<CookieList> {
        self.call(|state| CookieList {
            cookies: state.cookies.clone(),
            origin: state.origin.clone(),
        })
    }

    fn cookie_set(&self, params: CookieSetParams, _: &mut App) -> Call<bool> {
        self.call(|state| {
            let host = |url: &String| url::Url::parse(url).ok()?.host_str().map(str::to_string);
            let Some(domain) = params.domain.or_else(|| params.url.as_ref().and_then(host)) else {
                return false;
            };
            state.put_cookie(CookieRow {
                name: params.name,
                value: params.value,
                domain,
                path: params.path.unwrap_or_else(|| "/".to_string()),
                // CDP's way of saying "session cookie".
                expires: Some(params.expires.unwrap_or(-1.0)),
                secure: params.secure,
                http_only: params.http_only,
                same_site: params.same_site,
            });
            true
        })
    }

    fn cookie_delete(&self, params: CookieDeleteParams, _: &mut App) -> Call<bool> {
        self.call(|state| {
            state.cookies.retain(|cookie| {
                cookie.name != params.name
                    || params.domain.as_ref().is_some_and(|d| *d != cookie.domain)
                    || params.path.as_ref().is_some_and(|p| *p != cookie.path)
            });
            true
        })
    }

    fn storage_get(&self, area: StorageArea, _: &mut App) -> Call<Vec<(String, String)>> {
        self.call(|state| state.entries(area).clone())
    }

    fn storage_set(
        &self,
        area: StorageArea,
        key: String,
        value: String,
        _: &mut App,
    ) -> Call<bool> {
        self.call(|state| {
            let entries = state.entries(area);
            match entries.iter_mut().find(|(held, _)| *held == key) {
                Some(entry) => entry.1 = value,
                None => entries.push((key, value)),
            }
            true
        })
    }

    fn storage_delete(&self, area: StorageArea, key: String, _: &mut App) -> Call<bool> {
        self.call(|state| {
            state.entries(area).retain(|(held, _)| *held != key);
            true
        })
    }

    fn storage_clear(&self, area: StorageArea, _: &mut App) -> Call<bool> {
        self.call(|state| {
            state.entries(area).clear();
            true
        })
    }

    fn persistence_get(&self, _: &mut App) -> Call<Persistence> {
        self.call(|state| state.persistence)
    }

    fn persistence_set(&self, enabled: bool, _: &mut App) -> Call<bool> {
        self.call(|state| {
            state.persistence.enabled = enabled;
            enabled
        })
    }

    fn persistence_snapshot(&self, _: &mut App) -> Call<u64> {
        self.call(|state| {
            state.persistence.snapshot_count += 1;
            state.persistence.snapshot_count
        })
    }

    fn browsers(&self, _: &mut App) -> Call<Vec<BrowserInfo>> {
        self.call(|state| state.browsers.clone())
    }

    fn browser_import(
        &self,
        options: BrowserImportOptions,
        cx: &mut App,
    ) -> Call<BrowserImportResult> {
        if self.state.borrow().hold_import {
            self.state.borrow_mut().last_import = Some(options);
            return cx.spawn(async |_| std::future::pending().await);
        }
        self.call(|state| {
            let hosts = options.hosts.clone().unwrap_or_default();
            let wanted = |cookie: &CookieRow| {
                hosts.is_empty() || hosts.iter().any(|host| cookie.domain.contains(host))
            };
            let (mut imported, mut total) = (0, 0);
            for (browser, cookie) in state.importable.clone() {
                if browser != options.browser {
                    continue;
                }
                total += 1;
                if wanted(&cookie) {
                    imported += 1;
                    state.put_cookie(cookie);
                }
            }
            state.last_import = Some(options);
            BrowserImportResult {
                ok: true,
                imported,
                skipped: total - imported,
                undecryptable: 0,
                total,
                error: None,
            }
        })
    }

    fn download_ask(&self, _: &mut App) -> Call<bool> {
        self.call(|state| state.ask_where_to_save)
    }

    fn set_download_ask(&self, enabled: bool, _: &mut App) -> Call<bool> {
        self.call(|state| {
            state.ask_where_to_save = enabled;
            enabled
        })
    }

    fn dialog_settings(&self, _: &mut App) -> Call<DialogSettings> {
        self.call(|state| state.dialog.clone())
    }

    fn set_dialog_auto_dismiss(&self, enabled: bool, _: &mut App) -> Call<bool> {
        self.call(|state| {
            state.dialog.auto_dismiss = enabled;
            enabled
        })
    }

    fn clear_dialog_history(&self, _: &mut App) -> Call<bool> {
        self.call(|state| {
            state.dialog.history.clear();
            true
        })
    }
}
