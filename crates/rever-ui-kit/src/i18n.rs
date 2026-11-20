//! Interface strings in English and Korean.
//!
//! Port of `src/renderer/src/stores/i18n.ts`: the dictionaries are the Electron app's
//! own (generated into locales.rs); a missing Korean entry falls back to English.

use crate::locales::STRINGS;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Ko,
}

/// Strings the Rust app added: the Electron dictionaries (`locales.rs`) never had them.
const PORT_STRINGS: &[(&str, &str, &str)] = &[
    ("chat.tools.ran", "Ran {n} tools", "도구 {n}개 실행함"),
    ("chat.tools.running", "Running {n} tools", "도구 {n}개 실행 중"),
    ("chat.working", "Working…", "작업 중…"),
    (
        "port.panelNotYet",
        "This panel is not available in the Rust app yet.",
        "이 패널은 아직 Rust 앱으로 옮겨지지 않았습니다.",
    ),
];

impl Lang {
    /// `REVER_LANG=ko|en` wins; otherwise the OS locale decides (Korean → ko, else
    /// English), as `detectLang` does with `navigator.language`.
    pub fn detect() -> Self {
        let forced = std::env::var("REVER_LANG")
            .unwrap_or_default()
            .to_lowercase();
        Self::from_locale(&forced)
            .or_else(|| {
                Self::from_locale(&sys_locale::get_locale().unwrap_or_default().to_lowercase())
            })
            .unwrap_or(Lang::En)
    }

    fn from_locale(locale: &str) -> Option<Self> {
        match locale.get(..2)? {
            "ko" => Some(Lang::Ko),
            "en" => Some(Lang::En),
            _ => None,
        }
    }

    /// The string for `key`, or the key itself when it is unknown.
    pub fn t(self, key: &'static str) -> &'static str {
        let lookup = |table: &'static [(&'static str, &'static str, &'static str)]| {
            table
                .binary_search_by(|(k, _, _)| (*k).cmp(key))
                .ok()
                .map(|index| table[index])
        };
        match lookup(STRINGS).or_else(|| lookup(PORT_STRINGS)) {
            Some((_, _, ko)) if self == Lang::Ko && !ko.is_empty() => ko,
            Some((_, en, _)) => en,
            None => key,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 언어에_맞는_문구를_주고_없는_키는_키를_돌려준다() {
        assert_eq!(Lang::En.t("panel.traffic"), "Traffic");
        assert_eq!(Lang::Ko.t("panel.traffic"), "트래픽");
        assert_eq!(
            Lang::Ko.t("port.panelNotYet"),
            "이 패널은 아직 Rust 앱으로 옮겨지지 않았습니다."
        );
        assert_eq!(Lang::Ko.t("no.such.key"), "no.such.key");
    }

    #[test]
    fn 사전은_키_순으로_정렬돼_있다() {
        // 이진 탐색의 전제.
        assert!(STRINGS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert!(PORT_STRINGS.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    #[test]
    fn 로캘_문자열에서_언어를_고른다() {
        assert_eq!(Lang::from_locale("ko_kr.utf-8"), Some(Lang::Ko));
        assert_eq!(Lang::from_locale("ko-kr"), Some(Lang::Ko));
        assert_eq!(Lang::from_locale("en_us.utf-8"), Some(Lang::En));
        assert_eq!(Lang::from_locale("ja_jp"), None);
        assert_eq!(Lang::from_locale(""), None);
    }
}
