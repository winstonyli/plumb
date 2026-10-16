//! What plumb remembers between runs: window placement, column widths and sort, as TOML in
//! `%LOCALAPPDATA%\plumb\state.toml`. A missing or unreadable file means defaults; unknown keys are ignored.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The window's restored rectangle in screen pixels (what `GetWindowPlacement` reports).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub maximized: bool,
}

impl Placement {
    /// Whether the rectangle is a plausible window; anything smaller is a damaged file.
    pub fn is_sane(&self) -> bool {
        self.w >= 300 && self.h >= 200
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sort {
    /// Header column: 0 Name, 1 Size, 2 % of parent, 3 Items, 4 Modified.
    pub column: usize,
    pub descending: bool,
}

/// The treemap pane: shown or hidden, and its wanted width in DIPs.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MapState {
    pub visible: bool,
    pub width: f32,
}

impl Default for MapState {
    fn default() -> Self {
        Self { visible: true, width: 420.0 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub window: Option<Placement>,
    /// Widths in DIPs of the Size, % of parent, Items and Modified columns; a shorter list (an older file) sets the first few.
    pub columns: Option<Vec<f32>>,
    pub sort: Option<Sort>,
    /// The treemap pane.
    pub map: MapState,
}

impl State {
    pub fn parse(text: &str) -> Self {
        toml::from_str(text).unwrap_or_default()
    }

    pub fn to_text(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }
}

fn path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("plumb").join("state.toml"))
}

pub fn load() -> State {
    path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| State::parse(&t)).unwrap_or_default()
}

/// Writes through a temporary file and a rename, so a crash never leaves half a file. Best effort.
pub fn save(state: &State) {
    let Some(p) = path() else { return };
    let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
    let tmp = p.with_extension("toml.tmp");
    if std::fs::write(&tmp, state.to_text()).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_survives_a_round_trip() {
        let s = State {
            window: Some(Placement { x: -20, y: 40, w: 1100, h: 760, maximized: true }),
            columns: Some(vec![120.0, 90.5, 80.0]),
            sort: Some(Sort { column: 3, descending: false }),
            map: MapState { visible: false, width: 333.5 },
        };
        assert_eq!(State::parse(&s.to_text()), s);
        assert_eq!(State::parse(&State::default().to_text()), State::default());
    }

    #[test]
    fn damaged_partial_and_newer_files_fall_back_to_what_they_can() {
        assert_eq!(State::parse("this is not toml {{"), State::default());
        assert_eq!(State::parse(""), State::default());
        let partial = State::parse("[sort]\ncolumn = 2\ndescending = true\nfuture_key = 1\n");
        assert_eq!(partial.sort, Some(Sort { column: 2, descending: true }));
        assert_eq!((partial.window, partial.columns), (None, None));
        assert_eq!(partial.map, MapState::default());
    }

    #[test]
    fn a_tiny_rectangle_is_not_sane() {
        assert!(Placement { x: 0, y: 0, w: 1100, h: 760, maximized: false }.is_sane());
        assert!(!Placement { x: 0, y: 0, w: 0, h: 0, maximized: false }.is_sane());
    }

    #[test]
    fn the_map_defaults_to_shown_and_a_damaged_value_falls_back() {
        assert_eq!(State::default().map, MapState { visible: true, width: 420.0 });
        let s = State::parse("[map]
visible = false
");
        assert_eq!(s.map, MapState { visible: false, width: 420.0 }, "a missing key means its default");
        assert_eq!(State::parse("[map]
width = \"wide\"
"), State::default(), "a damaged file means defaults");
    }
}
