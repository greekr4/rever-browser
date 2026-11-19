//! Port of `src/renderer/src/components/chat/ProviderIcon.tsx` — real brand marks for
//! the providers, replacing the C/A/O/X letter avatars.
//!
//! Claude surfaces (`claude-code`, `anthropic`) → the Claude sunburst; OpenAI surfaces
//! (`codex`, `openai`) → the OpenAI knot; `grok` → the Grok mark; `glm` → the Z.ai mark.
//! Grok and Z.ai paths: @lobehub/icons-static-svg 1.102.0 (MIT), grok.svg / zai.svg.
//!
//! The component returned inline SVG. Here the same SVG document is built as text and
//! drawn through GPUI's image element: its `svg()` element only loads from the app's
//! asset source, and tints everything one color.

use gpui::{AnyElement, FontWeight, Rgba, div, prelude::*, px};

use crate::ui::{svg_image, svg_open, svg_paint};

const CLAUDE_CLAY: Rgba = Rgba {
    r: 0xd9 as f32 / 255.0,
    g: 0x77 as f32 / 255.0,
    b: 0x57 as f32 / 255.0,
    a: 1.0,
};

const GROK_PATH: &str = "M9.27 15.29l7.978-5.897c.391-.29.95-.177 1.137.272.98 2.369.542 5.215-1.41 7.169-1.951 1.954-4.667 2.382-7.149 1.406l-2.711 1.257c3.889 2.661 8.611 2.003 11.562-.953 2.341-2.344 3.066-5.539 2.388-8.42l.006.007c-.983-4.232.242-5.924 2.75-9.383.06-.082.12-.164.179-.248l-3.301 3.305v-.01L9.267 15.292M7.623 16.723c-2.792-2.67-2.31-6.801.071-9.184 1.761-1.763 4.647-2.483 7.166-1.425l2.705-1.25a7.808 7.808 0 00-1.829-1A8.975 8.975 0 005.984 5.83c-2.533 2.536-3.33 6.436-1.962 9.764 1.022 2.487-.653 4.246-2.34 6.022-.599.63-1.199 1.259-1.682 1.925l7.62-6.815";
const GLM_PATH: &str = "M12.105 2L9.927 4.953H.653L2.83 2h9.276zM23.254 19.048L21.078 22h-9.242l2.174-2.952h9.244zM24 2L9.264 22H0L14.736 2H24z";
const OPENAI_PATH: &str = "M22.282 9.821a5.985 5.985 0 0 0-.516-4.91 6.046 6.046 0 0 0-6.51-2.9A6.065 6.065 0 0 0 4.981 4.18a5.985 5.985 0 0 0-3.998 2.9 6.046 6.046 0 0 0 .743 7.097 5.98 5.98 0 0 0 .51 4.911 6.051 6.051 0 0 0 6.515 2.9A5.985 5.985 0 0 0 13.26 24a6.056 6.056 0 0 0 5.772-4.206 5.99 5.99 0 0 0 3.997-2.9 6.056 6.056 0 0 0-.747-7.073zM13.26 22.43a4.476 4.476 0 0 1-2.876-1.04l.141-.081 4.779-2.758a.795.795 0 0 0 .392-.681v-6.737l2.02 1.168a.071.071 0 0 1 .038.052v5.583a4.504 4.504 0 0 1-4.494 4.494zM3.6 18.304a4.47 4.47 0 0 1-.535-3.014l.142.085 4.783 2.759a.771.771 0 0 0 .78 0l5.843-3.369v2.332a.08.08 0 0 1-.033.062L9.74 19.95a4.5 4.5 0 0 1-6.14-1.646zM2.34 7.896a4.485 4.485 0 0 1 2.366-1.973V11.6a.766.766 0 0 0 .388.676l5.815 3.355-2.02 1.168a.076.076 0 0 1-.071.006l-4.83-2.786A4.504 4.504 0 0 1 2.34 7.872zm16.597 3.855l-5.833-3.387L15.119 7.15a.076.076 0 0 1 .071-.006l4.83 2.791a4.494 4.494 0 0 1-.676 8.105v-5.678a.79.79 0 0 0-.407-.667zm2.01-3.023l-.141-.085-4.774-2.782a.776.776 0 0 0-.785 0L9.409 9.23V6.897a.066.066 0 0 1 .028-.061l4.83-2.787a4.5 4.5 0 0 1 6.68 4.66zM8.3 12.863l-2.02-1.164a.08.08 0 0 1-.038-.057V6.075a4.5 4.5 0 0 1 7.375-3.453l-.142.08L8.704 5.46a.795.795 0 0 0-.393.681zm1.097-2.365l2.602-1.5 2.607 1.5v2.999l-2.597 1.5-2.607-1.5z";

