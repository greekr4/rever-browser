//! Profiles and proxy windows.
//!
//! The engine keeps one cookie jar and reads one proxy per process, so neither can
//! differ between the tabs of a window. A profile is therefore a window of its own: a
//! second process of this app with its own data folder, and a proxy is given to such a
//! window when it starts. Everything in it (cookies, storage, cache) is apart from the
//! other windows, which a per-tab partition in this engine could not promise.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rever_tools_api::tool::Cookie;
use rever_ui_setup::host::{BrowserProfile, ProfileKind};
use rever_ui_setup::profiles::DEFAULT_PROFILE_ID;
use rever_ui_state::tabs::{ProxyConfig, ProxyScheme};
use url::Url;

/// The proxy of this window's process, as the engine takes it; empty for none.
pub(crate) const PROXY_ENV: &str = "REVER_PROXY";
/// The name a profile window shows.
pub(crate) const LABEL_ENV: &str = "REVER_WINDOW_LABEL";
const EPHEMERAL_ENV: &str = "REVER_EPHEMERAL";
const DATA_DIR_ENV: &str = "REVER_USER_DATA_DIR";

/// Written by the launcher: the process that owns a window's data folder.
const OWNER_FILE: &str = "owner.pid";
/// Written by a window that wants to start again with another proxy.
const NEXT_PROXY_FILE: &str = "next-proxy";
const EPHEMERAL_PREFIX: &str = "ephemeral-";
const PROFILES_FILE: &str = "profiles.json";

/// Runs a window and stays until it is closed for good: `sh -c LAUNCHER sh <app> [url]`.
/// A window that left a `next-proxy` file starts again with that proxy and its own
/// tabs; the wait for the old process to end is what keeps two from sharing a folder.
/// A folder marked ephemeral is deleted once its window is gone.
const LAUNCHER: &str = r#"
dir=$REVER_USER_DATA_DIR
mkdir -p "$dir" && echo $$ > "$dir/owner.pid"
while :; do
  "$@"
  [ -f "$dir/next-proxy" ] || break
  REVER_PROXY=$(cat "$dir/next-proxy")
  export REVER_PROXY
  rm -f "$dir/next-proxy"
  set -- "$1"
done
rm -f "$dir/owner.pid"
if [ -n "$REVER_EPHEMERAL" ]; then rm -rf "$dir"; fi
"#;

