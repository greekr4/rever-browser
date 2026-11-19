//! The interface's icons, served to GPUI as SVG. The path data is the Electron
//! app's: Port of the `ICON` table in `src/renderer/src/components/FloatingChips.tsx`,
//! plus the toolbar and composer icons inlined in App.tsx, TabBar.tsx,
//! ProfileMenu.tsx and ChatPanel.tsx.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// (name, stroke width, parts). A part is path data, `c:cx,cy,r` for a circle,
/// or `pl:points` for a polyline.
const ICONS: &[(&str, f32, &[&str])] = &[
    (
        "apimap",
        1.9,
        &["M3 6l6-3 6 3 6-3v15l-6 3-6-3-6 3z", "M9 3v15", "M15 6v15"],
    ),
    ("back", 2.0, &["M19 12H5M12 19l-7-7 7-7"]),
    ("close", 2.0, &["M18 6L6 18", "M6 6l12 12"]),
    ("console", 1.9, &["M5 17l5-5-5-5", "M12 19h7"]),
    (
        "exceptions",
        1.9,
        &[
            "M10.3 3.9L1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z",
            "M12 9v4",
            "M12 17h.01",
        ],
    ),
    ("forward", 2.0, &["M5 12h14M12 5l7 7-7 7"]),
    ("history", 1.9, &["c:12,12,9", "M12 7v5l3 2"]),
    ("more", 2.6, &["M12 5h.01", "M12 12h.01", "M12 19h.01"]),
    ("plus", 2.0, &["M12 5v14M5 12h14"]),
    ("profile", 2.0, &["c:12,8,4", "M4 21a8 8 0 0 1 16 0"]),
    ("redteam", 1.9, &["c:12,12,9", "c:12,12,5", "c:12,12,1"]),
    (
        "reload",
        2.0,
        &["pl:23 4 23 10 17 10", "M20.49 15a9 9 0 1 1-2.12-9.36L23 10"],
    ),
    (
        "repeater",
        1.9,
        &[
            "M17 2l4 4-4 4",
            "M3 11V9a3 3 0 0 1 3-3h15",
            "M7 22l-4-4 4-4",
            "M21 13v2a3 3 0 0 1-3 3H3",
        ],
    ),
    ("send", 2.4, &["M12 19V5M5 12l7-7 7 7"]),
    (
        "shield",
        2.0,
        &["M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"],
    ),
    (
        "storage",
        1.9,
        &[
            "M4 6c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3z",
            "M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6",
            "M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3",
        ],
    ),
    (
        "traffic",
        1.9,
        &["M7 4v16", "M3 8l4-4 4 4", "M17 20V4", "M13 16l4 4 4-4"],
    ),
    ("websocket", 1.9, &["M13 2L4 14h8l-1 8 9-12h-8z"]),
    (
        "workflows",
        1.9,
        &[
            "c:6,6,2.5",
            "c:6,18,2.5",
            "c:18,12,2.5",
            "M6 8.5v7",
            "M8.5 6h3a4 4 0 0 1 4 4v-.5",
        ],
    ),
    (
        "wrench",
        2.0,
        &[
            "M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z",
        ],
    ),
    (
        "wsrepeater",
        1.9,
        &[
            "M13 2L6 11h6l-1 5 7-9h-6z",
            "M7 23l-4-4 4-4",
            "M21 14v2a3 3 0 0 1-3 3H3",
        ],
    ),
];

/// The SVG document for an icon, or `None` for an unknown name.
pub fn svg_source(name: &str) -> Option<String> {
    let (_, stroke, parts) = ICONS.iter().find(|(icon, _, _)| *icon == name)?;
    let shapes: String = parts
        .iter()
        .map(|part| {
            if let Some(circle) = part.strip_prefix("c:") {
                let mut numbers = circle.split(',');
                let (cx, cy, r) = (numbers.next()?, numbers.next()?, numbers.next()?);
                Some(format!("<circle cx=\"{cx}\" cy=\"{cy}\" r=\"{r}\"/>"))
            } else if let Some(points) = part.strip_prefix("pl:") {
                Some(format!("<polyline points=\"{points}\"/>"))
            } else {
                Some(format!("<path d=\"{part}\"/>"))
            }
        })
        .collect::<Option<String>>()?;
    Some(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"black\" \
         stroke-width=\"{stroke}\" stroke-linecap=\"round\" stroke-linejoin=\"round\">{shapes}</svg>"
    ))
}

/// GPUI asset source: `icons/<name>.svg` resolves to the table above.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let name = path
            .strip_prefix("icons/")
            .and_then(|file| file.strip_suffix(".svg"));
        Ok(name
            .and_then(svg_source)
            .map(|svg| Cow::Owned(svg.into_bytes())))
    }

    fn list(&self, _: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(name, _, _)| format!("icons/{name}.svg").into())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 아이콘은_경로_원_폴리라인을_svg로_만든다() {
        let history = svg_source("history").unwrap();
        assert!(history.contains("<circle cx=\"12\" cy=\"12\" r=\"9\"/>"));
        assert!(history.contains("<path d=\"M12 7v5l3 2\"/>"));
        assert!(
            svg_source("reload")
                .unwrap()
                .contains("<polyline points=\"23 4 23 10 17 10\"/>")
        );
        assert!(svg_source("nope").is_none());
    }

    #[test]
    fn 자산_경로는_아이콘_이름으로_풀린다() {
        assert!(Assets.load("icons/traffic.svg").unwrap().is_some());
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
        assert!(Assets.load("other/traffic.svg").unwrap().is_none());
    }
}