// The Claude mark is a radial burst of tapered blades in alternating long/short
// lengths around a small solid core. Generated (rather than a hand-copied path)
// so it stays crisp at any size.
const CLAUDE_RAY_COUNT: u32 = 12;
const CLAUDE_BASE_LEN: f64 = 9.7;
const CLAUDE_HALF_WIDTH: f64 = 2.0;

// Full body near the base narrowing to a pointed tip.
pub fn blade_path(len: f64) -> String {
    let c = 12.0;
    let tip = c - len;
    let hw = CLAUDE_HALF_WIDTH;
    format!(
        "M {c} {c} C {} {}, {} {}, {c} {tip} C {} {}, {} {}, {c} {c} Z",
        c - hw,
        c - len * 0.25,
        c - hw * 0.3,
        c - len * 0.82,
        c + hw * 0.3,
        c - len * 0.82,
        c + hw,
        c - len * 0.25,
    )
}

/// Which mark an agent id gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Claude,
    /// A one-color path, drawn in the surrounding text color.
    Mono {
        path: &'static str,
        even_odd: bool,
    },
    /// No brand mark: the fallback letter.
    Letter,
}

/// Takes the id as text, so an id the app does not know still gets the letter.
pub fn mark(agent_id: &str) -> Mark {
    match agent_id {
        "anthropic" | "claude-code" => Mark::Claude,
        "openai" | "codex" => Mark::Mono {
            path: OPENAI_PATH,
            even_odd: false,
        },
        "grok" => Mark::Mono {
            path: GROK_PATH,
            even_odd: true,
        },
        "glm" => Mark::Mono {
            path: GLM_PATH,
            even_odd: true,
        },
        _ => Mark::Letter,
    }
}

fn filled(size: f32, color: Rgba) -> String {
    let (paint, opacity) = svg_paint(color);
    svg_open(
        size,
        &format!("fill=\"{paint}\" fill-opacity=\"{opacity}\""),
    )
}

/// The Claude sunburst as an SVG document for an icon `size` CSS pixels wide.
pub fn claude_spark_svg(size: f32) -> String {
    let step = 360.0 / f64::from(CLAUDE_RAY_COUNT);
    let mut svg = filled(size, CLAUDE_CLAY);
    for i in 0..CLAUDE_RAY_COUNT {
        let len = CLAUDE_BASE_LEN + if i % 2 == 0 { 1.2 } else { -1.3 };
        svg.push_str(&format!(
            "<path d=\"{}\" transform=\"rotate({} 12 12)\"/>",
            blade_path(len),
            f64::from(i) * step
        ));
    }
    svg.push_str("<circle cx=\"12\" cy=\"12\" r=\"1.7\"/></svg>");
    svg
}

/// A one-path mark in `color` (the component's `currentColor`).
pub fn mono_svg(path: &str, even_odd: bool, size: f32, color: Rgba) -> String {
    let rule = if even_odd {
        " fill-rule=\"evenodd\""
    } else {
        ""
    };
    format!("{}<path d=\"{path}\"{rule}/></svg>", filled(size, color))
}

/// The font size of the fallback letter.
pub fn letter_size(size: f32) -> f32 {
    size * 0.7
}

