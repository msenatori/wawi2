//! Her body on the desktop: gravity, walking, climbing, the parachute,
//! sitting on windows. A port of Wawi 1's (`wawi/pet/world.py` and
//! `wawi/pet/life.py`), driven by the pet pack's `[cuerpo]`.
//!
//! Pure and cheap: no AppKit, no threads, no clock of its own. The backend
//! calls [`Walker::tick`] at the frame rate with the time (the injectable
//! clock: the tests pass made-up seconds) and a [`World`] (screens and
//! windows; the tests make one up), and draws what [`Out`] says. Physics
//! runs on a fixed step, whatever the frame rate.
//!
//! The rules that keep her calm (and match the voice):
//!
//! - During a voice turn or an action she stands still; a fall still
//!   finishes (Wawi 1).
//! - She wanders rarely: every `cuanto_camina` minutes, or to get off the
//!   text of the window in front (at most every 10 minutes). Never while a
//!   full-screen app is in front.
//! - Walking is the usual. Climbing, trotting and the parachute never come
//!   twice in a row, and each has its own wait (`cada_trepada`, …): no
//!   climbing loops.

mod motion;
pub mod world;

#[cfg(test)]
mod tests;

pub use motion::{Body, Event, MIN_BOUNCE, Mode, Touch};
pub use world::{Ledge, Leg, RawWindow, Screen, Snapshot, Win, World};

use world::{
    avoid_rects, fullscreen_active, her_rect, intersects, landing, ledges, on_screen, plan_path,
    support,
};

/// The physics step, in seconds.
pub const STEP: f64 = 1.0 / 120.0;
/// Longest stretch of time one tick catches up on (a stall, a sleep).
const MAX_DT: f64 = 0.25;
/// How often she checks whether she is sitting on the front window's text.
const COVER_CHECK: f64 = 20.0;
/// She moves off the front window's text at most this often (seconds).
const COVER_EVERY: f64 = 600.0;
/// A wander goes at least this far and at most that far (points).
const MIN_TRIP: f64 = 60.0;
const MAX_TRIP: f64 = 900.0;
/// A trot only for trips longer than this (points).
const TROT_FROM: f64 = 200.0;
/// Window tops weigh this against a floor's 1, and spots on her own level
/// this much more (Wawi 1's calm dog, `perra_tranquila`).
const TOP_WEIGHT: f64 = 0.5;
const SAME_LEVEL_WEIGHT: f64 = 1.5;
/// A trip whose landing keeps moving re-plans at most this many times.
const MAX_REPLANS: u8 = 3;

/// The numbers of her body. The defaults are Wawi 1's, the feel the owner
/// misses; a pack changes them in `[cuerpo]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// Points/s² (`gravedad`).
    pub gravity: f64,
    /// Terminal speed of a fall, points/s (`velocidad_maxima`).
    pub max_fall: f64,
    /// Points/s (`velocidad_caminar`, `velocidad_trote`,
    /// `velocidad_trepar`, `velocidad_paracaidas`).
    pub walk_speed: f64,
    pub trot_speed: f64,
    pub climb_speed: f64,
    pub chute_speed: f64,
    /// 0 = no bounce; 0.3 = back up with 30% of the speed (`rebote`).
    pub bounce: f64,
    /// Seconds of the landing squash (`aterrizaje`).
    pub land_secs: f64,
    /// A fall at least this high (points) ends in the porrazo; 0 = never
    /// (`porrazo_desde`).
    pub flop_from: f64,
    /// How long the porrazo and the parachute landing last (their clips).
    pub flop_secs: f64,
    pub chute_land_secs: f64,
    /// Chance (0..1) that a trip down off a window goes by parachute
    /// (`chance_paracaidas`).
    pub chute_chance: f64,
    /// The parachute only with at least this drop (points).
    pub chute_min_drop: f64,
    /// Chance (0..1) that a long trip on her own level is a trot
    /// (`chance_trote`).
    pub trot_chance: f64,
    /// Seconds between wanders, drawn in this range (`cuanto_camina`, in
    /// minutes in the pack).
    pub wander_every: (f64, f64),
    /// She walks at all (the pack has a walk). Without it she only falls.
    pub walks: bool,
    /// She climbs windows (the pack has a climb).
    pub climbs: bool,
    /// The parachute (the pack has one).
    pub chutes: bool,
    /// Seconds before each special trip may come again (`cada_trepada`,
    /// `cada_paracaidas`, `cada_trote`).
    pub climb_every: f64,
    pub chute_every: f64,
    pub trot_every: f64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            gravity: 1500.0,
            max_fall: 950.0,
            walk_speed: 42.0,
            trot_speed: 130.0,
            climb_speed: 30.0,
            chute_speed: 75.0,
            bounce: 0.0,
            land_secs: 0.35,
            flop_from: 0.0,
            flop_secs: 2.3,
            chute_land_secs: 2.3,
            chute_chance: 0.0,
            chute_min_drop: 120.0,
            trot_chance: 0.0,
            wander_every: (8.0 * 60.0, 25.0 * 60.0),
            walks: true,
            climbs: true,
            chutes: true,
            climb_every: 30.0 * 60.0,
            chute_every: 10.0 * 60.0,
            trot_every: 5.0 * 60.0,
        }
    }
}

