//! Opens this crate's views alone with fixture data. See rever_ui_kit::preview.
//!
//! The toolbar holds four IP badges (shown, hidden, failed, still loading) and the ⋮
//! menu, open. The page area holds a composer card with two chip selects (the second
//! one open) and the bot-check button with its menu open. The copy confirmation fires
//! every 1.7 seconds and page notices keep arriving, so a capture shows them at almost
//! any moment (the confirmation is faint in the last 0.3 seconds of each round). The
//! line at the top of the page area names the last thing a widget asked the host to do.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, Context, Entity, SharedString, Task, Window, div, prelude::*, px};
use rever_ui_kit::i18n::Lang;
use rever_ui_kit::theme::{Theme, ThemeMode};
use rever_ui_menus::app_menu::{AppMenu, AppMenuItem, AppMenuProps};
use rever_ui_menus::bot_check_button::BotCheckButton;
use rever_ui_menus::chip_select::{ChipOption, ChipSelect, ChipSelectProps};
use rever_ui_menus::copy_toast::{CopyToast, ElementCopied, Rect};
use rever_ui_menus::ip_badge::{IpBadge, IpBadgeHost, IpResult};
use rever_ui_menus::page_event_toast::{Download, PageEvent, PageEventToast};
use rever_ui_menus::ui::{Look, LookFn};

const WIDTH: f32 = 1000.0;
const HEIGHT: f32 = 640.0;
const TOOLBAR: f32 = 46.0;
/// The "picked element" the copy outline frames, in page coordinates.
const PICKED: Rect = Rect {
    x: 380.0,
    y: 70.0,
    width: 240.0,
    height: 44.0,
};

/// Stands in for `window.rev.getCurrentIp` and the IP visibility store.
struct FakeIp {
    /// `None` never answers, which leaves the badge loading.
    answer: Option<Result<IpResult, String>>,
    hidden: Cell<bool>,
}

impl IpBadgeHost for FakeIp {
    fn current_ip(&self, cx: &mut App) -> Task<Result<IpResult, String>> {
        match self.answer.clone() {
            Some(answer) => Task::ready(answer),
            None => cx.spawn(async |_| std::future::pending().await),
        }
    }

    fn hidden(&self, _: &App) -> bool {
        self.hidden.get()
    }

    fn toggle_hidden(&self, _: &mut App) {
        self.hidden.set(!self.hidden.get());
    }
}

fn notices() -> Vec<PageEvent> {
    let download = |id, filename: &str, state: &str| PageEvent::Download {
        download: Download {
            id,
            filename: filename.into(),
            state: state.into(),
            save_path: format!("/Users/me/Downloads/Rever/{filename}"),
        },
    };
    vec![
        download(1, "pushed-out-by-the-limit.txt", "completed"),
        download(2, "traffic-export.har", "completed"),
        download(3, "bundle.min.js", "interrupted"),
        PageEvent::Dialog {
            dialog_type: "confirm".into(),
            message: "Are you sure you want to leave this page? Changes you made may not be saved, and the upload in progress will be cancelled.".into(),
            accept: true,
        },
    ]
}

struct Preview {
    look: Rc<Cell<Look>>,
    last: Rc<RefCell<String>>,
    badges: Vec<Entity<IpBadge>>,
    menu: Entity<AppMenu>,
    permission: Entity<ChipSelect>,
    profile: Entity<ChipSelect>,
    botcheck: Entity<BotCheckButton>,
    copy: Entity<CopyToast>,
    notices: Entity<PageEventToast>,
}

