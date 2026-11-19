//! Port of `src/shared/tool-profile-line.ts` — the one line appended to every user turn.
//!
//! The system prompt is sent only once, so the current profile rides on each turn.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolProfileId {
    Browse,
    Reverse,
    Probe,
}

impl ToolProfileId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Browse => "browse",
            Self::Reverse => "reverse",
            Self::Probe => "probe",
        }
    }

    /// Exact, case-sensitive match on the three ids; anything else is `None`.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "browse" => Some(Self::Browse),
            "reverse" => Some(Self::Reverse),
            "probe" => Some(Self::Probe),
            _ => None,
        }
    }
}

impl fmt::Display for ToolProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub fn tool_profile_turn_line(profile: ToolProfileId) -> String {
    format!(
        "[tool-profile: {profile}] Tools outside this profile are not loaded. Call tool_profile_set to switch between browse and reverse. Probe is selected by the user."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 턴_한_줄은_프로파일_이름만_바뀐다() {
        assert_eq!(
            tool_profile_turn_line(ToolProfileId::Browse),
            "[tool-profile: browse] Tools outside this profile are not loaded. Call tool_profile_set to switch between browse and reverse. Probe is selected by the user."
        );
        assert!(
            tool_profile_turn_line(ToolProfileId::Reverse).starts_with("[tool-profile: reverse] ")
        );
        assert!(tool_profile_turn_line(ToolProfileId::Probe).starts_with("[tool-profile: probe] "));
    }

    #[test]
    fn 프로파일_id는_소문자_문자열로_직렬화되고_대소문자를_구분해_읽는다() {
        for profile in [
            ToolProfileId::Browse,
            ToolProfileId::Reverse,
            ToolProfileId::Probe,
        ] {
            let json = serde_json::to_string(&profile).unwrap();
            assert_eq!(json, format!("\"{}\"", profile.as_str()));
            assert_eq!(
                serde_json::from_str::<ToolProfileId>(&json).unwrap(),
                profile
            );
            assert_eq!(ToolProfileId::parse(profile.as_str()), Some(profile));
        }
        assert_eq!(ToolProfileId::parse("Browse"), None);
        assert_eq!(ToolProfileId::parse(" browse"), None);
        assert_eq!(ToolProfileId::parse(""), None);
        assert!(serde_json::from_str::<ToolProfileId>("\"full\"").is_err());
    }
}