/// What the body shows: the backend turns it into the pack's clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Loco {
    Walk,
    Trot,
    Climb,
    Fall,
    Chute,
    Held,
    Land,
    Flop,
    ChuteLand,
}

/// A kind of trip, for the anti-repetition rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Trip {
    /// Along her level: the usual.
    Walk,
    /// Off the end of her ledge, down to a lower one.
    Down,
    Trot,
    Climb,
    Chute,
}

impl Trip {
    /// Special trips never come twice in a row and have their own wait.
    pub fn special(self) -> bool {
        matches!(self, Trip::Trot | Trip::Climb | Trip::Chute)
    }
}

/// What the rest of her is doing, for the body to keep in step.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ctx {
    /// A voice turn, or an action playing: stand still (a fall finishes).
    pub calm: bool,
    /// Awake, idle and doing nothing: she may get up and go.
    pub free: bool,
    pub asleep: bool,
    /// Reduce Motion: no falls across the screen (she just lands) and no
    /// trips.
    pub still: bool,
    /// The mouse, to stay out of its way and wander near the owner.
    pub cursor: Option<(f64, f64)>,
}

/// What to draw after a tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Out {
    /// Her ground point.
    pub ground: (f64, f64),
    pub mode: Mode,
    pub loco: Option<Loco>,
    pub facing_right: bool,
    /// Resting on a window's top edge.
    pub perched: bool,
    /// Asleep and it is time to go: wake her (her `despertarse`), the trip
    /// starts once she is up.
    pub wake: bool,
    /// She just came to rest somewhere new: a good moment to save the spot.
    pub settled: bool,
    /// Touched down this tick, and how.
    pub touchdown: Option<Touch>,
}

/// A tiny xorshift, seedable for tests.
#[derive(Clone, Debug)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 11) as f64 / (1u64 << 53) as f64
    }

    fn between(&mut self, (lo, hi): (f64, f64)) -> f64 {
        lo + (hi - lo) * self.next()
    }
}

#[derive(Clone, Copy, Debug)]
struct Goal {
    target: (f64, f64),
    ledge: Option<Ledge>,
    replans: u8,
}

/// Her body and the decisions about where it goes.
#[derive(Clone, Debug)]
pub struct Walker {
    pub params: Params,
    size: (f64, f64),
    body: Body,
    ledges: Vec<Ledge>,
    version: Option<u64>,
    rng: Rng,
    last: Option<f64>,
    acc: f64,
    settled: bool,
    next_wander: Option<f64>,
    next_cover: f64,
    last_cover_move: f64,
    goal: Option<Goal>,
    last_trip: Option<Trip>,
    /// When each special trip may come again.
    waits: Vec<(Trip, f64)>,
    /// How the landing in progress began.
    last_touch: Option<Touch>,
    /// The next wander was asked for (a preview): it may cover the front
    /// window and the cursor.
    asked: bool,
    /// Trips started, oldest first (for the tests and the log).
    pub trips: Vec<Trip>,
}

