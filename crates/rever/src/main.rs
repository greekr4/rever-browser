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
mod agent_acp;
mod agent_api;
mod api_keys;
mod app;
mod bridge;
mod chat_agents;
mod debug;
mod engine;
mod intercept;
mod mcp_client;
mod memory;
mod panels;
mod permissions;
mod profile_windows;
mod scan;
mod setup_host;
mod shell;
mod storage_host;
mod workflow_agents;

use app::{Browser, Launch, PANEL_GROUPS};
use gpui::{
    App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, prelude::*, px,
    size,
};
use rever_core::address::normalize_address;
use rever_ui_kit::text_input;
use url::Url;

fn main() {
    // No URL: the app opens what the last run left open.
    let mut url = None;
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
            url = Some(arg);
        }
    }
    let url = url.map(|url| {
        Url::parse(&normalize_address(&url)).expect("the URL argument is not a valid URL")
    });
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
            shell::shortcuts::bind_keys(cx);
            // There is one window: closing it (the red button) ends the app, and Cmd+Q
            // ends it wherever the focus is, even when nothing has it.
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.on_action(|_: &shell::shortcuts::Quit, cx| cx.quit());
            #[cfg(target_os = "macos")]
            set_dock_icon();
            let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    // Chrome's look: no title bar, the traffic lights sit in the tab strip.
                    titlebar: Some(TitlebarOptions {
                        title: None,
                        appears_transparent: true,
                        traffic_light_position: Some(point(
                            px(14.0),
                            px((shell::STRIP_HEIGHT - 12.0) / 2.0),
                        )),
                    }),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Browser::new(launch, window, cx)),
            )
            .expect("could not open the main window");
            cx.activate(true);
        });
}

/// Run from `cargo run` there is no .app bundle, so macOS shows a generic "exec" icon in
/// the Dock. Hand it the app icon instead.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2's macros test a cfg newer compilers do not know.
fn set_dock_icon() {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    const ICON: &[u8] = include_bytes!("../../../resources/icon-macos.png");
    unsafe {
        let data: *mut Object =
            msg_send![class!(NSData), dataWithBytes: ICON.as_ptr() length: ICON.len()];
        let image: *mut Object = msg_send![class!(NSImage), alloc];
        let image: *mut Object = msg_send![image, initWithData: data];
        if image.is_null() {
            return;
        }
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let () = msg_send![app, setApplicationIconImage: image];
    }
}
