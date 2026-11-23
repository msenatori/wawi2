//! The desktop as surfaces she can stand on, ported from Wawi 1
//! (`wawi/pet/world.py`). Pure: screens and windows come in, ledges and
//! paths go out. Everything is in AppKit global coordinates (points, origin
//! at the bottom-left of the primary screen, y up). Her position is her
//! **ground point**: where her paws touch whatever she stands on.

use crate::place::Rect;

/// Window owners that are never furniture: system overlays and chrome.
pub const IGNORED_OWNERS: &[&str] = &[
    "Dock",
    "Window Server",
    "Control Center",
    "Notification Center",
    "Spotlight",
    "SystemUIServer",
    "screencaptureui",
    "Screenshot",
    "WindowManager",
    "Wallpaper",
    "loginwindow",
    "TextInputMenuAgent",
    "CursorUIViewService",
];
/// Windows smaller than this (points) are popovers, tooltips, HUDs.
pub const MIN_WINDOW: (f64, f64) = (160.0, 90.0);
/// Below this opacity a window is an overlay, not something to sit on.
pub const MIN_ALPHA: f64 = 0.3;
/// A standard title bar: the part of a window that is not its content.
pub const TITLE_BAR: f64 = 28.0;
/// She keeps this fraction of her width clear of a screen's sides.
pub const EDGE_MARGIN: f64 = 0.4;
/// How close (points) her paws must be to a surface to stand on it.
pub const TOLERANCE: f64 = 2.5;

/// A screen: all of it, and the part not under the menu bar or the Dock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screen {
    pub frame: Rect,
    pub visible: Rect,
}

/// An ordinary window of another app. Lists are front to back.
#[derive(Clone, Debug, PartialEq)]
pub struct Win {
    pub id: u32,
    pub rect: Rect,
}

impl Win {
    pub fn top(&self) -> f64 {
        self.rect.1 + self.rect.3
    }

    /// The window minus its title bar: where the text is.
    pub fn content(&self) -> Rect {
        let (x, y, w, h) = self.rect;
        (x, y, w, (h - TITLE_BAR).max(0.0))
    }
}

/// One entry of the system's window list, as macOS gives it (Quartz
/// coordinates: origin top-left of the primary screen, y down).
#[derive(Clone, Debug, PartialEq)]
pub struct RawWindow {
    pub id: u32,
    pub layer: i64,
    pub pid: i64,
    pub owner: String,
    pub alpha: f64,
    pub bounds: Rect,
}

/// The screens and windows she lives among. The backend keeps one up to
/// date; the tests make them up.
pub trait World {
    fn screens(&self) -> &[Screen];
    /// Front to back.
    fn windows(&self) -> &[Win];
    /// Changes whenever the screens or the windows change.
    fn version(&self) -> u64;
}

/// A world that is just data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub version: u64,
    pub screens: Vec<Screen>,
    pub windows: Vec<Win>,
}

impl World for Snapshot {
    fn screens(&self) -> &[Screen] {
        &self.screens
    }
    fn windows(&self) -> &[Win] {
        &self.windows
    }
    fn version(&self) -> u64 {
        self.version
    }
}

/// Somewhere to stand: her centre may be anywhere in `[x0, x1]` at height
/// `y`. `window` is the window it is the top of, `None` for a floor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ledge {
    pub x0: f64,
    pub x1: f64,
    pub y: f64,
    pub window: Option<u32>,
    pub screen: usize,
}

impl Ledge {
    pub fn holds(&self, x: f64, y: f64) -> bool {
        self.x0 - 0.5 <= x && x <= self.x1 + 0.5 && (y - self.y).abs() <= TOLERANCE
    }

    pub fn clamp(&self, x: f64) -> f64 {
        x.max(self.x0).min(self.x1)
    }
}

pub fn intersects(a: Rect, b: Rect) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

fn contains(outer: Rect, inner: Rect) -> bool {
    let slack = 0.5;
    inner.0 >= outer.0 - slack
        && inner.1 >= outer.1 - slack
        && inner.0 + inner.2 <= outer.0 + outer.2 + slack
        && inner.1 + inner.3 <= outer.1 + outer.3 + slack
}

/// The box she covers standing at ground point `(x, y)`.
pub fn her_rect(x: f64, y: f64, size: (f64, f64)) -> Rect {
    (x - size.0 / 2.0, y, size.0, size.1)
}

