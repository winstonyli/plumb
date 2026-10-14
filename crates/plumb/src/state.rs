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
    /// Header column: 0 Name, 1 Size, 2 % of parent, 3 Items.
    pub column: usize,
    pub descending: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub window: Option<Placement>,
    /// Widths in DIPs of the Size, % of parent and Items columns.
    pub columns: Option<[f32; 3]>,
    pub sort: Option<Sort>,
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
            columns: Some([120.0, 90.5, 80.0]),
            sort: Some(Sort { column: 3, descending: false }),
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
    }

    #[test]
    fn a_tiny_rectangle_is_not_sane() {
        assert!(Placement { x: 0, y: 0, w: 1100, h: 760, maximized: false }.is_sane());
        assert!(!Placement { x: 0, y: 0, w: 0, h: 0, maximized: false }.is_sane());
    }
}
