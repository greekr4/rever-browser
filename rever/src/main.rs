//! rever: a GPUI interface around Servo page views.
//!
//! Usage: rever [--panel=NAME] [--say=TEXT] [--self-check=DIR] [URL]
//!   --panel=NAME      start with a bottom panel open (traffic, console, ...).
//!   --say=TEXT        send a chat message 2 seconds after start (port harness).
//!   --self-check=DIR  after 8 seconds write DIR/frame.png (the page as handed to the
//!                     interface) and DIR/state.json, then quit. Used by the port harness.
//!
//! `app` holds the window's state, `panels` the panel crates' views and what they need
//! from the app, `shell` draws the window.

mod agent;
mod app;
mod bridge;
mod engine;
mod panels;
mod shell;

use app::{Browser, DEFAULT_URL, Launch, PANEL_GROUPS};
use gpui::{App, Application, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};
use rever_core::address::normalize_address;
use rever_ui_kit::text_input;
use url::Url;

fn main() {
    let mut url = DEFAULT_URL.to_owned();
    let (mut self_check, mut open_panel, mut say) = (None, None, None);
    for arg in std::env::args().skip(1) {
        if let Some(dir) = arg.strip_prefix("--self-check=") {
            self_check = Some(dir.to_owned());
        } else if let Some(text) = arg.strip_prefix("--say=") {
            say = Some(text.to_owned());
        } else if let Some(name) = arg.strip_prefix("--panel=") {
            // Only known panels: the name is also the chip's element id.
            open_panel = PANEL_GROUPS
                .iter()
                .flat_map(|group| group.iter())
                .copied()
                .find(|panel| *panel == name);
        } else {
            url = arg;
        }
    }
    let url = Url::parse(&normalize_address(&url)).expect("the URL argument is not a valid URL");
    let launch = Launch {
        url,
        open_panel,
        self_check,
        say,
    };

    Application::new()
        .with_assets(shell::Assets)
        .run(move |cx: &mut App| {
            text_input::bind_keys(cx);
            let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Browser::new(launch, window, cx)),
            )
            .expect("could not open the main window");
            cx.activate(true);
        });
}