/// The provider's mark, `size` pixels square (the component's default was 16).
/// `color` is the text color where the icon sits: an image does not inherit it.
/// `fallback` is the letter for a provider without a brand mark.
pub fn provider_icon(agent_id: &str, size: f32, fallback: Option<&str>, color: Rgba) -> AnyElement {
    let svg = match mark(agent_id) {
        Mark::Claude => claude_spark_svg(size),
        Mark::Mono { path, even_odd } => mono_svg(path, even_odd, size, color),
        Mark::Letter => {
            return div()
                .flex_none()
                .text_size(px(letter_size(size)))
                .line_height(px(size))
                .font_weight(FontWeight::BOLD)
                .text_color(color)
                .child(fallback.unwrap_or("?").to_owned())
                .into_any_element();
        }
    };
    svg_image(svg, size).into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::rgb;

    use super::*;

    #[test]
    fn 날의_경로는_컴포넌트가_만드는_문자열과_같다() {
        // bladePath(9.7 + 1.2) and bladePath(9.7 - 1.3) in the TypeScript (node).
        assert_eq!(
            blade_path(9.7 + 1.2),
            "M 12 12 C 10 9.275, 11.4 3.062000000000001, 12 1.1000000000000014 \
             C 12.6 3.062000000000001, 14 9.275, 12 12 Z"
        );
        assert_eq!(
            blade_path(9.7 - 1.3),
            "M 12 12 C 10 9.9, 11.4 5.112000000000002, 12 3.6000000000000014 \
             C 12.6 5.112000000000002, 14 9.9, 12 12 Z"
        );
    }

    #[test]
    fn 클로드_표시는_길고_짧은_날_열두_개와_가운데_원이다() {
        let svg = claude_spark_svg(16.0);
        assert_eq!(svg.matches("<path ").count(), 12);
        assert_eq!(svg.matches(&blade_path(9.7 + 1.2)).count(), 6);
        assert_eq!(svg.matches(&blade_path(9.7 - 1.3)).count(), 6);
        // Even rays are the long ones, 30° apart.
        assert!(svg.contains(&format!(
            "<path d=\"{}\" transform=\"rotate(0 12 12)\"/>",
            blade_path(9.7 + 1.2)
        )));
        assert!(svg.contains(&format!(
            "<path d=\"{}\" transform=\"rotate(30 12 12)\"/>",
            blade_path(9.7 - 1.3)
        )));
        assert!(svg.contains("transform=\"rotate(330 12 12)\""));
        assert!(svg.contains("<circle cx=\"12\" cy=\"12\" r=\"1.7\"/>"));
        assert!(svg.contains("viewBox=\"0 0 24 24\""));
        assert!(svg.contains("width=\"64\" height=\"64\""));
    }

    #[test]
    fn 클로드_색은_빨강과_파랑을_바꿔_적는다() {
        // #d97757 with red and blue exchanged.
        assert!(claude_spark_svg(12.0).contains("fill=\"#5777d9\" fill-opacity=\"1\""));
    }

    #[test]
    fn 에이전트마다_제_표시를_고른다() {
        assert_eq!(mark("claude-code"), Mark::Claude);
        assert_eq!(mark("anthropic"), Mark::Claude);
        let openai = Mark::Mono {
            path: OPENAI_PATH,
            even_odd: false,
        };
        assert_eq!(mark("openai"), openai);
        assert_eq!(mark("codex"), openai);
        assert_eq!(
            mark("grok"),
            Mark::Mono {
                path: GROK_PATH,
                even_odd: true
            }
        );
        assert_eq!(
            mark("glm"),
            Mark::Mono {
                path: GLM_PATH,
                even_odd: true
            }
        );
    }

    #[test]
    fn 모르는_에이전트는_글자로_대신한다() {
        assert_eq!(mark("gemini"), Mark::Letter);
        assert_eq!(mark(""), Mark::Letter);
        assert_eq!(mark("Claude-Code"), Mark::Letter);
    }

    #[test]
    fn 단색_표시는_주어진_색으로_칠하고_grok과_glm만_evenodd다() {
        let grok = mono_svg(GROK_PATH, true, 12.0, rgb(0x102030));
        assert!(grok.contains("fill=\"#302010\""));
        assert!(grok.contains(" fill-rule=\"evenodd\"/>"));
        assert!(grok.contains("width=\"48\""));
        let openai = mono_svg(OPENAI_PATH, false, 14.0, rgb(0xffffff));
        assert!(!openai.contains("fill-rule"));
        assert!(openai.contains(&format!("<path d=\"{OPENAI_PATH}\"/>")));
    }

    #[test]
    fn 반투명한_색은_fill_opacity로_옮긴다() {
        let half = Rgba {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 0.5,
        };
        assert!(
            mono_svg(GLM_PATH, true, 12.0, half).contains("fill=\"#0000ff\" fill-opacity=\"0.5\"")
        );
    }

    #[test]
    fn 글자_크기는_아이콘_크기의_70퍼센트다() {
        assert!((letter_size(16.0) - 11.2).abs() < 1e-4);
        assert!((letter_size(14.0) - 9.8).abs() < 1e-4);
    }

    #[test]
    fn 경로_데이터는_컴포넌트의_것_그대로다() {
        assert!(GROK_PATH.starts_with("M9.27 15.29l7.978-5.897"));
        assert!(GROK_PATH.ends_with("1.925l7.62-6.815"));
        assert_eq!(GROK_PATH.len(), 556);
        assert_eq!(
            GLM_PATH,
            "M12.105 2L9.927 4.953H.653L2.83 2h9.276zM23.254 19.048L21.078 22h-9.242l2.174-2.952h9.244zM24 2L9.264 22H0L14.736 2H24z"
        );
        assert!(OPENAI_PATH.starts_with("M22.282 9.821a5.985"));
        assert!(OPENAI_PATH.ends_with("-2.597 1.5-2.607-1.5z"));
        assert_eq!(OPENAI_PATH.len(), 1255);
    }
}
