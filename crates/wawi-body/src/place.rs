//! Where she stands, whether she is shown, which pet pack is on screen and
//! whether her body moves (`"movimiento"`), kept in `~/.wawi2/mascot.json`
//! (Wawi 1's `alone_origin`: a saved spot is kept on the screen that holds
//! it, and she comes back to the corner if that screen is gone).

use std::fs;
use std::io;
use std::path::Path;

use wawi_core::json::{self, Json};

/// `(x, y, width, height)`, AppKit style: origin bottom-left, y up.
pub type Rect = (f64, f64, f64, f64);

/// Gap between her and the edges of the screen when she is in her corner.
pub const MARGIN: f64 = 24.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    /// Her window's origin, or `None` for "never moved": the corner.
    pub origin: Option<(f64, f64)>,
    pub visible: bool,
    /// The pet pack's id (`"mascota"`); `None` = the built-in Wawi.
    pub pet: Option<String>,
    /// Gravity and wandering (`"movimiento"`; «Quedate quieta» turns it
    /// off). Only for pets with a body.
    pub movement: bool,
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            origin: None,
            visible: true,
            pet: None,
            movement: true,
        }
    }
}

impl Placement {
    /// A missing or garbled file is the default: she never fails to show up
    /// over a settings file.
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    pub fn parse(text: &str) -> Self {
        let Ok(value) = json::parse(text) else {
            return Self::default();
        };
        let number = |key: &str| match value.get(key) {
            Some(Json::Number(n)) if n.is_finite() => Some(*n),
            _ => None,
        };
        let origin = number("x").zip(number("y"));
        let visible = !matches!(value.get("visible"), Some(Json::Bool(false)));
        let pet = match value.get("mascota") {
            Some(Json::String(id)) if !id.trim().is_empty() => Some(id.trim().to_owned()),
            _ => None,
        };
        let movement = !matches!(value.get("movimiento"), Some(Json::Bool(false)));
        Self {
            origin,
            visible,
            pet,
            movement,
        }
    }

    pub fn to_json(&self) -> String {
        let mut members = Vec::new();
        if let Some((x, y)) = self.origin {
            members.push(format!("\"x\": {x:.1}"));
            members.push(format!("\"y\": {y:.1}"));
        }
        members.push(format!("\"visible\": {}", self.visible));
        if let Some(pet) = &self.pet {
            let escaped: String = pet
                .chars()
                .filter(|c| !c.is_control())
                .flat_map(|c| match c {
                    '"' | '\\' => vec!['\\', c],
                    c => vec![c],
                })
                .collect();
            members.push(format!("\"mascota\": \"{escaped}\""));
        }
        if !self.movement {
            members.push("\"movimiento\": false".into());
        }
        format!("{{{}}}\n", members.join(", "))
    }

    /// Written whole to a temporary file and renamed, so a crash mid-write
    /// never leaves half a file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, self.to_json())?;
        fs::rename(tmp, path)
    }
}

/// Her corner with no saved spot: bottom-right of the visible frame.
pub fn default_origin(size: (f64, f64), screen: Rect) -> (f64, f64) {
    let (sx, sy, sw, _) = screen;
    (sx + sw - size.0 - MARGIN, sy + MARGIN)
}

/// Where to put her: the saved spot nudged fully inside the screen that holds
/// its centre, or the corner of `main` if no screen does. `None` without
/// screens.
pub fn on_screen(
    saved: Option<(f64, f64)>,
    size: (f64, f64),
    screens: &[Rect],
    main: Option<Rect>,
) -> Option<(f64, f64)> {
    let home = main.or_else(|| screens.first().copied())?;
    let Some((x, y)) = saved else {
        return Some(default_origin(size, home));
    };
    let (w, h) = size;
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    for &(sx, sy, sw, sh) in screens {
        if sx <= cx && cx <= sx + sw && sy <= cy && cy <= sy + sh {
            let x = x.max(sx).min(sx + sw - w);
            let y = y.max(sy).min(sy + sh - h);
            return Some((x, y));
        }
    }
    Some(default_origin(size, home))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = (0.0, 0.0, 1440.0, 875.0);
    const SIZE: (f64, f64) = (156.0, 143.0);

    #[test]
    fn it_survives_a_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("wawi-body-place-{}", std::process::id()));
        let path = dir.join("mascot.json");
        assert_eq!(Placement::load(&path), Placement::default());
        let placement = Placement {
            origin: Some((1200.5, 40.0)),
            visible: false,
            pet: Some("kitty".into()),
            movement: false,
        };
        placement.save(&path).unwrap();
        assert_eq!(Placement::load(&path), placement);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bad_file_is_the_default() {
        assert_eq!(Placement::parse("{not json"), Placement::default());
        assert_eq!(
            Placement::parse("{\"x\": \"a\", \"y\": 3}"),
            Placement::default()
        );
        assert_eq!(
            Placement::parse("{\"visible\": false}"),
            Placement {
                origin: None,
                visible: false,
                pet: None,
                movement: true,
            }
        );
        assert!(!Placement::parse("{\"movimiento\": false}").movement);
        assert!(Placement::parse("{\"movimiento\": 3}").movement);
        assert_eq!(
            Placement::parse("{\"mascota\": \"kitty\"}").pet.as_deref(),
            Some("kitty")
        );
    }

    #[test]
    fn never_moved_means_the_bottom_right_corner() {
        assert_eq!(
            on_screen(None, SIZE, &[SCREEN], None),
            Some((1440.0 - 156.0 - MARGIN, MARGIN))
        );
        assert_eq!(on_screen(None, SIZE, &[], None), None);
    }

    #[test]
    fn a_spot_half_off_the_edge_is_pulled_in() {
        assert_eq!(
            on_screen(Some((1300.0, 800.0)), SIZE, &[SCREEN], None),
            Some((1440.0 - 156.0, 875.0 - 143.0))
        );
        assert_eq!(
            on_screen(Some((300.0, 200.0)), SIZE, &[SCREEN], None),
            Some((300.0, 200.0))
        );
    }

    #[test]
    fn an_unplugged_monitor_sends_her_home() {
        let second = (1440.0, 0.0, 1920.0, 1080.0);
        let spot = Some((2000.0, 300.0));
        assert_eq!(
            on_screen(spot, SIZE, &[SCREEN, second], Some(SCREEN)),
            Some((2000.0, 300.0))
        );
        assert_eq!(
            on_screen(spot, SIZE, &[SCREEN], Some(SCREEN)),
            Some(default_origin(SIZE, SCREEN))
        );
    }
}
