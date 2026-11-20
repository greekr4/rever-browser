//! Port of `src/renderer/src/components/MarkupEditor.tsx` — the screenshot annotation
//! overlay. Draws the captured image on a canvas and lets the user mark it up with
//! rectangles, arrows, and freehand strokes, then copy the result to the clipboard or
//! save it. Fed by Grab (element capture).
//!
//! The canvas is a bitmap here too: the image and the shapes are drawn into one RGBA
//! buffer, which is both what the window shows and what is copied or saved.

use std::cell::Cell;
use std::io::Cursor;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use gpui::{
    Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase, EventEmitter, Image, ImageFormat,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, Pixels, RenderImage, SharedString,
    Window, anchored, canvas, deferred, div, img, point, prelude::*, px, rgb, rgba,
};
use image::{Frame, Rgba, RgbaImage};

use crate::ui::{LookFn, Tokens, clickable, tooltip};

/// The palette, in toolbar order. The first one is selected at the start.
pub const COLORS: [&str; 6] = [
    "#ef4444", "#f59e0b", "#22c55e", "#3b82f6", "#ffffff", "#111111",
];

/// The size of an HTML canvas nothing has sized yet.
const BLANK_CANVAS: (u32, u32) = (300, 150);
/// How long the copy button reads "Copied ✓".
const COPIED_MS: u64 = 1500;
/// `z-index: 2000`: over every other layer of the app.
const Z_MODAL: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Rect,
    Arrow,
    Pen,
}

/// A point in canvas pixels (the image's own resolution).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Rect {
        color: &'static str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    Arrow {
        color: &'static str,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
    },
    Pen {
        color: &'static str,
        points: Vec<CanvasPoint>,
    },
}

/// The image of a `data:…;base64,…` URL, `None` when it is not one or does not decode
/// (the `<img>` never fires `onload` then).
pub fn decode_data_url(url: &str) -> Option<RgbaImage> {
    let scheme = url.get(..5)?;
    if !scheme.eq_ignore_ascii_case("data:") {
        return None;
    }
    let (meta, data) = url.get(5..)?.split_once(',')?;
    let (_, encoding) = meta.rsplit_once(';')?;
    if !encoding.eq_ignore_ascii_case("base64") {
        return None;
    }
    // Browsers skip whitespace and do not insist on the padding.
    let data: Vec<u8> = data.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let engine = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let bytes = engine.decode(data).ok()?;
    Some(image::load_from_memory(&bytes).ok()?.into_rgba8())
}

/// `Math.max(2, Math.round(naturalWidth / 320))`: strokes stay visible on large captures.
pub fn line_width(natural_width: u32) -> f32 {
    // `Math.round` rounds a half up.
    ((f64::from(natural_width) / 320.0 + 0.5).floor() as f32).max(2.0)
}

/// A window position as a canvas position: the canvas is shown scaled down to `rect`
/// (left, top, width, height) but is `canvas` pixels large.
pub fn to_canvas_point(
    client: (f32, f32),
    rect: (f32, f32, f32, f32),
    canvas: (u32, u32),
) -> CanvasPoint {
    let (left, top, width, height) = rect;
    CanvasPoint {
        x: ((client.0 - left) / width) * canvas.0 as f32,
        y: ((client.1 - top) / height) * canvas.1 as f32,
    }
}

/// The shape a press at `p` starts.
pub fn begin_shape(tool: Tool, color: &'static str, p: CanvasPoint) -> Shape {
    match tool {
        Tool::Rect => Shape::Rect {
            color,
            x: p.x,
            y: p.y,
            w: 0.0,
            h: 0.0,
        },
        Tool::Arrow => Shape::Arrow {
            color,
            x1: p.x,
            y1: p.y,
            x2: p.x,
            y2: p.y,
        },
        Tool::Pen => Shape::Pen {
            color,
            points: vec![p],
        },
    }
}

/// Follow the pointer to `p` with the shape being drawn.
pub fn extend_shape(shape: &mut Shape, p: CanvasPoint) {
    match shape {
        Shape::Rect { x, y, w, h, .. } => {
            *w = p.x - *x;
            *h = p.y - *y;
        }
        Shape::Arrow { x2, y2, .. } => {
            *x2 = p.x;
            *y2 = p.y;
        }
        Shape::Pen { points, .. } => points.push(p),
    }
}

/// Whether a finished shape is kept. Discard zero-size clicks (no drag) — of the
/// rectangle and the pen; an arrow is kept whatever its length.
pub fn is_kept(shape: &Shape) -> bool {
    match shape {
        Shape::Rect { w, h, .. } => !(w.abs() < 3.0 && h.abs() < 3.0),
        Shape::Pen { points, .. } => points.len() >= 2,
        Shape::Arrow { .. } => true,
    }
}

fn shape_color(shape: &Shape) -> &'static str {
    match shape {
        Shape::Rect { color, .. } | Shape::Arrow { color, .. } | Shape::Pen { color, .. } => color,
    }
}

/// The straight lines a shape is stroked along, `lw` being the line width (it sets the
/// size of the arrowhead).
pub fn segments(shape: &Shape, lw: f32) -> Vec<(CanvasPoint, CanvasPoint)> {
    let at = |x: f32, y: f32| CanvasPoint { x, y };
    match *shape {
        Shape::Rect { x, y, w, h, .. } => {
            let corners = [at(x, y), at(x + w, y), at(x + w, y + h), at(x, y + h)];
            (0..4)
                .filter_map(|i| Some((*corners.get(i)?, *corners.get((i + 1) % 4)?)))
                .collect()
        }
        Shape::Arrow { x1, y1, x2, y2, .. } => {
            // Arrowhead.
            let angle = (y2 - y1).atan2(x2 - x1);
            let head = lw * 4.0;
            let barb = |turn: f32| {
                let a = angle + turn;
                at(x2 - head * a.cos(), y2 - head * a.sin())
            };
            let spread = std::f32::consts::FRAC_PI_6;
            vec![
                (at(x1, y1), at(x2, y2)),
                (at(x2, y2), barb(-spread)),
                (at(x2, y2), barb(spread)),
            ]
        }
        Shape::Pen { ref points, .. } => points
            .windows(2)
            .filter_map(|pair| Some((*pair.first()?, *pair.get(1)?)))
            .collect(),
    }
}

