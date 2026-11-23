//! Spaces: the open tabs in groups, each with a tab strip of its own, and the grid
//! that shows every space at once. This is the part that needs no window: which tabs
//! a space holds, what is shown when one closes, and how spaces are saved.

use serde_json::{Value, json};

/// The storage key of the saved spaces, next to the saved tabs (`rev:tabs`).
pub const STORE_KEY: &str = "rev:spaces";

/// The positions in the tab list of the tabs that belong to `space`.
/// `tab_spaces` holds each tab's space, in tab order.
pub fn tabs_of<S: PartialEq + Copy>(tab_spaces: &[S], space: S) -> Vec<usize> {
    tab_spaces
        .iter()
        .enumerate()
        .filter(|(_, of)| **of == space)
        .map(|(index, _)| index)
        .collect()
}

/// The tab to show once the one at `closed` is gone: its right neighbour in the same
/// space, else its left one. `None` when it was the only tab of its space.
pub fn neighbour_after_close<S: PartialEq + Copy>(
    tab_spaces: &[S],
    closed: usize,
) -> Option<usize> {
    let space = *tab_spaces.get(closed)?;
    let same = tabs_of(tab_spaces, space);
    let at = same.iter().position(|index| *index == closed)?;
    same.get(at + 1)
        .or_else(|| at.checked_sub(1).and_then(|left| same.get(left)))
        .copied()
}

/// How many tiles a row of the grid holds: large tiles while there are few.
pub fn grid_columns(tiles: usize) -> usize {
    match tiles {
        0 | 1 => 1,
        2..=4 => 2,
        _ => 3,
    }
}

/// The name of a new space: `Space N`, with the lowest N no space has yet.
pub fn new_space_name(taken: &[String]) -> String {
    (1..)
        .map(|number| format!("Space {number}"))
        .find(|name| !taken.contains(name))
        .unwrap_or_default()
}

/// The spaces as they were when the app last ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedSpaces {
    pub names: Vec<String>,
    /// For each saved tab, in order, the position of its space in `names`.
    pub of: Vec<usize>,
    /// The space that was shown.
    pub current: usize,
}

impl SavedSpaces {
    /// One space holding every tab: the state without usable saved spaces.
    pub fn single(tab_count: usize) -> Self {
        Self {
            names: vec![new_space_name(&[])],
            of: vec![0; tab_count],
            current: 0,
        }
    }
}

pub fn serialize_spaces(saved: &SavedSpaces) -> String {
    json!({ "names": saved.names, "of": saved.of, "current": saved.current }).to_string()
}

/// The saved spaces for a session of `tab_count` restored tabs. What was saved is used
/// only when it fits those tabs exactly (every tab has a space, every space a tab);
/// anything else gives one space with all the tabs.
pub fn parse_saved_spaces(raw: Option<&str>, tab_count: usize) -> SavedSpaces {
    let parsed = || -> Option<SavedSpaces> {
        let saved: Value = serde_json::from_str(raw?).ok()?;
        let names = saved["names"]
            .as_array()?
            .iter()
            .map(|name| name.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        let of = saved["of"]
            .as_array()?
            .iter()
            .map(|space| space.as_u64().map(|space| space as usize))
            .collect::<Option<Vec<_>>>()?;
        let fits = of.len() == tab_count
            && !names.is_empty()
            && (0..names.len()).all(|space| of.contains(&space))
            && of.iter().all(|space| *space < names.len());
        let current = saved["current"].as_u64().unwrap_or(0) as usize;
        fits.then(|| SavedSpaces {
            current: current.min(names.len() - 1),
            names,
            of,
        })
    };
    parsed().unwrap_or_else(|| SavedSpaces::single(tab_count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn 스페이스의_탭은_탭_순서대로_고른다() {
        assert_eq!(tabs_of(&[1, 2, 1, 1, 2], 1), [0, 2, 3]);
    }

    #[test]
    fn 탭이_없는_스페이스는_빈_목록이다() {
        assert!(tabs_of(&[1, 1], 7).is_empty());
    }

    #[test]
    fn 닫은_탭의_오른쪽_이웃이_같은_스페이스에_있으면_그_탭을_보여_준다() {
        // 1번 탭(스페이스 a)을 닫으면 사이의 b 탭을 건너 3번이 보인다.
        assert_eq!(neighbour_after_close(&['a', 'a', 'b', 'a'], 1), Some(3));
    }

    #[test]
    fn 오른쪽에_같은_스페이스의_탭이_없으면_왼쪽_이웃을_보여_준다() {
        assert_eq!(neighbour_after_close(&['a', 'b', 'a', 'b'], 2), Some(0));
    }

    #[test]
    fn 스페이스의_하나뿐인_탭을_닫으면_보여_줄_이웃이_없다() {
        assert_eq!(neighbour_after_close(&['a', 'b', 'a'], 1), None);
    }

    #[test]
    fn 타일이_적으면_두_칸씩_크게_놓는다() {
        assert_eq!(grid_columns(3), 2);
    }

    #[test]
    fn 타일이_다섯부터는_세_칸씩_놓는다() {
        assert_eq!(grid_columns(5), 3);
    }

    #[test]
    fn 새_스페이스는_쓰지_않는_가장_작은_번호를_받는다() {
        assert_eq!(new_space_name(&names(&["Space 1", "Space 3"])), "Space 2");
    }

    #[test]
    fn 저장한_스페이스는_그대로_돌아온다() {
        let saved = SavedSpaces {
            names: names(&["업무", "조사"]),
            of: vec![0, 1, 1],
            current: 1,
        };
        assert_eq!(
            parse_saved_spaces(Some(&serialize_spaces(&saved)), 3),
            saved
        );
    }

    #[test]
    fn 저장된_것이_없으면_모든_탭이_한_스페이스에_들어간다() {
        assert_eq!(parse_saved_spaces(None, 2), SavedSpaces::single(2));
    }

    #[test]
    fn 탭_수가_맞지_않는_저장은_쓰지_않는다() {
        let raw = r#"{"names":["a","b"],"of":[0,1],"current":0}"#;
        assert_eq!(parse_saved_spaces(Some(raw), 3), SavedSpaces::single(3));
    }

    #[test]
    fn 탭이_하나도_없는_스페이스가_있는_저장은_쓰지_않는다() {
        let raw = r#"{"names":["a","b"],"of":[0,0],"current":0}"#;
        assert_eq!(parse_saved_spaces(Some(raw), 2), SavedSpaces::single(2));
    }

    #[test]
    fn 없는_스페이스를_가리키는_저장은_쓰지_않는다() {
        let raw = r#"{"names":["a"],"of":[0,4],"current":0}"#;
        assert_eq!(parse_saved_spaces(Some(raw), 2), SavedSpaces::single(2));
    }

    #[test]
    fn 보던_스페이스_번호가_범위를_넘으면_마지막_스페이스다() {
        let raw = r#"{"names":["a","b"],"of":[0,1],"current":9}"#;
        assert_eq!(parse_saved_spaces(Some(raw), 2).current, 1);
    }
}
