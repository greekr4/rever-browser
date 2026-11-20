//! Chrome's macOS keyboard shortcuts. Bindings match before the page sees the key, so a
//! page never receives Cmd+W and the like.

use gpui::{App, Context, Div, Focusable, KeyBinding, Window, actions, prelude::*};
use rever_ui_kit::text_input::SelectAll;

use crate::app::Browser;

actions!(
    browser,
    [
        NewTab,
        CloseTab,
        ReopenClosedTab,
        NextTab,
        PrevTab,
        Tab1,
        Tab2,
        Tab3,
        Tab4,
        Tab5,
        Tab6,
        Tab7,
        Tab8,
        LastTab,
        FocusAddress,
        Reload,
        Back,
        Forward,
        Bookmark,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        Minimize,
        Hide,
        Quit,
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-t", NewTab, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("cmd-shift-t", ReopenClosedTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("cmd-alt-right", NextTab, None),
        KeyBinding::new("cmd-}", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new("cmd-alt-left", PrevTab, None),
        KeyBinding::new("cmd-{", PrevTab, None),
        KeyBinding::new("cmd-1", Tab1, None),
        KeyBinding::new("cmd-2", Tab2, None),
        KeyBinding::new("cmd-3", Tab3, None),
        KeyBinding::new("cmd-4", Tab4, None),
        KeyBinding::new("cmd-5", Tab5, None),
        KeyBinding::new("cmd-6", Tab6, None),
        KeyBinding::new("cmd-7", Tab7, None),
        KeyBinding::new("cmd-8", Tab8, None),
        KeyBinding::new("cmd-9", LastTab, None),
        KeyBinding::new("cmd-l", FocusAddress, None),
        KeyBinding::new("cmd-r", Reload, None),
        KeyBinding::new("cmd-shift-r", Reload, None),
        KeyBinding::new("f5", Reload, None),
        KeyBinding::new("cmd-[", Back, None),
        KeyBinding::new("cmd-]", Forward, None),
        KeyBinding::new("cmd-d", Bookmark, None),
        KeyBinding::new("cmd-=", ZoomIn, None),
        KeyBinding::new("cmd-+", ZoomIn, None),
        KeyBinding::new("cmd--", ZoomOut, None),
        KeyBinding::new("cmd-0", ZoomReset, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]);
}

/// Cmd+1..8 pick that tab; Cmd+9 always picks the last one, as in Chrome.
fn tab_index(number: usize, count: usize) -> Option<usize> {
    (number <= count).then(|| number - 1)
}

/// Next or previous tab, wrapping around the ends.
fn cycle(active: usize, count: usize, forward: bool) -> usize {
    if forward {
        (active + 1) % count
    } else {
        (active + count - 1) % count
    }
}

impl Browser {
    fn select_number(&mut self, number: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = tab_index(number, self.tabs.len()) {
            self.select_tab(index, window, cx);
        }
    }

    /// Hooks every shortcut to the window's root element.
    pub(super) fn shortcut_handlers(&self, root: Div, cx: &mut Context<Self>) -> Div {
        root.on_action(cx.listener(|this, _: &NewTab, window, cx| this.new_tab(window, cx)))
            .on_action(
                cx.listener(|this, _: &CloseTab, window, cx| {
                    this.close_tab(this.active, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ReopenClosedTab, window, cx| {
                this.reopen_closed_tab(window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                let next = cycle(this.active, this.tabs.len(), true);
                this.select_tab(next, window, cx)
            }))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| {
                let previous = cycle(this.active, this.tabs.len(), false);
                this.select_tab(previous, window, cx)
            }))
            .on_action(cx.listener(|this, _: &Tab1, w, cx| this.select_number(1, w, cx)))
            .on_action(cx.listener(|this, _: &Tab2, w, cx| this.select_number(2, w, cx)))
            .on_action(cx.listener(|this, _: &Tab3, w, cx| this.select_number(3, w, cx)))
            .on_action(cx.listener(|this, _: &Tab4, w, cx| this.select_number(4, w, cx)))
            .on_action(cx.listener(|this, _: &Tab5, w, cx| this.select_number(5, w, cx)))
            .on_action(cx.listener(|this, _: &Tab6, w, cx| this.select_number(6, w, cx)))
            .on_action(cx.listener(|this, _: &Tab7, w, cx| this.select_number(7, w, cx)))
            .on_action(cx.listener(|this, _: &Tab8, w, cx| this.select_number(8, w, cx)))
            .on_action(cx.listener(|this, _: &LastTab, window, cx| {
                this.select_tab(this.tabs.len() - 1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusAddress, window, cx| {
                window.focus(&this.views.address.focus_handle(cx));
                window.dispatch_action(Box::new(SelectAll), cx);
            }))
            .on_action(cx.listener(|this, _: &Reload, _, _| this.engine.reload(this.active_id())))
            .on_action(cx.listener(|this, _: &Back, _, _| this.engine.back(this.active_id())))
            .on_action(cx.listener(|this, _: &Forward, _, _| this.engine.forward(this.active_id())))
            .on_action(cx.listener(|this, _: &Bookmark, _, cx| this.toggle_bookmark(cx)))
            .on_action(
                cx.listener(|this, _: &ZoomIn, _, _| {
                    this.engine.zoom(this.active_id(), Some(true))
                }),
            )
            .on_action(cx.listener(|this, _: &ZoomOut, _, _| {
                this.engine.zoom(this.active_id(), Some(false))
            }))
            .on_action(
                cx.listener(|this, _: &ZoomReset, _, _| this.engine.zoom(this.active_id(), None)),
            )
            .on_action(cx.listener(|_, _: &Minimize, window, _| window.minimize_window()))
            .on_action(cx.listener(|_, _: &Hide, _, cx| cx.hide()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 숫자_단축키는_있는_탭만_고르고_9는_마지막_탭이다() {
        assert_eq!(tab_index(1, 3), Some(0));
        assert_eq!(tab_index(3, 3), Some(2));
        assert_eq!(tab_index(4, 3), None);
    }

    #[test]
    fn 다음_이전_탭은_양끝에서_돌아간다() {
        assert_eq!(cycle(2, 3, true), 0);
        assert_eq!(cycle(0, 3, false), 2);
        assert_eq!(cycle(1, 3, true), 2);
    }
}