impl Walker {
    /// Her body, `size` points (the sprite), standing at ground point `at`.
    pub fn new(params: Params, size: (f64, f64), at: (f64, f64), seed: u64) -> Self {
        Self {
            params,
            size,
            body: Body::new(at.0, at.1),
            ledges: Vec::new(),
            version: None,
            rng: Rng(seed | 1),
            last: None,
            acc: 0.0,
            settled: false,
            next_wander: None,
            next_cover: 0.0,
            last_cover_move: f64::NEG_INFINITY,
            goal: None,
            last_trip: None,
            waits: Vec::new(),
            last_touch: None,
            asked: false,
            trips: Vec::new(),
        }
    }

    pub fn body(&self) -> &Body {
        &self.body
    }

    pub fn ground(&self) -> (f64, f64) {
        (self.body.x, self.body.y)
    }

    pub fn mode(&self) -> Mode {
        self.body.mode
    }

    /// Moving, falling or landing (the backend redraws at full rate).
    pub fn moving(&self) -> bool {
        self.body.busy()
    }

    /// Turns her to face right (or left).
    pub fn face(&mut self, right: bool) {
        self.body.facing_right = right;
    }

    /// Picked up by the mouse.
    pub fn grab(&mut self) {
        self.goal = None;
        self.body.grab();
    }

    /// Carried to ground point `(x, y)`.
    pub fn carry(&mut self, x: f64, y: f64) {
        self.body.x = x;
        self.body.y = y;
        self.body.fall_top = y;
    }

    /// Let go: she falls to whatever is below.
    pub fn release(&mut self) {
        self.body.release();
        self.body.fall_top = self.body.y;
        self.settled = true;
    }

    /// Put somewhere at once (another pet, a new screen layout): she lands
    /// without falling.
    pub fn place(&mut self, at: (f64, f64)) {
        self.body = Body::new(at.0, at.1);
        self.goal = None;
        self.settled = false;
    }

    /// Lift her `height` points and let go (`mascota ver … caer`).
    pub fn drop_from(&mut self, height: f64) {
        self.goal = None;
        self.body.grab();
        self.body.y += height;
        self.body.release();
        self.body.fall_top = self.body.y;
        self.settled = true;
    }

    /// Time to wander now, anywhere (`mascota ver … pasear`).
    pub fn wander_soon(&mut self, now: f64) {
        self.next_wander = Some(now);
        self.asked = true;
    }

    /// Advances to `now` (seconds, any origin) in `world`.
    pub fn tick(&mut self, now: f64, world: &dyn World, ctx: Ctx) -> Out {
        let dt = self
            .last
            .map_or(0.0, |last| (now - last).clamp(0.0, MAX_DT));
        self.last = Some(now);
        let mut out = Out {
            ground: self.ground(),
            mode: self.body.mode,
            loco: None,
            facing_right: self.body.facing_right,
            perched: false,
            wake: false,
            settled: false,
            touchdown: None,
        };
        let screens = world.screens();
        if screens.is_empty() {
            return out;
        }
        if self.version != Some(world.version()) || self.ledges.is_empty() {
            self.version = Some(world.version());
            self.ledges = ledges(screens, world.windows(), self.size);
        }
        if self.next_wander.is_none() {
            self.next_wander = Some(now + self.rng.between(self.params.wander_every));
        }
        if !self.settled {
            // Where she was left last time: straight onto what is below.
            self.settled = true;
            if self.body.mode != Mode::Agarrada
                && support(&self.ledges, self.body.x, self.body.y).is_none()
            {
                let (x, y) = world::clamp_to_screens(screens, self.body.x, self.body.y, self.size);
                self.body.x = x;
                self.body.y = y;
                self.body.snap_down(&self.ledges);
            }
        }
        let fullscreen = fullscreen_active(world.windows(), screens);
        // Reduce Motion is Wawi 1's calm too: no trips at all.
        self.body.paused = ctx.calm || fullscreen || ctx.still;
        if (ctx.still || fullscreen) && self.body.mode.falling() {
            self.body.snap_down(&self.ledges);
            out.settled = true;
        }
        self.acc = (self.acc + dt).min(MAX_DT);
        while self.acc >= STEP {
            self.acc -= STEP;
            match self.body.update(STEP, &self.ledges, &self.params) {
                Some(Event::Touchdown { touch, .. }) => {
                    out.touchdown = Some(touch);
                    self.last_touch = Some(touch);
                }
                Some(Event::Landed) => {
                    out.settled = true;
                    self.replan();
                }
                Some(Event::Arrived) => {
                    self.goal = None;
                    self.face_inward();
                    out.settled = true;
                }
                Some(Event::Fell) | None => {}
            }
        }
        if (ctx.still || fullscreen) && self.body.mode.falling() {
            self.body.snap_down(&self.ledges);
            out.settled = true;
        }
        if !ctx.calm && !ctx.still && !fullscreen && self.goal.is_none() && !self.body.busy() {
            self.decide(now, world, ctx, &mut out);
        }
        let body = &self.body;
        out.ground = (body.x, body.y);
        out.mode = body.mode;
        out.facing_right = body.facing_right;
        out.perched = body.mode == Mode::Posada;
        out.loco = match body.mode {
            Mode::Quieta | Mode::Posada => None,
            Mode::Caminando => Some(Loco::Walk),
            Mode::Trotando => Some(Loco::Trot),
            Mode::Trepando => Some(Loco::Climb),
            Mode::Cayendo => Some(Loco::Fall),
            Mode::Paracaidas => Some(Loco::Chute),
            Mode::Agarrada => Some(Loco::Held),
            Mode::Aterrizando => Some(match self.last_touch {
                Some(Touch::Flop) => Loco::Flop,
                Some(Touch::Chute) => Loco::ChuteLand,
                _ => Loco::Land,
            }),
        };
        out
    }

