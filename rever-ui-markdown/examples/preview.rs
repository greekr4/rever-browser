//! Opens this crate's view alone with fixture data. See rever_ui_kit::preview.

use gpui::{Context, SharedString, Window, div, prelude::*, px};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};
use rever_ui_markdown::markdown;

const LEFT: &str = r#"# Reversing the login API

The page signs every request in **WebAssembly**, not in *Web Crypto*, so `crypto_trace`
shows nothing. ~~The token is static.~~ See [the bundle notes](https://example.com/notes)
or the bare link https://example.com/api/v2/sign_in?debug=1, then read on.
Questions go to dev@example.com, and the nonce rule has a footnote.[^nonce]
A hard break follows here.\
한국어 문장도 줄바꿈이 자연스럽게 되어야 하고, ***굵은 기울임***도 섞일 수 있습니다.

## Steps

1. Capture the `POST /api/login` request
2. Find the signer
   - search the bundle for `x-signature`
   - follow the call into `sign.wasm`
     - exported as `_sign(ptr, len)`
3. Replay it with a fresh nonce

### Checklist

- [x] request captured
- [x] signer located
- [ ] replay verified with a long item that wraps onto a second line so the checkbox stays on the first one

- a loose item

- another loose item

#### Code

```ts
async function sign(body: string, nonce: number): Promise<string> {
	const digest = await wasm.exports._sign(encode(body), body.length, nonce) // a long line that scrolls sideways
  return hex(digest)
}
```

##### h5 heading

###### h6 heading

[^nonce]: The server rejects a nonce older than **30 s**.
"#;

const RIGHT: &str = r#"> **Note** the nonce is a millisecond timestamp.
> A second paragraph line inside the quote, with `code` and a [link](/relative).
>
> - quoted item
> > nested quote

| Header | Type | Example value | Notes |
|:-------|:----:|--------------:|-------|
| `x-signature` | hex | `9f2c…e1` | HMAC over the body, computed in WASM |
| `x-nonce` | int | 1731465600000 | **must** be within 30 s of server time |
| `x-device` | uuid | — | optional |

---

Raw <b>HTML</b> stays text, and an image is its alt text: ![diagram](https://example.com/d.png).

<div class="note">block html</div>

| # | Method | Path | Status | Type | Size | Time |
|--:|--------|------|-------:|------|-----:|-----:|
| 1 | POST | /api/login | 200 | json | 512 | 84 ms |
| 2 | GET | /api/me | 401 | json | 96 | 31 ms |

An unsafe [javascript link](javascript:alert(1)) reports nothing.

```
unclosed fence while streaming
  keeps rendering as code"#;

const DIM: &str = "Thinking about **which** request carries the token… `list_requests` next.";

struct Preview {
    theme: Theme,
    clicked: SharedString,
}

impl Render for Preview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let column = |id: &'static str, text: &'static str, cx: &mut Context<Self>| {
            let this = cx.weak_entity();
            div().flex_1().min_w_0().child(
                markdown(text, &theme)
                    .id(id)
                    .lang(Lang::detect())
                    .on_link(move |url, _, cx| {
                        let url = url.to_owned();
                        let _ = this.update(cx, |preview, cx| {
                            preview.clicked = format!("clicked: {url}").into();
                            cx.notify();
                        });
                    }),
            )
        };
        div()
            .id("preview")
            .size_full()
            .overflow_y_scroll()
            .bg(theme.bg)
            .font_family(".SystemUIFont")
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(24.0))
                    .child(column("left", LEFT, cx))
                    .child(
                        // As narrow as the chat panel, where tables and code must cope.
                        div()
                            .w(px(380.0))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .gap(px(12.0))
                            .child(column("right", RIGHT, cx))
                            .child(markdown(DIM, &theme).id("dim").dim(true)),
                    ),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_dim)
                    .child(self.clicked.clone()),
            )
    }
}

fn main() {
    rever_ui_kit::preview::run(1100.0, 1000.0, |window, cx| {
        let theme = Theme::resolve(ThemeMode::from_env(), window.appearance());
        cx.new(|_| Preview {
            theme,
            clicked: "click a link".into(),
        })
    });
}
