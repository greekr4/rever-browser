//! Port of `src/renderer/src/components/PageEventToast.tsx` — bottom-right notices for
//! things a page did off-screen: a finished download (with a way to find the file) and
//! a dialog the app answered for the agent. The host calls
//! [`PageEventToast::page_event`] where the React component listened to
//! `window.rev.onPageEvent`.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, App, Context, Corner, Window, anchored, deferred, div, point,
    prelude::*, px, rgba,
};
use rever_ui_kit::i18n::Lang;
use serde::{Deserialize, Serialize};

use crate::ui::{LookFn, Tokens, Z_TOAST, css_ease, fill, shadow, text};

pub const SHOW_MS: u64 = 6000;
pub const MAX_VISIBLE: usize = 3;
/// `animation: page-event-in 0.2s ease`
const ENTER_MS: u64 = 200;
/// The dialog message is cut to this many UTF-16 units, the unit of a JavaScript string.
const MESSAGE_LIMIT: usize = 80;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Download {
    pub id: i64,
    pub filename: String,
    pub state: String,
    pub save_path: String,
}

/// Things a page did that the user should hear about (downloads, auto-answered dialogs).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PageEvent {
    Download {
        download: Download,
    },
    Dialog {
        #[serde(rename = "type")]
        dialog_type: String,
        message: String,
        accept: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub key: u64,
    pub event: PageEvent,
}

/// The visible notices, oldest first. Kept apart from the view so it can be tested
/// without a window.
#[derive(Default, Debug)]
pub struct PageEventQueue {
    seq: u64,
    items: Vec<Item>,
}

impl PageEventQueue {
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Add a notice, dropping the oldest ones beyond [`MAX_VISIBLE`]. The returned key
    /// is what [`Self::expire`] takes [`SHOW_MS`] later.
    pub fn push(&mut self, event: PageEvent) -> u64 {
        self.seq += 1;
        let keep_from = self.items.len().saturating_sub(MAX_VISIBLE - 1);
        self.items.drain(..keep_from);
        self.items.push(Item {
            key: self.seq,
            event,
        });
        self.seq
    }

    /// Remove the notice with `key`; false when it was already pushed out.
    pub fn expire(&mut self, key: u64) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.key != key);
        self.items.len() != before
    }
}

/// `message.length > 80 ? message.slice(0, 80) + '…' : message`, counting UTF-16 units
/// as JavaScript does. A character that straddles the limit is dropped whole, where
/// `slice` would leave half a surrogate pair.
pub fn clip_message(message: &str) -> String {
    if message.encode_utf16().count() <= MESSAGE_LIMIT {
        return message.to_owned();
    }
    let mut units = 0;
    let mut clipped: String = message
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= MESSAGE_LIMIT
        })
        .collect();
    clipped.push('…');
    clipped
}

/// The text of a notice, and the id of the download its "Show in folder" button
/// reveals (finished downloads only).
pub fn toast_content(lang: Lang, event: &PageEvent) -> (String, Option<i64>) {
    match event {
        PageEvent::Download { download } if download.state == "completed" => (
            fill(lang.t("toast.downloaded"), &[("name", &download.filename)]),
            Some(download.id),
        ),
        PageEvent::Download { download } => (
            fill(
                lang.t("toast.downloadFailed"),
                &[("name", &download.filename), ("state", &download.state)],
            ),
            None,
        ),
        PageEvent::Dialog {
            dialog_type,
            message,
            accept,
        } => (
            fill(
                lang.t("toast.dialogAnswered"),
                &[
                    ("type", dialog_type),
                    ("answer", if *accept { "OK" } else { "Cancel" }),
                    ("message", &clip_message(message)),
                ],
            ),
            None,
        ),
    }
}

pub struct PageEventToast {
    queue: PageEventQueue,
    /// `window.rev.downloads.reveal(id)`
    reveal: Rc<dyn Fn(i64, &mut Window, &mut App)>,
    look: LookFn,
}