    /// At the end of a ledge she faces back in, not into the wall.
    fn face_inward(&mut self) {
        if let Some(l) = self.body.ledge
            && l.x1 - l.x0 > 8.0
        {
            if self.body.x - l.x0 <= 2.0 {
                self.body.facing_right = true;
            } else if l.x1 - self.body.x <= 2.0 {
                self.body.facing_right = false;
            }
        }
    }

    /// A trip broken by a fall goes on from where she landed.
    fn replan(&mut self) {
        let Some(mut goal) = self.goal.take() else {
            return;
        };
        if goal.replans >= MAX_REPLANS {
            return;
        }
        // The window she was going to may have moved or gone.
        if let Some(target) = goal.ledge
            && target.window.is_some()
        {
            match self.ledges.iter().find(|l| l.window == target.window) {
                Some(now) => {
                    goal.target = (now.clamp(goal.target.0), now.y);
                    goal.ledge = Some(*now);
                }
                None => return,
            }
        }
        let here = support(&self.ledges, self.body.x, self.body.y);
        let legs = plan_path(
            here,
            self.ground(),
            goal.target,
            goal.ledge,
            self.size,
            self.params.walk_speed,
        );
        if legs.is_empty() {
            return;
        }
        goal.replans += 1;
        self.body.go(legs);
        self.goal = Some(goal);
    }

    fn decide(&mut self, now: f64, world: &dyn World, ctx: Ctx, out: &mut Out) {
        if !self.params.walks {
            return;
        }
        let mut due = self.next_wander.is_some_and(|at| now >= at);
        if now >= self.next_cover {
            self.next_cover = now + COVER_CHECK;
            if let Some(front) = world.windows().first()
                && now - self.last_cover_move >= COVER_EVERY
                && intersects(
                    her_rect(self.body.x, self.body.y, self.size),
                    front.content(),
                )
            {
                self.last_cover_move = now;
                due = true;
                self.next_wander = Some(now);
            }
        }
        if !due {
            return;
        }
        if ctx.asleep {
            // A dog does not sleepwalk: she gets up first.
            out.wake = true;
            return;
        }
        if !ctx.free {
            return;
        }
        self.next_wander = Some(now + self.rng.between(self.params.wander_every));
        self.wander(now, world, ctx);
    }

    /// May this kind of trip start now? (The anti-repetition rule.)
    pub fn allowed(&self, trip: Trip, now: f64) -> bool {
        if !trip.special() {
            return true;
        }
        if self.last_trip == Some(trip) {
            return false;
        }
        !self
            .waits
            .iter()
            .any(|(t, until)| *t == trip && now < *until)
    }