impl Preview {
    fn new(cx: &mut Context<Self>) -> Self {
        let look = Rc::new(Cell::new(Look {
            theme_mode: ThemeMode::from_env(),
            lang: Lang::detect(),
        }));
        let look_fn: LookFn = {
            let look = look.clone();
            Rc::new(move |_| look.get())
        };
        let last = Rc::new(RefCell::new(String::from("(nothing yet)")));
        // Every fake host action lands here.
        let say = {
            let (last, preview) = (last.clone(), cx.entity_id());
            move |text: String, cx: &mut App| {
                *last.borrow_mut() = text;
                cx.notify(preview);
            }
        };

        let ip = |ip: &str| {
            Some(Ok(IpResult {
                ip: Some(ip.to_owned()),
                error: None,
            }))
        };
        let badges = [
            (ip("203.0.113.42"), false),
            (ip("203.0.113.42"), true),
            (
                Some(Err("net::ERR_PROXY_CONNECTION_FAILED".to_owned())),
                false,
            ),
            (None, false),
        ]
        .into_iter()
        .map(|(answer, hidden)| {
            let host = Rc::new(FakeIp {
                answer,
                hidden: Cell::new(hidden),
            });
            cx.new(|cx| IpBadge::new(host, look_fn.clone(), cx))
        })
        .collect();

        let menu = cx.new(|cx| {
            let (look, say) = (look.clone(), say.clone());
            let mut menu = AppMenu::new(
                move |_| {
                    let now = look.get();
                    let t = |key| now.lang.t(key);
                    let report = |what: &'static str| {
                        let say = say.clone();
                        move |_: &mut Window, cx: &mut App| say(format!("menu: {what}"), cx)
                    };
                    let (cycle_theme, switch_lang) = (look.clone(), look.clone());
                    AppMenuProps {
                        title: t("menu.title").into(),
                        footer: Some("Rever Browser v0.9.0".into()),
                        items: vec![
                            AppMenuItem::new(t("menu.theme"), move |_, _| {
                                let now = cycle_theme.get();
                                cycle_theme.set(Look {
                                    theme_mode: match now.theme_mode {
                                        ThemeMode::Light => ThemeMode::Dark,
                                        ThemeMode::Dark => ThemeMode::System,
                                        ThemeMode::System => ThemeMode::Light,
                                    },
                                    ..now
                                });
                            })
                            .hint(t(match now.theme_mode {
                                ThemeMode::Light => "menu.theme.light",
                                ThemeMode::Dark => "menu.theme.dark",
                                ThemeMode::System => "menu.theme.system",
                            })),
                            AppMenuItem::new(t("menu.pageTheme"), report("page theme"))
                                .hint(t("menu.pageTheme.auto"))
                                .title("https://example.com — Auto → Light → Dark"),
                            AppMenuItem::new(t("lang.label"), move |_, _| {
                                let now = switch_lang.get();
                                switch_lang.set(Look {
                                    lang: match now.lang {
                                        Lang::En => Lang::Ko,
                                        Lang::Ko => Lang::En,
                                    },
                                    ..now
                                });
                            })
                            .hint(match now.lang {
                                Lang::En => "English",
                                Lang::Ko => "한국어",
                            }),
                            AppMenuItem::new(t("menu.notifications"), report("notifications"))
                                .hint(t("menu.on")),
                            AppMenuItem::new(t("menu.viewport"), report("viewport"))
                                .hint(t("toolbar.viewport.desktop"))
                                .sep(),
                            AppMenuItem::new(t("menu.viewSource"), report("view source"))
                                .title(t("toolbar.viewSource"))
                                .disabled(true)
                                .sep(),
                        ],
                    }
                },
                look_fn.clone(),
            );
            menu.set_open(true, cx);
            menu
        });