/// `#rrggbb` as bytes.
pub fn parse_color(hex: &str) -> Option<[u8; 3]> {
    let digits = hex.strip_prefix('#').filter(|digits| digits.len() == 6)?;
    let channel = |at: usize| u8::from_str_radix(digits.get(at..at + 2)?, 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

fn distance(p: (f32, f32), a: CanvasPoint, b: CanvasPoint) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let along = ((p.0 - a.x) * dx + (p.1 - a.y) * dy) / (dx * dx + dy * dy);
    let along = along.clamp(0.0, 1.0);
    (p.0 - (a.x + along * dx)).hypot(p.1 - (a.y + along * dy))
}

/// The pixel range `[lo - reach, hi + reach]` covers, cut to `0..limit`.
fn pixel_range(lo: f32, hi: f32, reach: f32, limit: u32) -> (u32, u32) {
    // Float-to-int casts saturate, so a far-away coordinate cannot wrap around.
    let start = (lo - reach).floor().max(0.0) as u32;
    let end = (hi + reach).ceil().max(0.0) as u32;
    (start.min(limit), end.min(limit))
}

/// Stroke a shape onto the canvas the way the 2D context does with round caps and
/// joins: every pixel within half the line width of one of its lines takes the colour.
pub fn draw_shape(canvas: &mut RgbaImage, shape: &Shape, lw: f32) {
    let Some(color) = parse_color(shape_color(shape)) else {
        return;
    };
    // A zero-length line is pruned from a canvas path: a click leaves no dot.
    let lines: Vec<_> = segments(shape, lw)
        .into_iter()
        .filter(|(a, b)| a != b && [a.x, a.y, b.x, b.y].iter().all(|n| n.is_finite()))
        .collect();
    let (width, height) = canvas.dimensions();
    let half = lw / 2.0;
    // One pixel more than the stroke: the antialiased edge.
    let reach = half + 1.0;
    let fold = |pick: fn(&CanvasPoint) -> f32| {
        lines
            .iter()
            .flat_map(|(a, b)| [pick(a), pick(b)])
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), n| {
                (lo.min(n), hi.max(n))
            })
    };
    let ((min_x, max_x), (min_y, max_y)) = (fold(|p| p.x), fold(|p| p.y));
    let (x0, x1) = pixel_range(min_x, max_x, reach, width);
    let (y0, y1) = pixel_range(min_y, max_y, reach, height);
    if lines.is_empty() || x0 >= x1 || y0 >= y1 {
        return;
    }

    // The lines of one shape overlap at the joins. Coverage is gathered per shape and
    // blended once, so a join is not painted twice.
    let mask_width = (x1 - x0) as usize;
    let mut mask = vec![0u8; mask_width * (y1 - y0) as usize];
    for (a, b) in &lines {
        let (sx0, sx1) = pixel_range(a.x.min(b.x), a.x.max(b.x), reach, width);
        let (sy0, sy1) = pixel_range(a.y.min(b.y), a.y.max(b.y), reach, height);
        for y in sy0..sy1 {
            for x in sx0..sx1 {
                let d = distance((x as f32 + 0.5, y as f32 + 0.5), *a, *b);
                let coverage = ((half + 0.5 - d).clamp(0.0, 1.0) * 255.0).round() as u8;
                let index = (y - y0) as usize * mask_width + (x - x0) as usize;
                if let Some(cell) = mask.get_mut(index) {
                    *cell = (*cell).max(coverage);
                }
            }
        }
    }

    for y in y0..y1 {
        for x in x0..x1 {
            let index = (y - y0) as usize * mask_width + (x - x0) as usize;
            let coverage = u32::from(mask.get(index).copied().unwrap_or(0));
            if coverage == 0 {
                continue;
            }
            let Some(Rgba(pixel)) = canvas.get_pixel_mut_checked(x, y) else {
                continue;
            };
            let over = |src: u8, dst: u8| {
                ((u32::from(src) * coverage + u32::from(dst) * (255 - coverage) + 127) / 255) as u8
            };
            let [r, g, b, a] = *pixel;
            *pixel = [
                over(color[0], r),
                over(color[1], g),
                over(color[2], b),
                over(255, a),
            ];
        }
    }
}

/// The canvas: the image and every committed shape (and the in-progress one). Without
/// an image nothing is ever drawn, and the canvas is the blank one HTML starts with.
pub fn render_canvas(
    image: Option<&RgbaImage>,
    shapes: &[Shape],
    drafting: Option<&Shape>,
    lw: f32,
) -> RgbaImage {
    let Some(image) = image else {
        return RgbaImage::new(BLANK_CANVAS.0, BLANK_CANVAS.1);
    };
    let mut canvas = image.clone();
    for shape in shapes.iter().chain(drafting) {
        draw_shape(&mut canvas, shape, lw);
    }
    canvas
}

/// `canvas.toBlob(…, 'image/png')`
pub fn encode_png(canvas: &RgbaImage) -> Option<Vec<u8>> {
    let mut png = Vec::new();
    canvas
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// The size the canvas is shown at: its own, scaled down (keeping its proportions) to
/// fit `max-width: 90vw; max-height: 78vh`.
pub fn fit_size(natural: (u32, u32), viewport: (f32, f32)) -> (f32, f32) {
    let (width, height) = (natural.0 as f32, natural.1 as f32);
    let scale = (viewport.0 * 0.9 / width)
        .min(viewport.1 * 0.78 / height)
        .min(1.0);
    // An empty canvas divides by zero above; it has no size to scale.
    if !scale.is_finite() {
        return (width, height);
    }
    (width * scale, height * scale)
}

/// The name a saved markup gets: `rever-markup-<Date.now()>.png`.
pub fn file_name(now_ms: u128) -> String {
    format!("rever-markup-{now_ms}.png")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkupEvent {
    /// `onClose`: the close button, or a click on the backdrop.
    Close,
    /// The "Save PNG" download: the host writes `png` to its downloads folder.
    Save { file_name: String, png: Vec<u8> },
}

pub struct MarkupEditor {
    look: LookFn,
    /// The capture, once decoded (`ready`).
    image: Option<RgbaImage>,
    line_width: f32,
    tool: Tool,
    color: &'static str,
    shapes: Vec<Shape>,
    drafting: Option<Shape>,
    copied: bool,
    /// The canvas as the window draws it, until the drawing changes.
    frame: Option<Arc<RenderImage>>,
    /// Where the canvas was last painted (`getBoundingClientRect`).
    canvas_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl EventEmitter<MarkupEvent> for MarkupEditor {}

impl MarkupEditor {
    /// The host shows the editor while it has a capture and drops it on
    /// [`MarkupEvent::Close`]; a new capture gets a new editor.
    pub fn new(image_data_url: &str, look: LookFn, cx: &mut Context<Self>) -> Self {
        let image = decode_data_url(image_data_url);
        // The frame lives in the GPU atlas until it is dropped by hand.
        cx.on_release(|this: &mut Self, cx| {
            if let Some(frame) = this.frame.take() {
                cx.drop_image(frame, None);
            }
        })
        .detach();
        Self {
            look,
            line_width: image
                .as_ref()
                .map_or(3.0, |image| line_width(image.width())),
            image,
            tool: Tool::default(),
            color: COLORS[0],
            shapes: Vec::new(),
            drafting: None,
            copied: false,
            frame: None,
            canvas_bounds: Rc::default(),
        }
    }

    /// The `imageDataUrl` prop changing on a mounted editor: the new capture goes under
    /// the shapes already drawn, and tool and colour stay. A URL that does not decode
    /// changes nothing (`onload` never fires). The component leaves its canvas blank
    /// until the next pointer event here; that is not kept, the canvas is redrawn at once.
    pub fn set_image(&mut self, image_data_url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(image) = decode_data_url(image_data_url) else {
            return;
        };
        self.line_width = line_width(image.width());
        self.image = Some(image);
        self.redraw(window, cx);
    }

    pub fn tool(&self) -> Tool {
        self.tool
    }

    pub fn color(&self) -> &'static str {
        self.color
    }

    pub fn shapes(&self) -> &[Shape] {
        &self.shapes
    }

    /// The canvas as it is copied and saved.
    pub fn canvas(&self) -> RgbaImage {
        render_canvas(
            self.image.as_ref(),
            &self.shapes,
            self.drafting.as_ref(),
            self.line_width,
        )
    }

    fn canvas_size(&self) -> (u32, u32) {
        self.image
            .as_ref()
            .map_or(BLANK_CANVAS, RgbaImage::dimensions)
    }

    /// The drawing changed: the frame on screen is stale.
    fn redraw(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(frame) = self.frame.take() {
            let _ = window.drop_image(frame);
        }
        cx.notify();
    }

    fn canvas_point(&self, position: gpui::Point<Pixels>) -> Option<CanvasPoint> {
        let bounds = self.canvas_bounds.get()?;
        Some(to_canvas_point(
            (position.x.into(), position.y.into()),
            (
                bounds.left().into(),
                bounds.top().into(),
                bounds.size.width.into(),
                bounds.size.height.into(),
            ),
            self.canvas_size(),
        ))
    }

    fn pointer_down(
        &mut self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = self.canvas_point(position) else {
            return;
        };
        self.drafting = Some(begin_shape(self.tool, self.color, p));
        self.redraw(window, cx);
    }

    fn pointer_move(
        &mut self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = self.canvas_point(position) else {
            return;
        };
        let Some(shape) = &mut self.drafting else {
            return;
        };
        extend_shape(shape, p);
        self.redraw(window, cx);
    }

    fn pointer_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shape) = self.drafting.take() else {
            return;
        };
        if is_kept(&shape) {
            self.shapes.push(shape);
        }
        self.redraw(window, cx);
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(png) = encode_png(&self.canvas()) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(
            ImageFormat::Png,
            png,
        )));
        self.copied = true;
        cx.notify();
        // Each copy starts its own timer and none is cancelled, as with `setTimeout`:
        // a second copy within the 1.5s is cut short by the first one's timer.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(COPIED_MS))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.copied = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn download(&mut self, cx: &mut Context<Self>) {
        let Some(png) = encode_png(&self.canvas()) else {
            return;
        };
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        cx.emit(MarkupEvent::Save {
            file_name: file_name(now_ms),
            png,
        });
    }

    /// The canvas as a GPU image, built once per change of the drawing.
    // shortcut: every pointer move redraws all shapes onto a fresh copy of the capture;
    // keep the committed shapes in a second buffer if that ever shows as lag.
    fn frame(&mut self) -> Arc<RenderImage> {
        if let Some(frame) = &self.frame {
            return frame.clone();
        }
        let mut canvas = self.canvas();
        // GPUI wants BGRA.
        for pixel in canvas.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        let frame = Arc::new(RenderImage::new(vec![Frame::new(canvas)]));
        self.frame = Some(frame.clone());
        frame
    }
}

