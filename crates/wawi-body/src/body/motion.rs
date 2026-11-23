//! Her position and how it changes: Wawi 1's `Body` (`wawi/pet/world.py`).
//! A plain state machine with `dt` coming in, so the tests drive it with
//! made-up ledges and no screen at all.

use super::Params;
use super::world::{Ledge, Leg, landing, support};

/// What her body is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Standing still on a floor.
    Quieta,
    /// Standing still on a window's top edge.
    Posada,
    Caminando,
    Trotando,
    /// Up (or down) a window's side.
    Trepando,
    Cayendo,
    /// Floating down under the parachute.
    Paracaidas,
    /// Just touched down: the squash (or the porrazo) before she stands.
    Aterrizando,
    /// Held by the mouse.
    Agarrada,
}

impl Mode {
    /// On her way somewhere: turns into standing when she stops.
    pub fn going(self) -> bool {
        matches!(self, Mode::Caminando | Mode::Trotando | Mode::Trepando)
    }

    pub fn falling(self) -> bool {
        matches!(self, Mode::Cayendo | Mode::Paracaidas)
    }

    pub fn resting(self) -> bool {
        matches!(self, Mode::Quieta | Mode::Posada)
    }
}

/// How she touched down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Touch {
    /// The usual squash.
    Soft,
    /// A long, hard fall: the porrazo.
    Flop,
    /// Under the parachute: she lands and folds it.
    Chute,
}

/// What happened in one step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    /// Whatever held her is gone (her window moved or closed, or she walked
    /// off an edge): she falls.
    Fell,
    /// Her paws hit a surface after falling `height` points.
    Touchdown { touch: Touch, height: f64 },
    /// The landing is over: she stands.
    Landed,
    /// She got to the end of her path.
    Arrived,
}

/// Below this speed (points/s) a landing does not bounce.
pub const MIN_BOUNCE: f64 = 120.0;
/// Falling past this far below everything, she stops (nowhere to land).
const BOTTOMLESS: f64 = -10_000.0;

#[derive(Clone, Debug)]
pub struct Body {
    pub x: f64,
    pub y: f64,
    /// Vertical speed, points/s, up is positive.
    pub vy: f64,
    pub mode: Mode,
    pub facing_right: bool,
    pub legs: Vec<Leg>,
    /// The ledge under her (`None` in the air).
    pub ledge: Option<Ledge>,
    /// A voice turn or an action: she stands where she is (a fall still
    /// finishes).
    pub paused: bool,
    /// The next fall is a parachute float.
    pub chute: bool,
    /// Where the current fall started.
    pub fall_top: f64,
    land_left: f64,
}

impl Body {
    pub fn new(x: f64, y: f64) -> Self {
        Self {
            x,
            y,
            vy: 0.0,
            mode: Mode::Quieta,
            facing_right: false,
            legs: Vec::new(),
            ledge: None,
            paused: false,
            chute: false,
            fall_top: y,
            land_left: 0.0,
        }
    }

    /// On her way, falling or landing: nothing else starts.
    pub fn busy(&self) -> bool {
        !self.legs.is_empty() || !self.mode.resting()
    }

    fn rest_mode(&self) -> Mode {
        match self.ledge {
            Some(l) if l.window.is_some() => Mode::Posada,
            _ => Mode::Quieta,
        }
    }

    pub fn go(&mut self, legs: Vec<Leg>) {
        self.legs = legs;
    }

    /// Picked up by the mouse.
    pub fn grab(&mut self) {
        self.legs.clear();
        self.vy = 0.0;
        self.chute = false;
        self.ledge = None;
        self.mode = Mode::Agarrada;
    }

    /// Let go: she falls if nothing is under her (Wawi 1 dropped her
    /// straight down, from rest).
    pub fn release(&mut self) {
        if self.mode == Mode::Agarrada {
            self.mode = Mode::Quieta;
            self.vy = 0.0;
        }
    }

    fn start_fall(&mut self) {
        self.mode = if self.chute {
            Mode::Paracaidas
        } else {
            Mode::Cayendo
        };
        self.vy = 0.0;
        self.ledge = None;
        self.legs.clear();
        self.fall_top = self.y;
    }

    /// Straight onto whatever is below, no fall (reduce motion, full screen,
    /// the first placement).
    pub fn snap_down(&mut self, ledges: &[Ledge]) {
        if let Some(led) = landing(ledges, self.x, self.y) {
            self.x = led.clamp(self.x);
            self.y = led.y;
            self.vy = 0.0;
            self.ledge = Some(led);
            self.legs.clear();
            self.chute = false;
            self.mode = self.rest_mode();
        }
    }

