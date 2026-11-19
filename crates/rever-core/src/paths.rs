//! Follows `userDataDir` in bin/revb.mjs: where the app keeps its data, computed
//! the way Electron's `app.getPath('userData')` does, so both apps share one folder.

use std::path::PathBuf;

const APP_DIR: &str = "rever-browser";

/// `env` looks up an environment variable; `os` is `std::env::consts::OS`.
pub fn user_data_dir(env: impl Fn(&str) -> Option<String>, os: &str, home: &str) -> PathBuf {
    let set = |name: &str| env(name).filter(|value| !value.is_empty());
    if let Some(dir) = set("REVER_USER_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let app_data = match os {
        "macos" => PathBuf::from(home)
            .join("Library")
            .join("Application Support"),
        "windows" => set("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(home).join("AppData").join("Roaming")),
        _ => set("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(home).join(".config")),
    };
    app_data.join(APP_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 환경변수가_있으면_그_경로를_쓴다() {
        let env = |name: &str| (name == "REVER_USER_DATA_DIR").then(|| "/tmp/e2e".to_owned());
        assert_eq!(user_data_dir(env, "macos", "/h"), PathBuf::from("/tmp/e2e"));
    }

    #[test]
    fn 운영체제별_기본_위치를_쓴다() {
        let none = |_: &str| None;
        assert_eq!(
            user_data_dir(none, "macos", "/h"),
            PathBuf::from("/h/Library/Application Support/rever-browser")
        );
        assert_eq!(
            user_data_dir(none, "linux", "/h"),
            PathBuf::from("/h/.config/rever-browser")
        );
        let appdata = |name: &str| (name == "APPDATA").then(|| "/ad".to_owned());
        assert_eq!(
            user_data_dir(appdata, "windows", "/h"),
            PathBuf::from("/ad/rever-browser")
        );
    }
}