    fn started(&mut self, trip: Trip, now: f64) {
        self.last_trip = Some(trip);
        self.trips.push(trip);
        let wait = match trip {
            Trip::Climb => self.params.climb_every,
            Trip::Chute => self.params.chute_every,
            Trip::Trot => self.params.trot_every,
            Trip::Walk | Trip::Down => 0.0,
        };
        self.waits.retain(|(t, _)| *t != trip);
        if wait > 0.0 {
            self.waits.push((trip, now + wait));
        }
    }

    /// Somewhere new to sit, Wawi 1's `choose_spot`: not too far, not over
    /// the front window's text or the cursor, fully on a screen; nearer
    /// spots, her own level and the owner's side likelier.
    fn wander(&mut self, now: f64, world: &dyn World, ctx: Ctx) {
        let here = support(&self.ledges, self.body.x, self.body.y);
        let (x, y) = self.ground();
        let avoid = if std::mem::take(&mut self.asked) {
            Vec::new()
        } else {
            avoid_rects(world.windows().first(), ctx.cursor)
        };
        let mut options: Vec<(f64, Ledge, f64, Trip)> = Vec::new();
        for led in self.ledges.clone() {
            for _ in 0..3 {
                let tx = if led.x1 > led.x0 {
                    led.x0 + (led.x1 - led.x0) * self.rng.next()
                } else {
                    led.x0
                };
                let d = (tx - x).hypot(led.y - y);
                if !(MIN_TRIP..=MAX_TRIP).contains(&d) {
                    continue;
                }
                let r = her_rect(tx, led.y, self.size);
                if avoid.iter().any(|a| intersects(r, *a)) || !on_screen(world.screens(), r) {
                    continue;
                }
                let trip = if (led.y - y).abs() <= 2.0 {
                    Trip::Walk
                } else if led.y > y {
                    Trip::Climb
                } else {
                    Trip::Down
                };
                if trip == Trip::Climb && !(self.params.climbs && self.allowed(Trip::Climb, now)) {
                    continue;
                }
                let mut weight = if led.window.is_some() {
                    TOP_WEIGHT
                } else {
                    1.0
                };
                weight /= 1.0 + d / 300.0;
                if trip == Trip::Walk {
                    weight *= SAME_LEVEL_WEIGHT;
                }
                if let Some((cx, cy)) = ctx.cursor {
                    weight /= 1.0 + (tx - cx).hypot(led.y - cy) / 300.0;
                }
                options.push((weight, led, tx, trip));
            }
        }
        let total: f64 = options.iter().map(|o| o.0).sum();
        if options.is_empty() || total <= 0.0 {
            return;
        }
        let mut pick = self.rng.next() * total;
        let mut chosen = options[options.len() - 1];
        for option in &options {
            pick -= option.0;
            if pick < 0.0 {
                chosen = *option;
                break;
            }
        }
        let (_, led, tx, mut trip) = chosen;
        let mut speed = self.params.walk_speed;
        if trip == Trip::Walk
            && (tx - x).abs() >= TROT_FROM
            && self.params.trot_chance > 0.0
            && self.allowed(Trip::Trot, now)
            && self.rng.next() < self.params.trot_chance
        {
            trip = Trip::Trot;
            speed = self.params.trot_speed;
        }
        if trip == Trip::Down
            && self.params.chutes
            && self.params.chute_chance > 0.0
            && let Some(h) = here.filter(|h| h.window.is_some())
            && self.allowed(Trip::Chute, now)
        {
            let edge = if tx < x {
                h.x0 - self.size.0 * 0.9
            } else {
                h.x1 + self.size.0 * 0.9
            };
            let drop = landing(&self.ledges, edge, h.y - 3.0).map(|l| h.y - l.y);
            if drop.is_some_and(|d| d >= self.params.chute_min_drop)
                && self.rng.next() < self.params.chute_chance
            {
                trip = Trip::Chute;
                self.body.chute = true;
            }
        }
        let legs = plan_path(here, (x, y), (tx, led.y), Some(led), self.size, speed);
        if legs.is_empty() {
            self.body.chute = false;
            return;
        }
        self.started(trip, now);
        self.body.go(legs);
        self.goal = Some(Goal {
            target: (tx, led.y),
            ledge: Some(led),
            replans: 0,
        });
    }
}