impl PageEventToast {
    pub fn new(reveal: impl Fn(i64, &mut Window, &mut App) + 'static, look: LookFn) -> Self {
        Self {
            queue: PageEventQueue::default(),
            reveal: Rc::new(reveal),
            look,
        }
    }

    pub fn page_event(&mut self, event: PageEvent, cx: &mut Context<Self>) {
        let key = self.queue.push(event);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(SHOW_MS))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.queue.expire(key) {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

impl Render for PageEventToast {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Out of the host's layout flow: the stack places itself in the window.
        let root = div().absolute();
        if self.queue.items().is_empty() {
            return root;
        }
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let viewport = window.viewport_size();

        let toasts = self.queue.items().iter().map(|item| {
            let (message, reveal_id) = toast_content(look.lang, &item.event);
            let reveal = self.reveal.clone();
            let hover = theme.border_2;
            // `.page-event-toast`
            let key = item.key;
            text(12.0)
                .debug_selector(move || format!("page-event-toast-{key}"))
                // The stack's `max-width`; a box placed in the window has no parent
                // width to wrap against, so each notice carries the limit itself.
                .max_w(px(420.0))
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(7.0))
                .bg(theme.bg_bar)
                .border_1()
                .border_color(theme.border)
                .rounded(px(8.0))
                .shadow(vec![shadow(6.0, 24.0, rgba(0x0000004d))])
                .text_color(theme.text)
                .child(div().min_w_0().child(message))
                .when_some(reveal_id, |toast, id| {
                    toast.child(
                        // A plain `button`, with the size `.page-event-toast button` gives it.
                        div()
                            .id(("page-event-reveal", item.key))
                            .debug_selector(move || format!("page-event-reveal-{key}"))
                            .flex_none()
                            .px(px(8.0))
                            .py(px(2.0))
                            .rounded(px(4.0))
                            .bg(theme.surface_3)
                            .text_size(px(11.0))
                            .line_height(px(16.0))
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .on_click(move |_, window, cx| reveal(id, window, cx))
                            .child(look.lang.t("toast.showInFolder")),
                    )
                })
                .with_animation(
                    ("page-event-toast", item.key),
                    Animation::new(Duration::from_millis(ENTER_MS)).with_easing(css_ease),
                    // `page-event-in`: fade in while rising 6px into place.
                    |toast, progress| toast.opacity(progress).top(px(6.0 * (1.0 - progress))),
                )
        });

        // `.page-event-stack`
        let stack = div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(6.0))
            .children(toasts);