        let chip = |cx: &mut Context<Self>,
                    icon: &'static str,
                    title: &'static str,
                    options: &'static [(&'static str, &'static str, &'static str)],
                    open: bool| {
            let value = Rc::new(RefCell::new(SharedString::from(
                options.first().map_or("", |option| option.0),
            )));
            let (look, say, chosen) = (look.clone(), say.clone(), value.clone());
            let look_fn = look_fn.clone();
            cx.new(|cx| {
                let mut select = ChipSelect::new(
                    move |_| {
                        let lang = look.get().lang;
                        ChipSelectProps {
                            icon: Some(icon),
                            value: value.borrow().clone(),
                            options: options
                                .iter()
                                .map(|(value, label, hint)| {
                                    ChipOption::new(*value, lang.t(label)).hint(lang.t(hint))
                                })
                                .collect(),
                            title: lang.t(title).into(),
                            disabled: false,
                            collapsed: false,
                        }
                    },
                    move |value, _, cx| {
                        *chosen.borrow_mut() = value.to_owned().into();
                        say(format!("select: {value}"), cx);
                    },
                    look_fn,
                );
                select.set_open(open, cx);
                select
            })
        };
        let permission = chip(
            cx,
            "shield",
            "chat.perm.label",
            &[
                ("auto", "chat.perm.auto", "chat.perm.auto.hint"),
                ("manual", "chat.perm.manual", "chat.perm.manual.hint"),
            ],
            false,
        );
        let profile = chip(
            cx,
            "wrench",
            "tools.profile.label",
            &[
                ("browse", "tools.profile.browse", "tools.profile.tip.browse"),
                (
                    "reverse",
                    "tools.profile.reverse",
                    "tools.profile.tip.reverse",
                ),
                ("probe", "tools.profile.probe", "tools.profile.tip.probe"),
            ],
            true,
        );

        let botcheck = cx.new(|cx| {
            let say = say.clone();
            let mut button = BotCheckButton::new(
                move |url, _, cx| say(format!("navigate: {url}"), cx),
                look_fn.clone(),
            );
            button.set_open(true, cx);
            button
        });

        let copy = cx.new(|_| {
            CopyToast::new(
                |_| {
                    Some(Rect {
                        x: 0.0,
                        y: TOOLBAR,
                        width: WIDTH,
                        height: HEIGHT - TOOLBAR,
                    })
                },
                look_fn.clone(),
            )
        });

        let notices_view = cx.new(|_| {
            let say = say.clone();
            PageEventToast::new(
                move |id, _, cx| say(format!("reveal download {id}"), cx),
                look_fn.clone(),
            )
        });

        // Four at once (one more than fits), then one more with each copy confirmation.
        let (copy_loop, notices_loop) = (copy.clone(), notices_view.clone());
        cx.spawn(async move |_, cx| {
            let all = notices();
            let mut next = 0;
            let mut push = |count: usize, cx: &mut gpui::AsyncApp| {
                for _ in 0..count {
                    let Some(event) = all.get(next % all.len()).cloned() else {
                        return;
                    };
                    next += 1;
                    let _ = notices_loop.update(cx, |toast, cx| toast.page_event(event, cx));
                }
            };
            push(4, cx);
            loop {
                let payload = ElementCopied {
                    rect: Some(PICKED),
                    selector: Some("#picked".into()),
                    reference: None,
                };
                let _ = copy_loop.update(cx, |toast, cx| toast.element_copied(&payload, cx));
                cx.background_executor()
                    .timer(Duration::from_millis(1700))
                    .await;
                push(1, cx);
            }
        })
        .detach();

        Self {
            look,
            last,
            badges,
            menu,
            permission,
            profile,
            botcheck,
            copy,
            notices: notices_view,
        }
    }
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let look = self.look.get();
        let theme = Theme::resolve(look.theme_mode, window.appearance());
        let label = |text: &'static str| {
            div()
                .px(px(6.0))
                .text_size(px(11.0))
                .text_color(theme.text_dim)
                .child(text)
        };

        let toolbar = div()
            .h(px(TOOLBAR))
            .px(px(8.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .bg(theme.chrome_toolbar)
            .border_b_1()
            .border_color(theme.border)
            .child(label("IpBadge: shown / hidden / failed / loading"))
            .children(self.badges.iter().cloned())
            .child(div().flex_1())
            .child(label("AppMenu"))
            .child(self.menu.clone());

        // `.composer`
        let composer = div()
            .absolute()
            .left(px(360.0))
            .top(px(360.0))
            .w(px(300.0))
            .pt(px(10.0))
            .px(px(12.0))
            .pb(px(8.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .rounded(px(16.0))
            .border_1()
            .border_color(theme.border_2)
            .bg(theme.chrome_toolbar)
            .child(
                div()
                    .text_size(px(14.0))
                    .text_color(theme.text_dim)
                    .child("ChipSelect (composer row)"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(self.permission.clone())
                    .child(self.profile.clone()),
            );

        let page = div()
            .flex_1()
            .min_h_0()
            .relative()
            .child(
                div()
                    .p(px(12.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_dim)
                    .child(format!("last host call — {}", self.last.borrow())),
            )
            .child(
                // The element the copy confirmation outlines.
                div()
                    .absolute()
                    .left(px(PICKED.x))
                    .top(px(PICKED.y))
                    .w(px(PICKED.width))
                    .h(px(PICKED.height))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.surface_2)
                    .text_size(px(12.0))
                    .child("picked element (CopyToast)"),
            )
            .child(composer)
            .child(self.botcheck.clone());

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(px(13.0))
            .font_family(".SystemUIFont")
            .child(toolbar)
            .child(page)
            .child(self.copy.clone())
            .child(self.notices.clone())
    }
}

fn main() {
    rever_ui_kit::preview::run(WIDTH, HEIGHT, |_, cx| cx.new(Preview::new));
}
