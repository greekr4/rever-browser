//! Port of `src/shared/endpoint-template.ts` — folds id-like path segments so
//! requests to the same endpoint group together.

fn is_hex(bytes: &[u8]) -> bool {
    bytes.iter().all(u8::is_ascii_hexdigit)
}

// Mirrors the three patterns of the TypeScript, which are ASCII-only: `\d`
// there never matches other scripts' digits.
fn is_id(seg: &str) -> bool {
    let b = seg.as_bytes();
    // ^\d+$
    b.iter().all(u8::is_ascii_digit)
        // ^[0-9a-f]{16,}$ (case-insensitive)
        || (b.len() >= 16 && is_hex(b))
        // ^[0-9a-f]{8}-[0-9a-f]{4}- (case-insensitive, prefix only)
        || (b.len() >= 14
            && b.get(..8).is_some_and(is_hex)
            && b.get(8) == Some(&b'-')
            && b.get(9..13).is_some_and(is_hex)
            && b.get(13) == Some(&b'-'))
}

/// /users/12/orders/9f8a-... → /users/:id/orders/:id
pub fn template_path(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            if !seg.is_empty() && is_id(seg) {
                ":id"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 숫자_세그먼트를_id로_바꾼다() {
        assert_eq!(template_path("/users/12/orders"), "/users/:id/orders");
        assert_eq!(template_path("/users/12/orders/0"), "/users/:id/orders/:id");
    }

    #[test]
    fn 열여섯_자_이상의_hex_세그먼트를_id로_바꾼다() {
        assert_eq!(template_path("/f/0123456789abcdef"), "/f/:id");
        assert_eq!(template_path("/f/0123456789ABCDEF00"), "/f/:id");
        // 15 hex characters is one short.
        assert_eq!(template_path("/f/0123456789abcde"), "/f/0123456789abcde");
        assert_eq!(template_path("/f/0123456789abcdeg"), "/f/0123456789abcdeg");
    }

    #[test]
    fn uuid처럼_시작하는_세그먼트를_id로_바꾼다() {
        assert_eq!(
            template_path("/o/9F8A1b2c-3d4e-5f60-7182-93a4b5c6d7e8/x"),
            "/o/:id/x"
        );
        // Only the `8 hex - 4 hex -` prefix is checked.
        assert_eq!(template_path("/o/9f8a1b2c-3d4e-anything"), "/o/:id");
        assert_eq!(template_path("/o/9f8a1b2c-3d4e"), "/o/9f8a1b2c-3d4e");
    }

    #[test]
    fn 빈_세그먼트와_일반_세그먼트는_그대로_둔다() {
        assert_eq!(template_path(""), "");
        assert_eq!(template_path("/"), "/");
        assert_eq!(template_path("//a//"), "//a//");
        assert_eq!(template_path("/v1/users/me"), "/v1/users/me");
        assert_eq!(template_path("/a/12b"), "/a/12b");
    }

    #[test]
    fn 아스키가_아닌_숫자는_id로_보지_않는다() {
        // JavaScript's \d is [0-9]; Arabic-Indic digits stay as they are.
        assert_eq!(template_path("/a/١٢٣"), "/a/١٢٣");
    }
}
