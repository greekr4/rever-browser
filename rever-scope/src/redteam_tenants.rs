//! Port of `src/main/mcp/redteam-tenants.ts` — red-team multi-tenant state.
//!
//! Confirming BOLA/IDOR (broken object-level authorization) needs a SECOND
//! identity: the scan registers a throwaway attacker account during auth
//! bootstrap and stashes its session Cookie here, so idor_probe can replay an
//! owner's resource request with the attacker's cookie and see whether the
//! server lets a different tenant read it.
//!
//! A plain string (the raw `Cookie:` header value for the alternate tenant).

use std::sync::Mutex;

use crate::{js_trim, lock};

static ALT_SESSION: Mutex<Option<String>> = Mutex::new(None);

pub fn set_alt_session(cookie: Option<&str>) {
    *lock(&ALT_SESSION) = cookie
        .map(js_trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string);
}

pub fn get_alt_session() -> Option<String> {
    lock(&ALT_SESSION).clone()
}

pub fn clear_alt_session() {
    *lock(&ALT_SESSION) = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isolated;

    #[test]
    fn 저장한_쿠키를_돌려준다() {
        let _clean = isolated(clear_alt_session);
        set_alt_session(Some("ats_token=B; other=1"));
        assert_eq!(get_alt_session().as_deref(), Some("ats_token=B; other=1"));
    }

    #[test]
    fn 공백이나_빈_값은_none으로_지운다() {
        let _clean = isolated(clear_alt_session);
        set_alt_session(Some("ats_token=B"));
        set_alt_session(Some("   "));
        assert_eq!(get_alt_session(), None);
    }

    #[test]
    fn clear로_비운다() {
        let _clean = isolated(clear_alt_session);
        set_alt_session(Some("x=1"));
        clear_alt_session();
        assert_eq!(get_alt_session(), None);
    }
}