impl Render for MarkupEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = (self.look)(cx);
        let tokens = Tokens::resolve(look.theme_mode, window);
        let theme = tokens.theme;
        let t = |key: &'static str| look.lang.t(key);
        let viewport = window.viewport_size();
        let has_shapes = !self.shapes.is_empty();

        let tool_button =
            |id: &'static str, label: &'static str, tool: Tool, title: &'static str| {
                let active = self.tool == tool;
                clickable(id)
                    .w(px(30.0))
                    .h(px(26.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .border_1()
                    .text_size(px(15.0))
                    .line_height(px(15.0))
                    .cursor_pointer()
                    .map(|button| match active {
                        true => button
                            .bg(tokens.accent_soft)
                            .border_color(tokens.accent_border)
                            .text_color(theme.accent),
                        false => button.border_color(theme.border_2).text_color(theme.text_2),
                    })
                    .tooltip(tooltip(title, &tokens))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tool = tool;
                        cx.notify();
                    }))
                    .child(label)
            };
        let separator = || {
            div()
                .w(px(1.0))
                .h(px(20.0))
                .mx(px(4.0))
                .flex_none()
                .bg(theme.border_2)
        };
        let bar_button = |id: &'static str, label: &'static str, disabled: bool, primary: bool| {
            clickable(id)
                .px(px(10.0))
                .py(px(5.0))
                .flex_none()
                .rounded(px(4.0))
                .border_1()
                .text_size(px(11.0))
                .line_height(px(16.0))
                .whitespace_nowrap()
                .map(|button| match primary {
                    true => button
                        .bg(theme.accent)
                        .border_color(theme.accent)
                        .text_color(rgb(0xffffff)),
                    false => button.border_color(theme.border_2).text_color(theme.text_2),
                })
                .map(|button| match disabled {
                    true => button.opacity(0.4),
                    false => button.cursor_pointer(),
                })
                .child(label)
        };

        let swatches = COLORS.iter().enumerate().map(|(index, &color)| {
            let selected = self.color == color;
            let [r, g, b] = parse_color(color).unwrap_or_default();
            clickable(("markup-color", index))
                .debug_selector(move || format!("markup-color-{index}"))
                .size(px(18.0))
                .flex_none()
                .rounded_full()
                .bg(rgb(u32::from_be_bytes([0, r, g, b])))
                .cursor_pointer()
                .map(|swatch| match selected {
                    true => swatch.border_2().border_color(theme.accent),
                    false => swatch.border_1().border_color(theme.border_2),
                })
                .tooltip(tooltip(SharedString::new_static(color), &tokens))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.color = color;
                    cx.notify();
                }))
        });
        let swatches: Vec<_> = swatches.collect();

        let toolbar = div()
            .debug_selector(|| "markup-toolbar".into())
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(8.0))
            .rounded(px(8.0))
            .bg(theme.bg_bar)
            .border_1()
            .border_color(theme.border_2)
            .occlude()
            .child(tool_button(
                "markup-rect",
                "▭",
                Tool::Rect,
                t("markup.rect"),
            ))
            .child(tool_button(
                "markup-arrow",
                "↗",
                Tool::Arrow,
                t("markup.arrow"),
            ))
            .child(tool_button("markup-pen", "✎", Tool::Pen, t("markup.pen")))
            .child(separator())
            .children(swatches)
            .child(separator())
            .child(
                bar_button("markup-undo", t("markup.undo"), !has_shapes, false).when(
                    has_shapes,
                    |button| {
                        button.on_click(cx.listener(|this, _, window, cx| {
                            this.shapes.pop();
                            this.redraw(window, cx);
                        }))
                    },
                ),
            )
            .child(
                bar_button("markup-clear", t("markup.clear"), !has_shapes, false).when(
                    has_shapes,
                    |button| {
                        button.on_click(cx.listener(|this, _, window, cx| {
                            this.shapes.clear();
                            this.redraw(window, cx);
                        }))
                    },
                ),
            )
            .child(
                bar_button(
                    "markup-copy",
                    t(if self.copied {
                        "markup.copied"
                    } else {
                        "markup.copy"
                    }),
                    false,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
            )
            .child(
                bar_button("markup-save", t("markup.save"), false, false)
                    .on_click(cx.listener(|this, _, _, cx| this.download(cx))),
            )
            .child(
                bar_button("markup-close", t("markup.close"), false, false)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(MarkupEvent::Close))),
            );

        let (width, height) = fit_size(
            self.canvas_size(),
            (viewport.width.into(), viewport.height.into()),
        );
        let bounds = self.canvas_bounds.clone();
        let (on_move, on_up) = (cx.entity(), cx.entity());
        // Records where the canvas is, and stands in for `setPointerCapture`: while a
        // shape is being drawn the pointer is followed wherever it goes. The listeners
        // are there in every frame, not only while drawing: an event goes to the
        // listeners of the last frame drawn, and a release that comes before the frame
        // after the press would leave the shape following the pointer.
        let track = canvas(
            move |canvas_bounds, _, _| bounds.set(Some(canvas_bounds)),
            move |_, _, window, _| {
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        on_move
                            .update(cx, |this, cx| this.pointer_move(event.position, window, cx));
                    }
                });
                window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        on_up.update(cx, |this, cx| this.pointer_up(window, cx));
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .bottom_0()
        .left_0()
        .right_0();
        let canvas_box = clickable("markup-canvas")
            .relative()
            .flex_none()
            .w(px(width))
            .h(px(height))
            .rounded(px(6.0))
            .overflow_hidden()
            .bg(rgb(0xffffff))
            .cursor(CursorStyle::Crosshair)
            .occlude()
            .on_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                this.pointer_down(event.position, window, cx)
            }))
            .child(
                img(self.frame())
                    .size_full()
                    // The box's own rounding does not clip an image.
                    .rounded(px(6.0))
                    .object_fit(ObjectFit::Fill),
            )
            .child(track);

        let backdrop = clickable("markup-backdrop")
            .w(viewport.width)
            .h(viewport.height)
            .p(px(20.0))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .bg(rgba(0x00000099))
            .font_family(".SystemUIFont")
            .occlude()
            // Only the backdrop itself: the toolbar and the canvas take their own clicks.
            .on_click(cx.listener(|_, _, _, cx| cx.emit(MarkupEvent::Close)))
            .child(toolbar)
            .child(canvas_box);
        deferred(anchored().position(point(px(0.0), px(0.0))).child(backdrop))
            .with_priority(Z_MODAL)
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    /// A `width` × `height` capture of one colour, as the data URL Grab hands over.
    pub fn data_url(width: u32, height: u32, color: [u8; 4]) -> String {
        let image = RgbaImage::from_pixel(width, height, Rgba(color));
        let png = encode_png(&image).unwrap();
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::data_url;
    use super::*;

    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const RED: [u8; 4] = [0xef, 0x44, 0x44, 255];

    fn at(x: f32, y: f32) -> CanvasPoint {
        CanvasPoint { x, y }
    }

    fn white(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_pixel(width, height, Rgba(WHITE))
    }

    fn pixel(canvas: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
        canvas.get_pixel(x, y).0
    }

    #[test]
    fn 도구는_사각형_색은_첫_번째가_기본이다() {
        assert_eq!(Tool::default(), Tool::Rect);
        assert_eq!(COLORS[0], "#ef4444");
        assert_eq!(COLORS.len(), 6);
    }

    #[test]
    fn base64_png_데이터_url을_이미지로_푼다() {
        let image = decode_data_url(&data_url(4, 3, [10, 20, 30, 255])).unwrap();
        assert_eq!(image.dimensions(), (4, 3));
        assert_eq!(pixel(&image, 3, 2), [10, 20, 30, 255]);
    }

    #[test]
    fn 줄바꿈이_섞이거나_패딩이_빠진_base64도_읽는다() {
        let url = data_url(1, 1, WHITE);
        let (head, data) = url.split_once(',').unwrap();
        let folded = format!("{head},{}\n{}", &data[..10], &data[10..]);
        assert!(decode_data_url(&folded).is_some());
        let unpadded = url.trim_end_matches('=');
        assert!(decode_data_url(unpadded).is_some());
        assert!(decode_data_url(&url.replacen("data:", "DATA:", 1)).is_some());
    }

    #[test]
    fn 이미지가_아닌_url은_none이다() {
        for url in [
            "",
            "data",
            "data:image/png;base64",
            "data:image/png;base64,",
            "data:image/png;base64,!!!!",
            // Valid base64 that is not an image.
            "data:image/png;base64,aGVsbG8=",
            // Not base64-encoded.
            "data:text/plain,hello",
            "https://example.com/a.png",
            "한글",
        ] {
            assert!(decode_data_url(url).is_none(), "{url:?}");
        }
    }

    #[test]
    fn 선_굵기는_너비_320px마다_1이고_최소_2다() {
        assert_eq!(line_width(0), 2.0);
        assert_eq!(line_width(320), 2.0);
        assert_eq!(line_width(799), 2.0);
        // 2.5 rounds up, as `Math.round` does.
        assert_eq!(line_width(800), 3.0);
        assert_eq!(line_width(1119), 3.0);
        assert_eq!(line_width(1120), 4.0);
        assert_eq!(line_width(1600), 5.0);
    }

    #[test]
    fn 화면_좌표를_캔버스_픽셀로_바꾼다() {
        // A 1600×1000 canvas shown at half size, 100px in and 50px down.
        let rect = (100.0, 50.0, 800.0, 500.0);
        assert_eq!(
            to_canvas_point((100.0, 50.0), rect, (1600, 1000)),
            at(0.0, 0.0)
        );
        assert_eq!(
            to_canvas_point((500.0, 300.0), rect, (1600, 1000)),
            at(800.0, 500.0)
        );
        // Outside the canvas the point is outside too: the pointer is captured.
        assert_eq!(
            to_canvas_point((0.0, 600.0), rect, (1600, 1000)),
            at(-200.0, 1100.0)
        );
    }

    #[test]
    fn 누르면_도구에_맞는_도형이_그_자리에서_시작한다() {
        let p = at(10.0, 20.0);
        assert_eq!(
            begin_shape(Tool::Rect, "#ef4444", p),
            Shape::Rect {
                color: "#ef4444",
                x: 10.0,
                y: 20.0,
                w: 0.0,
                h: 0.0
            }
        );
        assert_eq!(
            begin_shape(Tool::Arrow, "#111111", p),
            Shape::Arrow {
                color: "#111111",
                x1: 10.0,
                y1: 20.0,
                x2: 10.0,
                y2: 20.0
            }
        );
        assert_eq!(
            begin_shape(Tool::Pen, "#ffffff", p),
            Shape::Pen {
                color: "#ffffff",
                points: vec![p]
            }
        );
    }

    #[test]
    fn 끌면_사각형은_크기가_화살표는_끝점이_펜은_점이_늘어난다() {
        let mut rect = begin_shape(Tool::Rect, "#ef4444", at(10.0, 20.0));
        extend_shape(&mut rect, at(4.0, 50.0));
        // Dragging up or left gives a negative size.
        assert!(
            matches!(rect, Shape::Rect { x, y, w, h, .. } if (x, y, w, h) == (10.0, 20.0, -6.0, 30.0))
        );

        let mut arrow = begin_shape(Tool::Arrow, "#ef4444", at(10.0, 20.0));
        extend_shape(&mut arrow, at(30.0, 5.0));
        extend_shape(&mut arrow, at(40.0, 8.0));
        assert!(
            matches!(arrow, Shape::Arrow { x1, y1, x2, y2, .. } if (x1, y1, x2, y2) == (10.0, 20.0, 40.0, 8.0))
        );

        let mut pen = begin_shape(Tool::Pen, "#ef4444", at(1.0, 1.0));
        extend_shape(&mut pen, at(2.0, 2.0));
        extend_shape(&mut pen, at(2.0, 2.0));
        assert!(matches!(pen, Shape::Pen { ref points, .. } if points.len() == 3));
    }

    #[test]
    fn 끌지_않은_사각형과_펜은_버리고_화살표는_남긴다() {
        let rect = |w: f32, h: f32| Shape::Rect {
            color: "#ef4444",
            x: 0.0,
            y: 0.0,
            w,
            h,
        };
        assert!(!is_kept(&rect(0.0, 0.0)));
        assert!(!is_kept(&rect(2.9, -2.9)));
        // One side of 3px is enough.
        assert!(is_kept(&rect(3.0, 0.0)));
        assert!(is_kept(&rect(0.0, -3.0)));

        assert!(!is_kept(&begin_shape(Tool::Pen, "#ef4444", at(5.0, 5.0))));
        let mut pen = begin_shape(Tool::Pen, "#ef4444", at(5.0, 5.0));
        extend_shape(&mut pen, at(5.0, 5.0));
        assert!(is_kept(&pen));

        assert!(is_kept(&begin_shape(Tool::Arrow, "#ef4444", at(5.0, 5.0))));
    }

    #[test]
    fn 사각형은_네_변_펜은_점_사이의_선이다() {
        let rect = Shape::Rect {
            color: "#ef4444",
            x: 10.0,
            y: 20.0,
            w: 30.0,
            h: 40.0,
        };
        assert_eq!(
            segments(&rect, 2.0),
            [
                (at(10.0, 20.0), at(40.0, 20.0)),
                (at(40.0, 20.0), at(40.0, 60.0)),
                (at(40.0, 60.0), at(10.0, 60.0)),
                (at(10.0, 60.0), at(10.0, 20.0)),
            ]
        );
        let pen = Shape::Pen {
            color: "#ef4444",
            points: vec![at(0.0, 0.0), at(5.0, 5.0), at(9.0, 5.0)],
        };
        assert_eq!(
            segments(&pen, 2.0),
            [(at(0.0, 0.0), at(5.0, 5.0)), (at(5.0, 5.0), at(9.0, 5.0))]
        );
        let dot = Shape::Pen {
            color: "#ef4444",
            points: vec![at(0.0, 0.0)],
        };
        assert!(segments(&dot, 2.0).is_empty());
    }

    #[test]
    fn 화살촉은_선_굵기의_네_배_길이로_30도씩_벌어진다() {
        let arrow = Shape::Arrow {
            color: "#ef4444",
            x1: 10.0,
            y1: 50.0,
            x2: 110.0,
            y2: 50.0,
        };
        let lines = segments(&arrow, 2.0);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], (at(10.0, 50.0), at(110.0, 50.0)));
        // 8px barbs back from the tip: 8·cos30° = 6.93 along, 8·sin30° = 4 across.
        for (line, across) in [(lines[1], 54.0), (lines[2], 46.0)] {
            assert_eq!(line.0, at(110.0, 50.0));
            assert!((line.1.x - 103.072).abs() < 1e-3, "{line:?}");
            assert!((line.1.y - across).abs() < 1e-3, "{line:?}");
        }
    }

    #[test]
    fn 색은_여섯_자리_16진수만_읽는다() {
        assert_eq!(parse_color("#ef4444"), Some([0xef, 0x44, 0x44]));
        assert_eq!(parse_color("#FFFFFF"), Some([255, 255, 255]));
        for bad in ["", "#", "ef4444", "#fff", "#gggggg", "#ef44445", "#한글"] {
            assert_eq!(parse_color(bad), None, "{bad:?}");
        }
        assert!(COLORS.iter().all(|color| parse_color(color).is_some()));
    }

    #[test]
    fn 사각형은_테두리만_칠한다() {
        let mut canvas = white(100, 80);
        let rect = Shape::Rect {
            color: "#ef4444",
            x: 20.0,
            y: 20.0,
            w: 40.0,
            h: 30.0,
        };
        draw_shape(&mut canvas, &rect, 2.0);
        // A 2px line centered on y = 20 fills rows 19 and 20.
        assert_eq!(pixel(&canvas, 40, 19), RED);
        assert_eq!(pixel(&canvas, 40, 20), RED);
        assert_eq!(pixel(&canvas, 40, 17), WHITE);
        assert_eq!(pixel(&canvas, 40, 22), WHITE);
        // The other three sides.
        assert_eq!(pixel(&canvas, 40, 50), RED);
        assert_eq!(pixel(&canvas, 20, 35), RED);
        assert_eq!(pixel(&canvas, 59, 35), RED);
        // Inside and outside stay as they were.
        assert_eq!(pixel(&canvas, 40, 35), WHITE);
        assert_eq!(pixel(&canvas, 70, 35), WHITE);
    }

    #[test]
    fn 거꾸로_끈_사각형도_같은_자리에_그린다() {
        let (mut forward, mut backward) = (white(100, 80), white(100, 80));
        let rect = |x: f32, y: f32, w: f32, h: f32| Shape::Rect {
            color: "#3b82f6",
            x,
            y,
            w,
            h,
        };
        draw_shape(&mut forward, &rect(20.0, 20.0, 40.0, 30.0), 3.0);
        draw_shape(&mut backward, &rect(60.0, 50.0, -40.0, -30.0), 3.0);
        assert_eq!(forward, backward);
    }

    #[test]
    fn 겹치는_이음매를_두_번_칠하지_않는다() {
        // The corner pixel is covered by two sides; its colour is the plain stroke colour.
        let mut canvas = white(60, 60);
        let pen = Shape::Pen {
            color: "#ef4444",
            points: vec![at(10.0, 30.0), at(30.0, 30.0), at(30.0, 10.0)],
        };
        draw_shape(&mut canvas, &pen, 4.0);
        assert_eq!(pixel(&canvas, 29, 29), RED);
        assert_eq!(pixel(&canvas, 30, 30), RED);
        // The round join's antialiased rim is a blend, never darker than either colour.
        let rim = pixel(&canvas, 31, 31);
        assert!(rim[1] >= 0x44 && rim[0] >= 0xef, "{rim:?}");
    }

    #[test]
    fn 길이가_0인_화살표는_화살촉만_남는다() {
        let mut canvas = white(60, 60);
        let click = begin_shape(Tool::Arrow, "#ef4444", at(30.0, 30.0));
        draw_shape(&mut canvas, &click, 2.0);
        // atan2(0, 0) is 0: the barbs open to the left of the point.
        assert_eq!(pixel(&canvas, 27, 31), RED);
        assert_eq!(pixel(&canvas, 27, 28), RED);
        assert_eq!(pixel(&canvas, 34, 30), WHITE);
    }

    #[test]
    fn 움직이지_않은_펜은_점도_남기지_않는다() {
        let mut canvas = white(20, 20);
        let pen = Shape::Pen {
            color: "#ef4444",
            points: vec![at(10.0, 10.0), at(10.0, 10.0), at(10.0, 10.0)],
        };
        draw_shape(&mut canvas, &pen, 4.0);
        assert_eq!(canvas, white(20, 20));
    }

    #[test]
    fn 캔버스_밖이나_숫자가_아닌_좌표로도_죽지_않는다() {
        let mut canvas = white(20, 20);
        for (x2, y2) in [
            (1e30, -1e30),
            (f32::NAN, 5.0),
            (f32::INFINITY, f32::NEG_INFINITY),
            (-500.0, -500.0),
        ] {
            let arrow = Shape::Arrow {
                color: "#ef4444",
                x1: 10.0,
                y1: 10.0,
                x2,
                y2,
            };
            draw_shape(&mut canvas, &arrow, 2.0);
        }
        let far = Shape::Rect {
            color: "#ef4444",
            x: 500.0,
            y: 500.0,
            w: 50.0,
            h: 50.0,
        };
        let mut untouched = white(20, 20);
        draw_shape(&mut untouched, &far, 2.0);
        assert_eq!(untouched, white(20, 20));
        // An empty canvas has no pixels to touch.
        draw_shape(&mut RgbaImage::new(0, 0), &far, 2.0);
    }

    #[test]
    fn 모르는_색의_도형은_그리지_않는다() {
        let mut canvas = white(20, 20);
        let rect = Shape::Rect {
            color: "red",
            x: 2.0,
            y: 2.0,
            w: 10.0,
            h: 10.0,
        };
        draw_shape(&mut canvas, &rect, 2.0);
        assert_eq!(canvas, white(20, 20));
    }

    #[test]
    fn 투명한_바탕에_그린_선은_불투명해진다() {
        let mut canvas = RgbaImage::new(20, 20);
        let pen = Shape::Pen {
            color: "#ef4444",
            points: vec![at(2.0, 10.0), at(18.0, 10.0)],
        };
        draw_shape(&mut canvas, &pen, 2.0);
        assert_eq!(pixel(&canvas, 10, 10), RED);
        assert_eq!(pixel(&canvas, 10, 5), [0, 0, 0, 0]);
    }

    #[test]
    fn 캔버스는_이미지_위에_도형을_순서대로_그리고_그리는_중인_것을_맨_위에_둔다() {
        let image = white(40, 40);
        let line = |color: &'static str| Shape::Pen {
            color,
            points: vec![at(5.0, 20.0), at(35.0, 20.0)],
        };
        let shapes = [line("#ef4444"), line("#22c55e")];
        let canvas = render_canvas(Some(&image), &shapes, None, 2.0);
        assert_eq!(pixel(&canvas, 20, 20), [0x22, 0xc5, 0x5e, 255]);
        let drafting = line("#111111");
        let canvas = render_canvas(Some(&image), &shapes, Some(&drafting), 2.0);
        assert_eq!(pixel(&canvas, 20, 20), [0x11, 0x11, 0x11, 255]);
        // The capture itself is not drawn on.
        assert_eq!(image, white(40, 40));
    }

    #[test]
    fn 이미지가_없으면_빈_300x150_캔버스다() {
        let shape = Shape::Rect {
            color: "#ef4444",
            x: 10.0,
            y: 10.0,
            w: 50.0,
            h: 50.0,
        };
        let canvas = render_canvas(None, &[shape], None, 3.0);
        assert_eq!(canvas, RgbaImage::new(300, 150));
    }

    #[test]
    fn png로_저장한_캔버스는_다시_읽으면_같다() {
        let mut canvas = white(30, 20);
        canvas.put_pixel(3, 4, Rgba([1, 2, 3, 128]));
        let png = encode_png(&canvas).unwrap();
        assert_eq!(png.get(..8), Some(&b"\x89PNG\r\n\x1a\n"[..]));
        assert_eq!(image::load_from_memory(&png).unwrap().into_rgba8(), canvas);
    }

    #[test]
    fn 작은_캔버스는_제_크기로_큰_것은_비율을_지켜_줄여_보인다() {
        let viewport = (1000.0, 800.0);
        assert_eq!(fit_size((320, 200), viewport), (320.0, 200.0));
        // Too wide: 90vw = 900.
        assert_eq!(fit_size((1800, 400), viewport), (900.0, 200.0));
        // Too tall: 78vh = 624.
        assert_eq!(fit_size((400, 1248), viewport), (200.0, 624.0));
        // Both: the tighter limit decides.
        let (w, h) = fit_size((3600, 3120), viewport);
        assert!((w - 720.0).abs() < 1e-3 && (h - 624.0).abs() < 1e-3);
        assert_eq!(fit_size((0, 0), viewport), (0.0, 0.0));
    }

    #[test]
    fn 저장_파일_이름은_밀리초_시각을_담는다() {
        assert_eq!(
            file_name(1_700_000_000_000),
            "rever-markup-1700000000000.png"
        );
    }
}

