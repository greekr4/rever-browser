//! Run one view alone in a window, for looking at it while it is being built.
//!
//! A panel crate adds `examples/preview.rs` that calls [`run`] with fixture data;
//! `scripts/capture-window.sh <example-name> out.png` then captures that window.

use std::time::Duration;

use gpui::{
    App, Application, Bounds, Entity, Render, Window, WindowBounds, WindowOptions, px, size,
};

use crate::icons::Assets;
use crate::text_input;

/// Open a `width` × `height` window showing the view `build` returns. The window does
/// not take focus, and the process exits after `REVER_PREVIEW_SECONDS` (default 6), so
/// a preview can be captured unattended.
pub fn run<V: Render + 'static>(
    width: f32,
    height: f32,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V> + 'static,
) {
    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            text_input::bind_keys(cx);
            let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                focus: false,
                ..Default::default()
            };
            cx.open_window(options, build)
                .expect("could not open the preview window");
            let seconds = std::env::var("REVER_PREVIEW_SECONDS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(6);
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_secs(seconds))
                    .await;
                let _ = cx.update(|cx| cx.quit());
            })
            .detach();
        });
}