/// The system's window list, turned into furniture: keeps the front-to-back
/// order, drops our own windows, anything off the normal layer (menu bar,
/// Dock, overlays), system owners, see-through and tiny windows. Bounds,
/// owner and layer need no Screen Recording permission (titles would; they
/// are never read).
pub fn parse_windows(raw: &[RawWindow], own_pid: i64, primary_height: f64) -> Vec<Win> {
    raw.iter()
        .filter(|d| d.layer == 0 && d.pid != own_pid)
        .filter(|d| !IGNORED_OWNERS.contains(&d.owner.as_str()))
        .filter(|d| d.alpha >= MIN_ALPHA)
        .filter(|d| d.bounds.2 >= MIN_WINDOW.0 && d.bounds.3 >= MIN_WINDOW.1)
        .map(|d| {
            let (x, y, w, h) = d.bounds;
            Win {
                id: d.id,
                rect: (x, primary_height - y - h, w, h),
            }
        })
        .collect()
}

/// The front window fills a whole screen (a full-screen app, a video, a
/// presentation): she keeps still and out of the way.
pub fn fullscreen_active(windows: &[Win], screens: &[Screen]) -> bool {
    let Some(front) = windows.first() else {
        return false;
    };
    screens.iter().any(|s| {
        let (fx, fy, fw, fh) = s.frame;
        let (x, y, w, h) = front.rect;
        (x - fx).abs() <= 2.0
            && (y - fy).abs() <= 2.0
            && (w - fw).abs() <= 2.0
            && (h - fh).abs() <= 2.0
    })
}

fn subtract(segs: Vec<(f64, f64)>, a: f64, b: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for (s0, s1) in segs {
        if b <= s0 || a >= s1 {
            out.push((s0, s1));
            continue;
        }
        if a > s0 {
            out.push((s0, a));
        }
        if b < s1 {
            out.push((b, s1));
        }
    }
    out
}

/// Every surface she can stand on, floors first. A floor is the bottom of a
/// screen's visible frame (above the Dock). A window's top edge counts only
/// where no window in front of it covers the edge, only inside a screen, and
/// only with room for her under the menu bar.
pub fn ledges(screens: &[Screen], windows: &[Win], size: (f64, f64)) -> Vec<Ledge> {
    let (w, h) = size;
    let side = w / 2.0 + EDGE_MARGIN * w;
    let mut out = Vec::new();
    for (i, s) in screens.iter().enumerate() {
        let (vx, vy, vw, _) = s.visible;
        if vw >= 2.0 * side {
            out.push(Ledge {
                x0: vx + side,
                x1: vx + vw - side,
                y: vy,
                window: None,
                screen: i,
            });
        }
    }
    for (idx, win) in windows.iter().enumerate() {
        let (x, _, ww, _) = win.rect;
        let top = win.top();
        let mut segs = vec![(x, x + ww)];
        for front in &windows[..idx] {
            let (fx, fy, fw, fh) = front.rect;
            // Covers the edge, or would cover her sitting on it.
            if fy - 2.0 <= top && top <= fy + fh + h * 0.5 {
                segs = subtract(segs, fx, fx + fw);
            }
            if segs.is_empty() {
                break;
            }
        }
        for (i, s) in screens.iter().enumerate() {
            let (vx, vy, vw, vh) = s.visible;
            if !(vy + h * 0.5 < top && top + h <= vy + vh) {
                continue;
            }
            for &(s0, s1) in &segs {
                let (a, b) = (s0.max(vx), s1.min(vx + vw));
                let lo = (a + w / 2.0).max(vx + side);
                let hi = (b - w / 2.0).min(vx + vw - side);
                if hi - lo >= 4.0 {
                    out.push(Ledge {
                        x0: lo,
                        x1: hi,
                        y: top,
                        window: Some(win.id),
                        screen: i,
                    });
                }
            }
        }
    }
    out
}

/// The ledge she is standing on at `(x, y)`, or `None`: she is in the air.
pub fn support(ledges: &[Ledge], x: f64, y: f64) -> Option<Ledge> {
    ledges
        .iter()
        .filter(|l| l.holds(x, y))
        .max_by(|a, b| a.y.total_cmp(&b.y))
        .copied()
}

/// Where she lands falling straight down from `(x, y)`: the highest ledge
/// at or below her, under her centre; failing that (between screens), the
/// nearest floor.
pub fn landing(ledges: &[Ledge], x: f64, y: f64) -> Option<Ledge> {
    let below = ledges
        .iter()
        .filter(|l| l.x0 - 0.5 <= x && x <= l.x1 + 0.5 && l.y <= y + TOLERANCE)
        .max_by(|a, b| a.y.total_cmp(&b.y));
    if let Some(l) = below {
        return Some(*l);
    }
    ledges
        .iter()
        .filter(|l| l.window.is_none())
        .min_by(|a, b| {
            let d = |l: &Ledge| (l.clamp(x) - x).abs() + (l.y - y).abs() * 0.01;
            d(a).total_cmp(&d(b))
        })
        .copied()
}