#[cfg(test)]
mod view_tests {
    use std::cell::RefCell;

    use gpui::{
        ClipboardEntry, Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, size,
    };

    use super::fixtures::data_url;
    use super::*;
    use crate::ui::testing::{center, look};

    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const RED: [u8; 4] = [0xef, 0x44, 0x44, 255];

    /// The app under the overlay: counts the presses that get through to it.
    struct Stage {
        editor: Entity<MarkupEditor>,
        presses: Rc<Cell<usize>>,
    }

    impl Render for Stage {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let presses = self.presses.clone();
            div()
                .size_full()
                .on_any_mouse_down(move |_, _, _| presses.set(presses.get() + 1))
                .child(self.editor.clone())
        }
    }

    struct Fixture {
        editor: Entity<MarkupEditor>,
        events: Rc<RefCell<Vec<MarkupEvent>>>,
        presses: Rc<Cell<usize>>,
    }

    /// An editor on `url` in a 1000×800 window.
    fn open(url: String, cx: &mut TestAppContext) -> (Fixture, &mut VisualTestContext) {
        let events = Rc::new(RefCell::new(Vec::new()));
        let presses = Rc::new(Cell::new(0));
        let (stage, cx) = cx.add_window_view({
            let (events, presses) = (events.clone(), presses.clone());
            move |_, cx| {
                let editor = cx.new(|cx| MarkupEditor::new(&url, look(), cx));
                cx.subscribe(&editor, move |_, _, event: &MarkupEvent, _| {
                    events.borrow_mut().push(event.clone())
                })
                .detach();
                Stage { editor, presses }
            }
        });
        cx.simulate_resize(size(px(1000.0), px(800.0)));
        cx.run_until_parked();
        let editor = stage.read_with(cx, |stage, _| stage.editor.clone());
        let fixture = Fixture {
            editor,
            events,
            presses,
        };
        (fixture, cx)
    }

    /// A white 320×200 capture: shown at its own size, strokes 2px wide.
    fn small(cx: &mut TestAppContext) -> (Fixture, &mut VisualTestContext) {
        open(data_url(320, 200, WHITE), cx)
    }

    impl Fixture {
        fn shapes(&self, cx: &mut VisualTestContext) -> Vec<Shape> {
            self.editor
                .read_with(cx, |editor, _| editor.shapes().to_vec())
        }

        fn canvas(&self, cx: &mut VisualTestContext) -> RgbaImage {
            self.editor.read_with(cx, |editor, _| editor.canvas())
        }
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let at = center(selector, cx);
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
    }

    /// Press at `from` and release at `to`, both in canvas pixels of an unscaled canvas.
    fn drag(from: (f32, f32), to: (f32, f32), cx: &mut VisualTestContext) {
        let origin = cx.debug_bounds("markup-canvas").unwrap().origin;
        let at = |(x, y): (f32, f32)| point(origin.x + px(x), origin.y + px(y));
        cx.simulate_mouse_down(at(from), MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(at(to), MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(at(to), MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
    }

    fn close_to(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[gpui::test]
    fn 창을_덮고_도구_막대와_캔버스를_가운데에_둔다(cx: &mut TestAppContext) {
        let (_, cx) = small(cx);
        let backdrop = cx.debug_bounds("markup-backdrop").unwrap();
        assert_eq!(backdrop.origin, point(px(0.0), px(0.0)));
        assert_eq!(backdrop.size, size(px(1000.0), px(800.0)));
        let toolbar = cx.debug_bounds("markup-toolbar").unwrap();
        let canvas = cx.debug_bounds("markup-canvas").unwrap();
        assert_eq!(canvas.size, size(px(320.0), px(200.0)));
        assert!(close_to(canvas.center().x.into(), 500.0));
        assert!(close_to(toolbar.center().x.into(), 500.0));
        // 10px between the two, and the pair centered in the window.
        assert!(close_to((canvas.top() - toolbar.bottom()).into(), 10.0));
        assert!(close_to(
            toolbar.top().into(),
            (px(800.0) - canvas.bottom()).into()
        ));
        assert_eq!(
            cx.debug_bounds("markup-rect").unwrap().size,
            size(px(30.0), px(26.0))
        );
        assert_eq!(
            cx.debug_bounds("markup-color-0").unwrap().size,
            size(px(18.0), px(18.0))
        );
        // 16px line, 5px padding and the border on each side.
        assert_eq!(
            cx.debug_bounds("markup-copy").unwrap().size.height,
            px(28.0)
        );
    }

    #[gpui::test]
    fn 큰_캡처는_창의_90퍼센트_78퍼센트_안으로_줄여_보인다(
        cx: &mut TestAppContext,
    ) {
        let (f, cx) = open(data_url(1800, 400, WHITE), cx);
        let canvas = cx.debug_bounds("markup-canvas").unwrap();
        assert_eq!(canvas.size, size(px(900.0), px(200.0)));
        // 1800 / 320 rounds to 6.
        assert_eq!(f.editor.read_with(cx, |editor, _| editor.line_width), 6.0);

        // A press in the middle of the shown canvas is the middle of the real one.
        cx.simulate_mouse_down(canvas.center(), MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(canvas.origin, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(canvas.origin, MouseButton::Left, Modifiers::none());
        let shapes = f.shapes(cx);
        assert!(
            matches!(shapes[..], [Shape::Rect { x, y, w, h, .. }]
                if close_to(x, 900.0) && close_to(y, 200.0) && close_to(w, -900.0) && close_to(h, -200.0)),
            "{shapes:?}"
        );
    }

    #[gpui::test]
    fn 끌면_사각형이_그려지고_캔버스에_남는다(cx: &mut TestAppContext) {
        let (f, cx) = small(cx);
        drag((80.0, 50.0), (160.0, 100.0), cx);
        let shapes = f.shapes(cx);
        assert!(
            matches!(shapes[..], [Shape::Rect { color: "#ef4444", x, y, w, h }]
                if close_to(x, 80.0) && close_to(y, 50.0) && close_to(w, 80.0) && close_to(h, 50.0)),
            "{shapes:?}"
        );
        let canvas = f.canvas(cx);
        assert_eq!(canvas.dimensions(), (320, 200));
        assert_eq!(canvas.get_pixel(120, 50).0, RED);
        assert_eq!(canvas.get_pixel(120, 75).0, WHITE);
    }

    #[gpui::test]
    fn 그리는_중에는_포인터가_캔버스를_벗어나도_따라간다(
        cx: &mut TestAppContext,
    ) {
        let (f, cx) = small(cx);
        let canvas = cx.debug_bounds("markup-canvas").unwrap();
        let start = point(canvas.left() + px(80.0), canvas.top() + px(50.0));
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        // Over the backdrop, 40px left of the canvas.
        let outside = point(canvas.left() - px(40.0), canvas.top() + px(100.0));
        cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::none());
        // The shape in progress is on the canvas already.
        assert_eq!(f.canvas(cx).get_pixel(40, 50).0, RED);
        assert!(f.shapes(cx).is_empty());
        cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let shapes = f.shapes(cx);
        assert!(
            matches!(shapes[..], [Shape::Rect { w, h, .. }] if close_to(w, -120.0) && close_to(h, 50.0)),
            "{shapes:?}"
        );
        // Released over the backdrop, but the press began on the canvas: not a close.
        assert_eq!(f.events.take(), []);

        // No button down: moving over the canvas draws nothing.
        cx.simulate_mouse_move(canvas.center(), None, Modifiers::none());
        assert_eq!(f.shapes(cx).len(), 1);
    }

    #[gpui::test]
    fn 다음_프레임이_그려지기_전에_버튼을_떼도_그리기가_끝난다(
        cx: &mut TestAppContext,
    ) {
        let (f, cx) = small(cx);
        let canvas = cx.debug_bounds("markup-canvas").unwrap();
        let at = point(canvas.left() + px(80.0), canvas.top() + px(50.0));
        // A quick tap: the release reaches the window before it draws again, so it goes
        // to the listeners of the frame drawn before the press. The tests' window redraws
        // after every event, so the press is made here without one.
        f.editor.update(cx, |editor, _| {
            let p = editor.canvas_point(at).unwrap();
            editor.drafting = Some(begin_shape(Tool::Rect, COLORS[0], p));
        });
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        assert!(
            f.editor
                .read_with(cx, |editor, _| editor.drafting.is_none())
        );
        // No button is down any more: the pointer draws nothing.
        cx.simulate_mouse_move(canvas.center(), None, Modifiers::none());
        assert_eq!(f.canvas(cx), RgbaImage::from_pixel(320, 200, Rgba(WHITE)));
        assert!(f.shapes(cx).is_empty());
    }

    #[gpui::test]
    fn 새_캡처가_오면_그린_도형은_두고_이미지만_바꾼다(
        cx: &mut TestAppContext,
    ) {
        let (f, cx) = small(cx);
        click("markup-pen", cx);
        drag((80.0, 50.0), (160.0, 50.0), cx);
        let blue = [0x10, 0x20, 0xf0, 255];
        f.editor.update_in(cx, |editor, window, cx| {
            editor.set_image(&data_url(1600, 400, blue), window, cx)
        });
        cx.run_until_parked();
        // The canvas takes the new capture's size, and the line width follows it.
        let canvas = f.canvas(cx);
        assert_eq!(canvas.dimensions(), (1600, 400));
        assert_eq!(f.editor.read_with(cx, |editor, _| editor.line_width), 5.0);
        assert_eq!(
            cx.debug_bounds("markup-canvas").unwrap().size,
            size(px(900.0), px(225.0))
        );
        // Tool, colour and shapes stay; the stroke is where it was, in canvas pixels.
        assert_eq!(f.editor.read_with(cx, |editor, _| editor.tool()), Tool::Pen);
        assert_eq!(f.shapes(cx).len(), 1);
        assert_eq!(canvas.get_pixel(120, 50).0, RED);
        assert_eq!(canvas.get_pixel(120, 200).0, blue);

        // A URL that is no image never loads: the editor stays as it is.
        f.editor.update_in(cx, |editor, window, cx| {
            editor.set_image("data:image/png;base64,aGVsbG8=", window, cx)
        });
        assert_eq!(f.canvas(cx), canvas);
    }

    #[gpui::test]
    fn 끌지_않은_클릭은_사각형과_펜을_남기지_않고_화살표는_남긴다(
        cx: &mut TestAppContext,
    ) {
        let (f, cx) = small(cx);
        drag((80.0, 50.0), (82.0, 52.0), cx);
        assert!(f.shapes(cx).is_empty());
        // A few pixels one way is a rectangle.
        drag((80.0, 50.0), (85.0, 50.0), cx);
        assert_eq!(f.shapes(cx).len(), 1);

        click("markup-pen", cx);
        let at = center("markup-canvas", cx);
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
        assert_eq!(f.shapes(cx).len(), 1);

        click("markup-arrow", cx);
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
        let shapes = f.shapes(cx);
        assert!(matches!(shapes[..], [_, Shape::Arrow { .. }]), "{shapes:?}");
    }

    #[gpui::test]
    fn 도구와_색을_고르면_다음_도형에_쓰인다(cx: &mut TestAppContext) {
        let (f, cx) = small(cx);
        assert_eq!(
            f.editor.read_with(cx, |e, _| (e.tool(), e.color())),
            (Tool::Rect, "#ef4444")
        );
        click("markup-pen", cx);
        click("markup-color-3", cx);
        assert_eq!(
            f.editor.read_with(cx, |e, _| (e.tool(), e.color())),
            (Tool::Pen, "#3b82f6")
        );
        drag((80.0, 50.0), (160.0, 100.0), cx);
        let shapes = f.shapes(cx);
        assert!(
            matches!(&shapes[..], [Shape::Pen { color: "#3b82f6", points }] if points.len() == 2),
            "{shapes:?}"
        );
        click("markup-arrow", cx);
        click("markup-color-5", cx);
        drag((10.0, 10.0), (60.0, 10.0), cx);
        let shapes = f.shapes(cx);
        assert!(
            matches!(
                shapes[..],
                [
                    _,
                    Shape::Arrow {
                        color: "#111111",
                        ..
                    }
                ]
            ),
            "{shapes:?}"
        );
        // Earlier shapes keep the colour they were drawn in.
        assert_eq!(f.canvas(cx).get_pixel(30, 10).0, [0x11, 0x11, 0x11, 255]);
        assert_eq!(f.canvas(cx).get_pixel(120, 75).0, [0x3b, 0x82, 0xf6, 255]);
    }

    #[gpui::test]
    fn 실행_취소는_마지막_도형을_지우기는_전부를_지운다(
        cx: &mut TestAppContext,
    ) {
        let (f, cx) = small(cx);
        // Nothing drawn: both buttons are disabled.
        click("markup-undo", cx);
        click("markup-clear", cx);
        assert!(f.shapes(cx).is_empty());

        drag((20.0, 20.0), (60.0, 60.0), cx);
        drag((100.0, 20.0), (160.0, 60.0), cx);
        drag((200.0, 20.0), (260.0, 60.0), cx);
        click("markup-undo", cx);
        assert_eq!(f.shapes(cx).len(), 2);
        assert_eq!(f.canvas(cx).get_pixel(230, 20).0, WHITE);
        assert_eq!(f.canvas(cx).get_pixel(130, 20).0, RED);
        click("markup-clear", cx);
        assert!(f.shapes(cx).is_empty());
        assert_eq!(f.canvas(cx), RgbaImage::from_pixel(320, 200, Rgba(WHITE)));
    }

    #[gpui::test]
    fn 복사는_캔버스를_png로_클립보드에_넣고_잠시_복사됨을_보인다(
        cx: &mut TestAppContext,
    ) {
        let (f, cx) = small(cx);
        drag((80.0, 50.0), (160.0, 100.0), cx);
        click("markup-copy", cx);
        let item = cx.read_from_clipboard().unwrap();
        let [ClipboardEntry::Image(image)] = item.entries() else {
            panic!("expected one image on the clipboard");
        };
        assert_eq!(image.format, ImageFormat::Png);
        let copied = image::load_from_memory(image.bytes()).unwrap().into_rgba8();
        assert_eq!(copied, f.canvas(cx));
        assert_eq!(copied.get_pixel(120, 50).0, RED);

        let copied_label =
            |f: &Fixture, cx: &mut VisualTestContext| f.editor.read_with(cx, |e, _| e.copied);
        assert!(copied_label(&f, cx));
        cx.executor().advance_clock(Duration::from_millis(1499));
        cx.run_until_parked();
        assert!(copied_label(&f, cx));
        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        assert!(!copied_label(&f, cx));
    }

    #[gpui::test]
    fn 저장은_파일_이름과_png를_호스트에_넘긴다(cx: &mut TestAppContext) {
        let (f, cx) = small(cx);
        drag((80.0, 50.0), (160.0, 100.0), cx);
        click("markup-save", cx);
        let events = f.events.take();
        let [MarkupEvent::Save { file_name, png }] = &events[..] else {
            panic!("expected one save, got {events:?}");
        };
        let stamp = file_name
            .strip_prefix("rever-markup-")
            .and_then(|rest| rest.strip_suffix(".png"))
            .unwrap();
        assert!(stamp.parse::<u64>().unwrap() > 1_700_000_000_000);
        assert_eq!(
            image::load_from_memory(png).unwrap().into_rgba8(),
            f.canvas(cx)
        );
    }

    #[gpui::test]
    fn 닫기_버튼과_배경_클릭은_닫기를_알린다(cx: &mut TestAppContext) {
        let (f, cx) = small(cx);
        click("markup-close", cx);
        assert_eq!(f.events.take(), [MarkupEvent::Close]);
        cx.simulate_click(point(px(30.0), px(30.0)), Modifiers::none());
        assert_eq!(f.events.take(), [MarkupEvent::Close]);
        // The toolbar's own surface and the canvas are not the backdrop.
        let toolbar = cx.debug_bounds("markup-toolbar").unwrap();
        cx.simulate_click(
            point(toolbar.left() + px(4.0), toolbar.top() + px(4.0)),
            Modifiers::none(),
        );
        click("markup-canvas", cx);
        assert_eq!(f.events.take(), []);
        // Nothing reached the app underneath.
        assert_eq!(f.presses.get(), 0);
    }

    #[gpui::test]
    fn 읽지_못한_이미지는_빈_캔버스로_남는다(cx: &mut TestAppContext) {
        let (f, cx) = open("data:image/png;base64,aGVsbG8=".into(), cx);
        let canvas = cx.debug_bounds("markup-canvas").unwrap();
        assert_eq!(canvas.size, size(px(300.0), px(150.0)));
        // Shapes are still recorded (undo lights up), but nothing is ever drawn.
        drag((20.0, 20.0), (60.0, 60.0), cx);
        assert_eq!(f.shapes(cx).len(), 1);
        assert_eq!(f.canvas(cx), RgbaImage::new(300, 150));
        click("markup-copy", cx);
        let item = cx.read_from_clipboard().unwrap();
        let [ClipboardEntry::Image(image)] = item.entries() else {
            panic!("expected one image on the clipboard");
        };
        assert_eq!(
            image::load_from_memory(image.bytes()).unwrap().into_rgba8(),
            RgbaImage::new(300, 150)
        );
    }
}