    /// Advances `dt` seconds (one fixed step).
    pub fn update(&mut self, dt: f64, ledges: &[Ledge], p: &Params) -> Option<Event> {
        match self.mode {
            Mode::Agarrada => return None,
            Mode::Aterrizando => {
                self.land_left -= dt;
                if self.land_left <= 0.0 {
                    self.mode = self.rest_mode();
                    return Some(Event::Landed);
                }
                return None;
            }
            Mode::Cayendo | Mode::Paracaidas => return self.fall(dt, ledges, p),
            _ => {}
        }
        let leg = if self.paused {
            None
        } else {
            self.legs.first().copied()
        };
        let airborne = matches!(leg, Some(Leg::Climb { .. } | Leg::Shift { .. }));
        if !airborne {
            match support(ledges, self.x, self.y) {
                None => {
                    self.start_fall();
                    return Some(Event::Fell);
                }
                Some(led) => {
                    self.ledge = Some(led);
                    // The window under her may have shifted sideways.
                    self.y = led.y;
                }
            }
        }
        let Some(leg) = leg else {
            if self.mode.going() || self.mode.resting() {
                self.mode = self.rest_mode();
            }
            return None;
        };
        let mut done = false;
        match leg {
            Leg::Walk { to, speed } | Leg::WalkOff { to, speed } => {
                let target = match (leg, self.ledge) {
                    (Leg::Walk { .. }, Some(l)) => l.clamp(to),
                    _ => to,
                };
                done = self.step_x(target, speed, dt);
                self.mode = if speed > p.walk_speed * 1.5 {
                    Mode::Trotando
                } else {
                    Mode::Caminando
                };
            }
            Leg::Climb { to } => {
                self.mode = Mode::Trepando;
                self.ledge = None;
                let d = to - self.y;
                let step = p.climb_speed * dt;
                if d.abs() <= step {
                    self.y = to;
                    done = true;
                } else {
                    self.y += step.copysign(d);
                }
            }
            Leg::Shift { to } => {
                self.ledge = None;
                self.mode = Mode::Caminando;
                done = self.step_x(to, p.walk_speed, dt);
            }
        }
        if done {
            self.legs.remove(0);
            if self.legs.is_empty() {
                if self.mode.going() {
                    self.ledge = support(ledges, self.x, self.y).or(self.ledge);
                    self.mode = self.rest_mode();
                }
                return Some(Event::Arrived);
            }
        }
        None
    }

    /// Towards `target` at `speed`; true once there.
    fn step_x(&mut self, target: f64, speed: f64, dt: f64) -> bool {
        let d = target - self.x;
        if d.abs() > 0.01 {
            self.facing_right = d > 0.0;
        }
        if d.abs() <= speed * dt {
            self.x = target;
            true
        } else {
            self.x += (speed * dt).copysign(d);
            false
        }
    }

    fn fall(&mut self, dt: f64, ledges: &[Ledge], p: &Params) -> Option<Event> {
        let below = landing(ledges, self.x, self.y);
        let limit = if self.mode == Mode::Paracaidas {
            p.chute_speed
        } else {
            p.max_fall
        };
        self.vy = (self.vy - p.gravity * dt).max(-limit);
        let ny = self.y + self.vy * dt;
        if let Some(led) = below
            && ny <= led.y
        {
            self.x = led.clamp(self.x);
            self.y = led.y;
            let speed = -self.vy;
            if self.mode == Mode::Cayendo && p.bounce > 0.0 && speed * p.bounce >= MIN_BOUNCE {
                // A bounce: up again, a little less each time.
                self.vy = speed * p.bounce;
                return None;
            }
            let height = (self.fall_top - led.y).max(0.0);
            let touch = if self.mode == Mode::Paracaidas {
                Touch::Chute
            } else if p.flop_from > 0.0 && height >= p.flop_from {
                Touch::Flop
            } else {
                Touch::Soft
            };
            self.vy = 0.0;
            self.ledge = Some(led);
            self.chute = false;
            self.mode = Mode::Aterrizando;
            self.land_left = match touch {
                Touch::Soft => p.land_secs,
                Touch::Flop => p.flop_secs,
                Touch::Chute => p.chute_land_secs,
            };
            return Some(Event::Touchdown { touch, height });
        }
        if below.is_none() && ny < BOTTOMLESS {
            self.vy = 0.0;
            self.mode = Mode::Quieta;
            return Some(Event::Landed);
        }
        self.y = ny;
        None
    }
}
