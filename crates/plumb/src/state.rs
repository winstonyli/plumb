//! What plumb remembers between runs: window placement, and each view kind's column widths and sort, as TOML in
//! `%LOCALAPPDATA%\plumb\state.toml`. A missing or unreadable file means defaults; unknown keys are ignored.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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

/// A saved sort: the column by its id (`ColId::name`), so a reordered table cannot change what it means.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SortPref {
    pub col: String,
    pub descending: bool,
}

/// What one view kind remembers. Ids are strings, so an id this build does not know is skipped, not a damaged file.
/// `widths` is declared first so TOML writes the `sort` sub-table last.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewPrefs {
    /// Widths in DIPs of the Score through last columns, by id.
    pub widths: BTreeMap<String, f32>,
    pub sort: Option<SortPref>,
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
    pub disk: ViewPrefs,
    pub processes: ViewPrefs,
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

    fn prefs(col: &str, descending: bool, widths: &[(&str, f32)]) -> ViewPrefs {
        ViewPrefs { widths: widths.iter().map(|(k, w)| (k.to_string(), *w)).collect(), sort: Some(SortPref { col: col.into(), descending }) }
    }

    #[test]
    fn a_state_round_trips_per_kind() {
        let s = State {
            window: Some(Placement { x: -20, y: 40, w: 1100, h: 760, maximized: true }),
            disk: prefs("items", false, &[("score", 120.0), ("size", 90.5)]),
            processes: prefs("cpu", true, &[("io", 80.0)]),
            map: MapState { visible: false, width: 333.5 },
        };
        assert_eq!(State::parse(&s.to_text()), s);
        assert_eq!(State::parse(&State::default().to_text()), State::default());
    }

    #[test]
    fn a_0_13_state_file_gives_defaults() {
        let s = State::parse("columns = [1.0]
[sort]
column = 4
descending = true
[map]
visible = false
");
        assert_eq!((s.disk, s.processes), Default::default(), "the old sort and widths are dropped");
        assert!(!s.map.visible, "the rest of the file still loads");
    }

    #[test]
    fn damaged_partial_and_newer_files_fall_back_to_what_they_can() {
        assert_eq!(State::parse("this is not toml {{"), State::default());
        assert_eq!(State::parse(""), State::default());
        let partial = State::parse("[disk.sort]
col = \"pct\"
descending = true
future_key = 1
");
        assert_eq!(partial.disk.sort, Some(SortPref { col: "pct".into(), descending: true }));
        assert_eq!((partial.window, partial.processes), (None, Default::default()));
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