/// The proxy as the engine takes it. Only a plain HTTP proxy: the engine opens a
/// CONNECT tunnel and can speak neither TLS nor SOCKS to the proxy itself.
pub(crate) fn proxy_uri(proxy: &ProxyConfig) -> Result<String, String> {
    if proxy.scheme != ProxyScheme::Http {
        return Err("only HTTP proxies work in this engine".to_owned());
    }
    let mut url = Url::parse(&format!("http://{}:{}", proxy.host.trim(), proxy.port))
        .map_err(|error| format!("not a proxy address: {error}"))?;
    let user = proxy.username.as_deref().unwrap_or_default();
    if !user.is_empty() {
        let password = proxy.password.as_deref().filter(|word| !word.is_empty());
        if url.set_username(user).is_err() || url.set_password(password).is_err() {
            return Err("the proxy address cannot carry a user name".to_owned());
        }
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// The reverse of [`proxy_uri`], for the window that was started with one.
pub(crate) fn proxy_from_uri(uri: &str) -> Option<ProxyConfig> {
    let url = Url::parse(uri).ok()?;
    let decode = |text: &str| {
        percent_encoding::percent_decode_str(text)
            .decode_utf8_lossy()
            .into_owned()
    };
    Some(ProxyConfig {
        enabled: true,
        scheme: ProxyScheme::Http,
        host: url.host_str()?.to_owned(),
        port: url.port_or_known_default()?,
        username: Some(decode(url.username())).filter(|user| !user.is_empty()),
        password: url.password().map(decode),
    })
}

/// What this process's window is: `None` for the app's first window.
pub(crate) struct ThisWindow {
    /// The profile's name; empty for a window the proxy button opened.
    pub label: String,
    pub proxy: Option<ProxyConfig>,
}

pub(crate) fn this_window() -> Option<ThisWindow> {
    // The launcher always sets it, to "1" or to nothing.
    std::env::var(EPHEMERAL_ENV).ok()?;
    let proxy = std::env::var(PROXY_ENV)
        .ok()
        .and_then(|uri| proxy_from_uri(&uri));
    Some(ThisWindow {
        label: std::env::var(LABEL_ENV).unwrap_or_default(),
        proxy,
    })
}

/// The proxy the engine starts with, from the window's environment.
pub(crate) fn engine_proxy() -> Option<String> {
    std::env::var(PROXY_ENV).ok().filter(|uri| !uri.is_empty())
}

fn alive(pid: &str) -> bool {
    !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

/// Whether a window is running on this data folder.
pub(crate) fn is_open(folder: &Path) -> bool {
    std::fs::read_to_string(folder.join(OWNER_FILE)).is_ok_and(|pid| alive(pid.trim()))
}

/// A window to open: a process of this app on its own data folder.
pub(crate) struct WindowSpec {
    pub folder: PathBuf,
    pub url: String,
    /// As [`proxy_uri`] gives it.
    pub proxy: Option<String>,
    /// The folder is deleted when the window is closed.
    pub ephemeral: bool,
    pub label: String,
}

fn launch(app: &Path, spec: &WindowSpec) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(LAUNCHER)
        .arg("sh")
        .arg(app)
        .arg(&spec.url)
        .env(DATA_DIR_ENV, &spec.folder)
        .env(PROXY_ENV, spec.proxy.as_deref().unwrap_or_default())
        .env(EPHEMERAL_ENV, if spec.ephemeral { "1" } else { "" })
        .env(LABEL_ENV, &spec.label)
        .stdin(Stdio::null());
    command
}

/// Open the window. Two windows on one folder would overwrite each other's cookies
/// and storage, so a profile whose window is open is not opened again.
pub(crate) fn open_window(app: &Path, spec: &WindowSpec) -> Result<(), String> {
    if !cfg!(unix) {
        return Err("profile windows are not available on this system yet".to_owned());
    }
    if is_open(&spec.folder) {
        return Err(format!("the window of {} is already open", spec.label));
    }
    let mut child = launch(app, spec)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not open the window: {error}"))?;
    // Reaped when the window closes, so no dead process is left behind.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Ask the launcher to start this window again with another proxy (`None`: without
/// one). The caller quits the app; its tabs come back from its own storage.
pub(crate) fn restart_with_proxy(folder: &Path, proxy: Option<&str>) -> Result<(), String> {
    if !is_open(folder) {
        return Err("this window cannot change its proxy: open a proxy window".to_owned());
    }
    let file = folder.join(NEXT_PROXY_FILE);
    std::fs::write(&file, proxy.unwrap_or_default())
        .map_err(|error| format!("the proxy was not saved: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // It can hold the proxy's password until the launcher reads it.
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// A folder for a window that leaves nothing behind.
pub(crate) fn ephemeral_folder(profiles_dir: &Path, stamp: u64) -> PathBuf {
    profiles_dir.join(format!("{EPHEMERAL_PREFIX}{stamp}"))
}

/// Delete the folders of ephemeral windows that ended without their launcher (a crash,
/// a power cut): what an incognito window held must not stay on the disk.
pub(crate) fn sweep_ephemeral(profiles_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(profiles_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let ephemeral = entry
            .file_name()
            .to_string_lossy()
            .starts_with(EPHEMERAL_PREFIX);
        if ephemeral && !is_open(&entry.path()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Cookies brought from another browser for a profile whose window is not open yet:
/// each process has its own jar, so the window takes them in when it starts.
const HANDOFF_FILE: &str = "import-cookies.json";

pub(crate) fn leave_cookies(folder: &Path, cookies: &[Cookie]) -> Result<(), String> {
    let rows: Vec<serde_json::Value> = cookies
        .iter()
        .map(|cookie| {
            serde_json::json!({
                "name": cookie.name, "value": cookie.value, "domain": cookie.domain,
                "path": cookie.path, "expires": cookie.expires,
                "httpOnly": cookie.http_only, "secure": cookie.secure,
            })
        })
        .collect();
    let file = folder.join(HANDOFF_FILE);
    std::fs::create_dir_all(folder)
        .and_then(|()| std::fs::write(&file, serde_json::Value::from(rows).to_string()))
        .map_err(|error| format!("the cookies were not handed over: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// The cookies left for this window, if any; the file is gone once they are read.
pub(crate) fn take_cookies(folder: &Path) -> Vec<Cookie> {
    let file = folder.join(HANDOFF_FILE);
    let Ok(text) = std::fs::read_to_string(&file) else {
        return Vec::new();
    };
    let _ = std::fs::remove_file(&file);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
    let text =
        |row: &serde_json::Value, key: &str| row[key].as_str().unwrap_or_default().to_owned();
    rows.iter()
        .map(|row| Cookie {
            name: text(row, "name"),
            value: text(row, "value"),
            domain: text(row, "domain"),
            path: text(row, "path"),
            expires: row["expires"].as_f64(),
            http_only: row["httpOnly"].as_bool().unwrap_or(false),
            secure: row["secure"].as_bool().unwrap_or(false),
            same_site: None,
        })
        .collect()
}

/// The named profiles: the saved ones, and the incognito ones of this run.
pub(crate) struct Profiles {
    base: PathBuf,
    saved: Vec<BrowserProfile>,
    incognito: Vec<BrowserProfile>,
}

impl Profiles {
    /// `base` is the data folder of the app's first window.
    pub(crate) fn load(base: &Path) -> Self {
        let saved = std::fs::read_to_string(base.join(PROFILES_FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            base: base.to_owned(),
            saved,
            incognito: Vec::new(),
        }
    }

    pub(crate) fn profiles_dir(&self) -> PathBuf {
        self.base.join("profiles")
    }

    /// Default first, as the menu lists them.
    pub(crate) fn list(&self) -> Vec<BrowserProfile> {
        let default = BrowserProfile {
            id: DEFAULT_PROFILE_ID.to_owned(),
            name: "Default".to_owned(),
            kind: ProfileKind::Persistent,
            partition: String::new(),
            source: None,
        };
        std::iter::once(default)
            .chain(self.saved.iter().cloned())
            .chain(self.incognito.iter().cloned())
            .collect()
    }

    fn save(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&self.saved).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&self.base)
            .and_then(|()| std::fs::write(self.base.join(PROFILES_FILE), text))
            .map_err(|error| format!("the profile list was not saved: {error}"))
    }

    /// `stamp` tells this profile's id and folder from every other one's.
    pub(crate) fn create(
        &mut self,
        name: &str,
        kind: ProfileKind,
        source: Option<String>,
        stamp: u64,
    ) -> Result<BrowserProfile, String> {
        let count = self.saved.len() + self.incognito.len() + 1;
        let name = match (name.trim(), kind) {
            ("", ProfileKind::Persistent) => format!("Profile {count}"),
            ("", ProfileKind::Incognito) => "Incognito".to_owned(),
            (name, _) => name.to_owned(),
        };
        let profile = BrowserProfile {
            id: format!("p{stamp}"),
            name,
            kind,
            partition: format!("p{stamp}"),
            source,
        };
        match kind {
            ProfileKind::Persistent => {
                self.saved.push(profile.clone());
                if let Err(error) = self.save() {
                    self.saved.pop();
                    return Err(error);
                }
            }
            ProfileKind::Incognito => self.incognito.push(profile.clone()),
        }
        Ok(profile)
    }

    /// Where a saved profile keeps its data; an incognito one has no folder of its own.
    pub(crate) fn folder(&self, profile: &BrowserProfile) -> Option<PathBuf> {
        (profile.kind == ProfileKind::Persistent && profile.id != DEFAULT_PROFILE_ID)
            .then(|| self.profiles_dir().join(&profile.partition))
    }

    pub(crate) fn find(&self, id: &str) -> Option<BrowserProfile> {
        self.list().into_iter().find(|profile| profile.id == id)
    }

    /// Forget the profile and delete what it stored.
    pub(crate) fn remove(&mut self, id: &str) -> Result<(), String> {
        if let Some(at) = self.incognito.iter().position(|profile| profile.id == id) {
            self.incognito.remove(at);
            return Ok(());
        }
        let at = self
            .saved
            .iter()
            .position(|profile| profile.id == id)
            .ok_or("no such profile")?;
        let folder = self.profiles_dir().join(&self.saved[at].partition);
        if is_open(&folder) {
            return Err("close the profile's window first".to_owned());
        }
        let removed = self.saved.remove(at);
        if let Err(error) = self.save() {
            self.saved.insert(at, removed);
            return Err(error);
        }
        let _ = std::fs::remove_dir_all(folder);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(scheme: ProxyScheme, user: Option<&str>, password: Option<&str>) -> ProxyConfig {
        ProxyConfig {
            enabled: true,
            scheme,
            host: "10.0.0.5".to_owned(),
            port: 8080,
            username: user.map(str::to_owned),
            password: password.map(str::to_owned),
        }
    }

    /// A folder of this test's own, under the build's temp directory.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rever-windows-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The pid of a process that has ended.
    fn dead_pid() -> String {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id().to_string();
        child.wait().unwrap();
        pid
    }

    #[test]
    fn http_프록시는_엔진이_받는_주소로_바꾼다() {
        assert_eq!(
            proxy_uri(&proxy(ProxyScheme::Http, None, None)).unwrap(),
            "http://10.0.0.5:8080"
        );
    }

    #[test]
    fn 사용자_이름과_비밀번호는_주소에_안전하게_넣고_다시_읽을_수_있다() {
        let with_auth = proxy(ProxyScheme::Http, Some("me@corp"), Some("p:w/d"));
        let uri = proxy_uri(&with_auth).unwrap();
        assert_eq!(uri, "http://me%40corp:p%3Aw%2Fd@10.0.0.5:8080");
        assert_eq!(proxy_from_uri(&uri), Some(with_auth));
    }

    #[test]
    fn http가_아닌_프록시와_주소가_아닌_호스트는_거절한다() {
        assert!(proxy_uri(&proxy(ProxyScheme::Socks5, None, None)).is_err());
        assert!(proxy_uri(&proxy(ProxyScheme::Https, None, None)).is_err());
        let mut bad = proxy(ProxyScheme::Http, None, None);
        bad.host = "not a host".to_owned();
        assert!(proxy_uri(&bad).is_err());
    }

    #[test]
    fn 영구_프로필은_저장되어_다시_읽히고_시크릿_프로필은_저장되지_않는다() {
        let base = scratch("profiles");
        let mut profiles = Profiles::load(&base);
        let work = profiles
            .create("Work", ProfileKind::Persistent, None, 1)
            .unwrap();
        profiles
            .create("", ProfileKind::Incognito, None, 2)
            .unwrap();
        let names = |profiles: &Profiles| -> Vec<String> {
            profiles.list().into_iter().map(|p| p.name).collect()
        };
        assert_eq!(names(&profiles), ["Default", "Work", "Incognito"]);
        assert_eq!(
            profiles.folder(&work),
            Some(base.join("profiles").join("p1"))
        );

        assert_eq!(names(&Profiles::load(&base)), ["Default", "Work"]);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn 프로필을_지우면_목록과_데이터_폴더가_함께_없어진다() {
        let base = scratch("remove");
        let mut profiles = Profiles::load(&base);
        let work = profiles
            .create("Work", ProfileKind::Persistent, None, 7)
            .unwrap();
        let folder = profiles.folder(&work).unwrap();
        std::fs::create_dir_all(&folder).unwrap();

        profiles.remove(&work.id).unwrap();

        assert!(!folder.exists());
        assert_eq!(Profiles::load(&base).list().len(), 1);
        assert!(profiles.remove(DEFAULT_PROFILE_ID).is_err());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn 창이_열려_있는_프로필은_지우지_않는다() {
        let base = scratch("busy");
        let mut profiles = Profiles::load(&base);
        let work = profiles
            .create("Work", ProfileKind::Persistent, None, 3)
            .unwrap();
        let folder = profiles.folder(&work).unwrap();
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(OWNER_FILE), std::process::id().to_string()).unwrap();

        assert!(profiles.remove(&work.id).is_err());
        assert!(folder.exists());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn 주인_프로세스가_살아_있는_폴더만_열려_있다고_본다() {
        let folder = scratch("open");
        assert!(!is_open(&folder));
        std::fs::write(folder.join(OWNER_FILE), std::process::id().to_string()).unwrap();
        assert!(is_open(&folder));
        std::fs::write(folder.join(OWNER_FILE), dead_pid()).unwrap();
        assert!(!is_open(&folder));
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn 주인이_없는_임시_폴더만_치운다() {
        let dir = scratch("sweep");
        let (stale, live, saved) = (
            ephemeral_folder(&dir, 1),
            ephemeral_folder(&dir, 2),
            dir.join("p9"),
        );
        for folder in [&stale, &live, &saved] {
            std::fs::create_dir_all(folder).unwrap();
        }
        std::fs::write(stale.join(OWNER_FILE), dead_pid()).unwrap();
        std::fs::write(live.join(OWNER_FILE), std::process::id().to_string()).unwrap();

        sweep_ephemeral(&dir);

        assert!(!stale.exists());
        assert!(live.exists());
        assert!(saved.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A stand-in for the app: logs its proxy and arguments, and on its first run
    /// asks to start again with another proxy.
    #[cfg(unix)]
    fn fake_app(dir: &Path, log: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let app = dir.join("fake-app.sh");
        let script = format!(
            "#!/bin/sh\necho \"$REVER_PROXY|$*\" >> '{log}'\n\
             if [ ! -f '{log}.again' ]; then touch '{log}.again'; \
             printf 'http://second:2' > \"$REVER_USER_DATA_DIR/next-proxy\"; fi\n",
            log = log.display()
        );
        std::fs::write(&app, script).unwrap();
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
        app
    }

    #[cfg(unix)]
    #[test]
    fn 런처는_프록시를_바꿔_다시_띄우고_임시_폴더를_지운다() {
        let dir = scratch("launcher");
        let log = dir.join("runs.log");
        let app = fake_app(&dir, &log);
        let spec = WindowSpec {
            folder: ephemeral_folder(&dir, 5),
            url: "https://a.test/".to_owned(),
            proxy: Some("http://first:1".to_owned()),
            ephemeral: true,
            label: "Proxy".to_owned(),
        };

        assert!(launch(&app, &spec).status().unwrap().success());

        // 두 번째 실행은 새 프록시로, 주소 없이(자기 탭을 되살리도록) 뜬다.
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "http://first:1|https://a.test/\nhttp://second:2|\n"
        );
        assert!(!spec.folder.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn 런처는_영구_프로필의_폴더를_남기고_주인_표시만_지운다() {
        let dir = scratch("keep");
        let log = dir.join("runs.log");
        std::fs::write(format!("{}.again", log.display()), "").unwrap();
        let app = fake_app(&dir, &log);
        let spec = WindowSpec {
            folder: dir.join("p1"),
            url: "https://a.test/".to_owned(),
            proxy: None,
            ephemeral: false,
            label: "Work".to_owned(),
        };

        assert!(launch(&app, &spec).status().unwrap().success());

        assert_eq!(std::fs::read_to_string(&log).unwrap(), "|https://a.test/\n");
        assert!(spec.folder.exists());
        assert!(!spec.folder.join(OWNER_FILE).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn 넘겨_둔_쿠키는_한_번만_그대로_읽힌다() {
        let folder = scratch("handoff");
        let cookie = Cookie {
            name: "sid".to_owned(),
            value: "a=b; \"quoted\"".to_owned(),
            domain: ".a.test".to_owned(),
            path: "/".to_owned(),
            expires: Some(1_900_000_000.5),
            http_only: true,
            secure: true,
            same_site: None,
        };
        leave_cookies(&folder, std::slice::from_ref(&cookie)).unwrap();

        assert_eq!(take_cookies(&folder), [cookie]);
        assert!(take_cookies(&folder).is_empty());
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn 런처_밖에서_뜬_창은_프록시를_바꿔_다시_뜰_수_없다() {
        let folder = scratch("norestart");
        assert!(restart_with_proxy(&folder, Some("http://a:1")).is_err());
        std::fs::write(folder.join(OWNER_FILE), std::process::id().to_string()).unwrap();
        restart_with_proxy(&folder, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(folder.join(NEXT_PROXY_FILE)).unwrap(),
            ""
        );
        std::fs::remove_dir_all(folder).unwrap();
    }
}