        root.child(
            deferred(
                anchored()
                    .anchor(Corner::BottomRight)
                    .position(point(viewport.width - px(16.0), viewport.height - px(44.0)))
                    .child(stack),
            )
            .with_priority(Z_TOAST),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn download(id: i64, state: &str) -> PageEvent {
        PageEvent::Download {
            download: Download {
                id,
                filename: "report.pdf".into(),
                state: state.into(),
                save_path: "/Users/me/Downloads/Rever/report.pdf".into(),
            },
        }
    }

    fn dialog(message: &str, accept: bool) -> PageEvent {
        PageEvent::Dialog {
            dialog_type: "confirm".into(),
            message: message.into(),
            accept,
        }
    }

    #[test]
    fn 알림은_최대_세_개까지만_보인다() {
        let mut queue = PageEventQueue::default();
        let keys: Vec<u64> = (0..5)
            .map(|id| queue.push(download(id, "completed")))
            .collect();
        assert_eq!(keys, vec![1, 2, 3, 4, 5]);
        assert_eq!(queue.items().len(), MAX_VISIBLE);
        // The oldest are dropped; the newest is last.
        assert_eq!(
            queue
                .items()
                .iter()
                .map(|item| item.key)
                .collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
    }

    #[test]
    fn 시간이_지난_알림만_지운다() {
        let mut queue = PageEventQueue::default();
        let first = queue.push(download(1, "completed"));
        let second = queue.push(dialog("Leave?", true));
        assert!(queue.expire(first));
        assert_eq!(queue.items().len(), 1);
        assert_eq!(queue.items()[0].key, second);
        assert_eq!(queue.items()[0].event, dialog("Leave?", true));
    }

    #[test]
    fn 이미_밀려난_알림의_만료는_아무것도_지우지_않는다() {
        let mut queue = PageEventQueue::default();
        let first = queue.push(download(1, "completed"));
        for id in 2..=4 {
            queue.push(download(id, "completed"));
        }
        assert!(!queue.expire(first));
        assert_eq!(queue.items().len(), 3);
        assert!(!queue.expire(999));
    }

    #[test]
    fn 키는_지워진_뒤에도_다시_쓰이지_않는다() {
        let mut queue = PageEventQueue::default();
        let first = queue.push(download(1, "completed"));
        queue.expire(first);
        assert_eq!(queue.push(download(2, "completed")), first + 1);
    }

    #[test]
    fn 끝난_다운로드는_파일_이름과_폴더_열기_버튼을_준다() {
        assert_eq!(
            toast_content(Lang::En, &download(7, "completed")),
            ("Downloaded report.pdf".to_owned(), Some(7))
        );
        assert_eq!(
            toast_content(Lang::Ko, &download(7, "completed")),
            ("report.pdf 다운로드 완료".to_owned(), Some(7))
        );
    }

    #[test]
    fn 실패한_다운로드는_상태를_알리고_버튼이_없다() {
        assert_eq!(
            toast_content(Lang::En, &download(7, "cancelled")),
            ("Download cancelled: report.pdf".to_owned(), None)
        );
        assert_eq!(
            toast_content(Lang::En, &download(7, "interrupted")),
            ("Download interrupted: report.pdf".to_owned(), None)
        );
    }

    #[test]
    fn 대화상자_알림은_종류와_응답과_메시지를_담는다() {
        assert_eq!(
            toast_content(Lang::En, &dialog("Leave site?", true)).0,
            "Page confirm answered OK for the agent: Leave site?"
        );
        assert_eq!(
            toast_content(Lang::En, &dialog("Leave site?", false)),
            (
                "Page confirm answered Cancel for the agent: Leave site?".to_owned(),
                None
            )
        );
        assert_eq!(
            toast_content(Lang::Ko, &dialog("Leave site?", true)).0,
            "에이전트 대신 페이지 confirm에 OK 응답: Leave site?"
        );
    }

    #[test]
    fn 메시지는_80자까지는_그대로_81자부터_자르고_말줄임표를_붙인다() {
        let exactly = "a".repeat(80);
        assert_eq!(clip_message(&exactly), exactly);
        assert_eq!(clip_message(&"a".repeat(81)), format!("{exactly}…"));
        assert_eq!(clip_message(""), "");
    }

    #[test]
    fn 길이는_utf16_단위로_센다() {
        // Hangul is one UTF-16 unit but three UTF-8 bytes: 80 of them are not cut.
        let hangul = "가".repeat(80);
        assert_eq!(clip_message(&hangul), hangul);
        // An emoji is two units: 40 fit exactly, the 41st makes it too long.
        let emoji = "😀".repeat(40);
        assert_eq!(clip_message(&emoji), emoji);
        assert_eq!(clip_message(&"😀".repeat(41)), format!("{emoji}…"));
    }

    #[test]
    fn 경계에_걸친_문자는_반쪽을_남기지_않는다() {
        // 79 units, then an emoji covering units 80 and 81.
        let message = format!("{}😀b", "a".repeat(79));
        assert_eq!(clip_message(&message), format!("{}…", "a".repeat(79)));
    }

    #[test]
    fn 이벤트는_메인_프로세스가_보내는_json_그대로_읽고_쓴다() {
        let json = r#"{"kind":"download","download":{"id":3,"filename":"a.zip","state":"completed","savePath":"/tmp/a.zip"}}"#;
        let event: PageEvent = serde_json::from_str(json).unwrap();
        assert_eq!(
            event,
            PageEvent::Download {
                download: Download {
                    id: 3,
                    filename: "a.zip".into(),
                    state: "completed".into(),
                    save_path: "/tmp/a.zip".into()
                }
            }
        );
        assert_eq!(serde_json::to_string(&event).unwrap(), json);

        let json = r#"{"kind":"dialog","type":"alert","message":"hi","accept":true}"#;
        let event: PageEvent = serde_json::from_str(json).unwrap();
        assert_eq!(
            event,
            PageEvent::Dialog {
                dialog_type: "alert".into(),
                message: "hi".into(),
                accept: true
            }
        );
        assert_eq!(serde_json::to_string(&event).unwrap(), json);
    }

    #[test]
    fn 다운로드_이벤트는_기록의_나머지_필드가_딸려_와도_읽힌다() {
        // downloads.ts sends the whole record (`{ ...rec }`), not only the four fields
        // the notice uses.
        let json = r#"{"kind":"download","download":{"id":12,"url":"https://example.com/a.zip","filename":"a.zip","mime":"application/zip","totalBytes":2048,"receivedBytes":2048,"state":"interrupted","savePath":"","startedAt":1760000000000,"endedAt":1760000001500}}"#;
        let event: PageEvent = serde_json::from_str(json).unwrap();
        assert_eq!(
            toast_content(Lang::En, &event),
            ("Download interrupted: a.zip".to_owned(), None)
        );
    }

    #[test]
    fn 파일_이름과_메시지의_달러_기호는_타입스크립트처럼_풀린다() {
        // `t()` fills with `replaceAll`, which reads `$$` as one `$` and `$&` as the
        // placeholder it is replacing.
        let event = PageEvent::Download {
            download: Download {
                id: 1,
                filename: "cost$$.pdf".into(),
                state: "completed".into(),
                save_path: String::new(),
            },
        };
        assert_eq!(toast_content(Lang::En, &event).0, "Downloaded cost$.pdf");
        assert_eq!(
            toast_content(Lang::En, &dialog("Pay $$5 & $&?", false)).0,
            "Page confirm answered Cancel for the agent: Pay $5 & {message}?"
        );
    }

    #[test]
    fn 모르는_종류나_빠진_필드는_오류로_돌려준다() {
        assert!(serde_json::from_str::<PageEvent>(r#"{"kind":"beep"}"#).is_err());
        assert!(serde_json::from_str::<PageEvent>(r#"{"kind":"dialog","type":"alert"}"#).is_err());
        assert!(serde_json::from_str::<PageEvent>("[]").is_err());
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::RefCell;

    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::ui::testing::{look, stage};

    fn download(id: i64, state: &str) -> PageEvent {
        PageEvent::Download {
            download: Download {
                id,
                filename: "report.pdf".into(),
                state: state.into(),
                save_path: "/tmp/report.pdf".into(),
            },
        }
    }

    /// The toast view; the list holds every id `reveal` was given.
    fn toast(
        cx: &mut TestAppContext,
    ) -> (
        Entity<PageEventToast>,
        Rc<RefCell<Vec<i64>>>,
        &mut VisualTestContext,
    ) {
        let revealed: Rc<RefCell<Vec<i64>>> = Rc::default();
        let (toast, _, cx) = stage(cx, {
            let revealed = revealed.clone();
            move |_| PageEventToast::new(move |id, _, _| revealed.borrow_mut().push(id), look())
        });
        (toast, revealed, cx)
    }

    fn push(event: PageEvent, toast: &Entity<PageEventToast>, cx: &mut VisualTestContext) {
        toast.update(cx, |toast, cx| toast.page_event(event, cx));
        cx.run_until_parked();
    }

    fn wait(ms: u64, cx: &mut VisualTestContext) {
        cx.executor().advance_clock(Duration::from_millis(ms));
        cx.run_until_parked();
    }

    /// The keys of the notices on screen, oldest first.
    fn showing(toast: &Entity<PageEventToast>, cx: &mut VisualTestContext) -> Vec<u64> {
        toast.read_with(cx, |toast, _| {
            toast.queue.items().iter().map(|item| item.key).collect()
        })
    }

    #[gpui::test]
    fn 알림은_6초_뒤에_스스로_사라진다(cx: &mut TestAppContext) {
        let (toast, _, cx) = toast(cx);
        assert!(cx.debug_bounds("page-event-toast-1").is_none());
        push(download(1, "completed"), &toast, cx);
        assert!(cx.debug_bounds("page-event-toast-1").is_some());
        wait(SHOW_MS - 1, cx);
        assert_eq!(showing(&toast, cx), vec![1]);
        wait(1, cx);
        assert!(showing(&toast, cx).is_empty());
    }

    #[gpui::test]
    fn 알림마다_제_시간에_사라진다(cx: &mut TestAppContext) {
        let (toast, _, cx) = toast(cx);
        push(download(1, "completed"), &toast, cx);
        wait(4000, cx);
        push(download(2, "completed"), &toast, cx);
        wait(2000, cx);
        assert_eq!(showing(&toast, cx), vec![2]);
        wait(3999, cx);
        assert_eq!(showing(&toast, cx), vec![2]);
        wait(1, cx);
        assert!(showing(&toast, cx).is_empty());
    }

    #[gpui::test]
    fn 네_번째_알림이_오면_가장_오래된_것이_밀려난다(cx: &mut TestAppContext) {
        let (toast, _, cx) = toast(cx);
        for id in 1..=4 {
            push(download(id, "completed"), &toast, cx);
        }
        assert_eq!(showing(&toast, cx), vec![2, 3, 4]);
        let second = cx.debug_bounds("page-event-toast-2").unwrap();
        let third = cx.debug_bounds("page-event-toast-3").unwrap();
        // Oldest on top, 6px apart.
        assert_eq!(third.top(), second.bottom() + px(6.0));
    }

    #[gpui::test]
    fn 알림은_창_오른쪽에서_16_떨어져_놓이고_420보다_넓어지지_않는다(
        cx: &mut TestAppContext,
    ) {
        let (toast, _, cx) = toast(cx);
        push(
            PageEvent::Dialog {
                dialog_type: "confirm".into(),
                message: "Are you sure you want to leave? ".repeat(4),
                accept: false,
            },
            &toast,
            cx,
        );
        let viewport = cx.update(|window, _| window.viewport_size());
        let notice = cx.debug_bounds("page-event-toast-1").unwrap();
        assert_eq!(notice.right(), viewport.width - px(16.0));
        assert_eq!(notice.size.width, px(420.0));
    }

    #[gpui::test]
    fn 폴더에서_보기를_누르면_그_다운로드를_알린다(cx: &mut TestAppContext) {
        let (toast, revealed, cx) = toast(cx);
        push(download(7, "completed"), &toast, cx);
        let button = cx.debug_bounds("page-event-reveal-1").unwrap().center();
        cx.simulate_click(button, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(*revealed.borrow(), vec![7]);
        // The notice stays until its time is up.
        assert_eq!(showing(&toast, cx), vec![1]);
    }

    #[gpui::test]
    fn 끝나지_않은_다운로드와_대화상자_알림에는_버튼이_없다(
        cx: &mut TestAppContext,
    ) {
        let (toast, _, cx) = toast(cx);
        push(download(7, "interrupted"), &toast, cx);
        push(
            PageEvent::Dialog {
                dialog_type: "alert".into(),
                message: "hi".into(),
                accept: true,
            },
            &toast,
            cx,
        );
        assert_eq!(showing(&toast, cx), vec![1, 2]);
        assert!(cx.debug_bounds("page-event-reveal-1").is_none());
        assert!(cx.debug_bounds("page-event-reveal-2").is_none());
    }
}