/// Is `rect` entirely inside one screen's visible frame?
pub fn on_screen(screens: &[Screen], rect: Rect) -> bool {
    screens.iter().any(|s| contains(s.visible, rect))
}

/// The nearest ground point that keeps all of her on a visible frame (under
/// the menu bar, above the Dock).
pub fn clamp_to_screens(screens: &[Screen], x: f64, y: f64, size: (f64, f64)) -> (f64, f64) {
    let (w, h) = size;
    let mut best = (x, y);
    let mut best_d = f64::INFINITY;
    for s in screens {
        let (vx, vy, vw, vh) = s.visible;
        let side = w / 2.0 + EDGE_MARGIN * w;
        let cx = x.max(vx + side).min(vx + side.max(vw - side));
        let cy = y.max(vy).min(vy + (vh - h).max(0.0));
        let d = (cx - x).hypot(cy - y);
        if d < best_d {
            best = (cx, cy);
            best_d = d;
        }
    }
    best
}

/// What she must never cover: the front window's text, and the cursor (with
/// a margin).
pub fn avoid_rects(front: Option<&Win>, cursor: Option<(f64, f64)>) -> Vec<Rect> {
    let mut out = Vec::new();
    if let Some(front) = front
        && front.content().3 > 0.0
    {
        out.push(front.content());
    }
    if let Some((cx, cy)) = cursor {
        let pad = 40.0;
        out.push((cx - pad, cy - pad, 2.0 * pad, 2.0 * pad));
    }
    out
}

/// One piece of a path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Leg {
    /// Along her ledge to x (clamped to it).
    Walk { to: f64, speed: f64 },
    /// Past the end of her ledge to x: gravity takes it from there.
    WalkOff { to: f64, speed: f64 },
    /// Up (or down) a window's side to y, in the air.
    Climb { to: f64 },
    /// Across, in the air, onto a ledge at x (off the top of a climb, or
    /// over the gap between two monitors).
    Shift { to: f64 },
}

/// Legs from where she is to `target` (a ground point), Wawi 1's
/// `plan_path`:
///
/// - same height: walk;
/// - higher: walk to beside the window, climb its side, step onto it and
///   walk to the spot;
/// - another monitor's floor: hop the gap;
/// - lower: walk off the end of her ledge on the target's side; she falls,
///   and the trip goes on from where she lands.
pub fn plan_path(
    here: Option<Ledge>,
    at: (f64, f64),
    target: (f64, f64),
    target_ledge: Option<Ledge>,
    size: (f64, f64),
    walk: f64,
) -> Vec<Leg> {
    let (x, y) = at;
    let (tx, ty) = target;
    let w = size.0;
    let mut legs = Vec::new();
    if (ty - y).abs() <= TOLERANCE && here.is_none_or(|h| h.x0 - 0.5 <= tx && tx <= h.x1 + 0.5) {
        legs.push(Leg::Walk {
            to: tx,
            speed: walk,
        });
    } else if ty > y + TOLERANCE {
        let mut cx = tx;
        let side = target_ledge.filter(|l| l.window.is_some());
        if let Some(l) = side {
            let left = l.x0 - w * 0.7;
            let right = l.x1 + w * 0.7;
            cx = if (left - x).abs() <= (right - x).abs() {
                left
            } else {
                right
            };
        }
        if let Some(h) = here {
            cx = h.clamp(cx);
        }
        legs.push(Leg::Walk {
            to: cx,
            speed: walk,
        });
        legs.push(Leg::Climb { to: ty });
        match side {
            Some(l) => {
                legs.push(Leg::Shift { to: l.clamp(cx) });
                legs.push(Leg::Walk {
                    to: tx,
                    speed: walk,
                });
            }
            None if (cx - tx).abs() > 1.0 => legs.push(Leg::Shift { to: tx }),
            None => {}
        }
    } else if let (Some(h), Some(t)) = (here, target_ledge)
        && h.window.is_none()
        && t.window.is_none()
        && t.screen != h.screen
    {
        // Another monitor's floor: hop the gap between the two floors.
        let toward = tx > x;
        legs.push(Leg::Walk {
            to: if toward { h.x1 } else { h.x0 },
            speed: walk,
        });
        legs.push(Leg::Shift {
            to: if toward { t.x0 } else { t.x1 },
        });
        legs.push(Leg::Walk {
            to: tx,
            speed: walk,
        });
    } else if let Some(h) = here {
        let edge = if tx < x {
            h.x0 - w * 0.9
        } else {
            h.x1 + w * 0.9
        };
        legs.push(Leg::Walk {
            to: h.clamp(edge),
            speed: walk,
        });
        legs.push(Leg::WalkOff {
            to: edge,
            speed: walk,
        });
    } else {
        legs.push(Leg::Walk {
            to: tx,
            speed: walk,
        });
    }
    legs
}
