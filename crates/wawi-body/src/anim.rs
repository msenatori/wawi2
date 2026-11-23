//! The flip-book animator: which frame to show, how it crossfades, the
//! whole-sprite pose on top and the little badge over her head.
//!
//! It started as a port of Wawi 1's `MascotAnimator` (`wawi/mascot.py`); now
//! the frames, clips and timings come from the active pet pack
//! ([`crate::pack`]), so another pet plays by the same rules: a look per
//! dialogue state (with fallbacks), one-shot reactions, idle life (blinks,
//! gestures, falling asleep) and named actions.
//!
//! Low CPU is part of the design: [`Animator::next_frame_in`] says how long
//! nothing will change, so the backend only redraws when something moves.
//! While she is idle or asleep that is a few times a second at most.

use std::f64::consts::PI;
use std::sync::Arc;

use crate::mood::{Flash, Mood};
use crate::pack::{Look, Pack};

/// A frame of the active pack (an index into [`Pack::frames`]).
pub type FrameId = u16;

/// Redraw rate while something moves, and while only a slow dissolve runs.
pub const FPS: f64 = 30.0;
pub const SLOW_FPS: f64 = 15.0;

/// Syllable rate of the talking rhythm, in Hz.
const TALK_HZ: f64 = 3.1;
const MOUTH_HALF: f64 = 0.18;
const MOUTH_OPEN: f64 = 0.5;
const MOUTH_HOLD: f64 = 0.06;
/// How much likelier the muted gesture is than any other, while muted.
const MUTED_WEIGHT: usize = 3;

/// How the whole sprite moves over a clip step. The body's motions are
/// Wawi 1's (`wawi/mascot.py`, `motion_overlay`), with the same amplitudes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Motion {
    Still,
    /// A little jump.
    Hop,
    /// Jerked back (interrupted).
    Recoil,
    /// Squashed on landing, wider and lower, once over the step
    /// (`aplastar`).
    Squash,
    /// Turning over slowly while she falls (`tumbo`).
    Tumble,
    /// The bob of a walk (`paso`).
    Walk,
    /// The bigger bounce of a trot (`galope`).
    Gallop,
    /// Side to side while she climbs (`trepar`).
    Climb,
    /// Swinging under the parachute, one way or the other (`vaiven_izq`,
    /// `vaiven_der`).
    SwayLeft,
    SwayRight,
    /// Seeing stars, after a hard landing (`mareo`).
    Dizzy,
    /// A fast shake (`sacudida`).
    Shake,
    /// Chin up, pleased with herself (`orgullo`).
    Proud,
}

impl Motion {
    /// Moves every frame (the backend redraws at full rate while it plays).
    pub fn moves(self) -> bool {
        self != Motion::Still
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    pub frame: FrameId,
    pub secs: f64,
    pub motion: Motion,
}

/// A named sequence of frames: looping (a state) or one-shot (a gesture, a
/// reaction, an action).
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub name: String,
    pub steps: Vec<Step>,
    pub looped: bool,
    /// Crossfade between its frames, in seconds.
    pub xfade: f64,
    /// What plays after it, once it ends (`siguiente`).
    pub next: Option<String>,
    /// Other clips that may play in its place, picked at random.
    pub variants: Vec<String>,
    /// Lip-sync frames, closed, half and open: speaking follows the talking
    /// rhythm with these instead of the steps.
    pub mouth: Option<[FrameId; 3]>,
    /// What it does, in a few words (`descripcion`; actions).
    pub description: Option<String>,
    /// A frame drawn with her while it plays (`capa`: the parachute's
    /// canopy).
    pub layer: Option<Layer>,
}

/// A frame drawn together with hers, like the parachute's canopy: its own
/// box moved by `at` (fractions of the frame: `[0.36, 0.99]` is a bit to the
/// front and a whole frame up). It mirrors and sways with her.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layer {
    pub frame: FrameId,
    pub at: (f64, f64),
}

impl Clip {
    pub fn duration(&self) -> f64 {
        self.steps.iter().map(|s| s.secs).sum()
    }

    /// The step showing at clip time `t`, and how far into it.
    pub fn step_at(&self, t: f64) -> (&Step, f64) {
        let total = self.duration();
        let last = self.steps.last().expect("clips have steps");
        if total <= 0.0 {
            return (last, 0.0);
        }
        let mut t = t.max(0.0);
        if self.looped {
            t %= total;
        } else if t >= total {
            return (last, last.secs);
        }
        let mut acc = 0.0;
        for step in &self.steps {
            if t < acc + step.secs {
                return (step, t - acc);
            }
            acc += step.secs;
        }
        (last, last.secs)
    }

    /// Seconds from clip time `t` to the next step change (or the end).
    pub fn until_next_step(&self, t: f64) -> f64 {
        let (step, inside) = self.step_at(t);
        (step.secs - inside).max(0.0)
    }

    /// Played once: has it ended? (A one-shot reaction or action plays a
    /// looping clip once through too.)
    pub fn done_once(&self, t: f64) -> bool {
        t >= self.duration()
    }
}

/// One frame of the sprite relative to its resting place. Scale and rotation
/// pivot on her bottom-centre, where she touches the ground. `rot` is in
/// degrees, counter-clockwise; amplitudes are in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub dx: f64,
    pub dy: f64,
    pub sx: f64,
    pub sy: f64,
    pub rot: f64,
}

pub const REST: Pose = Pose {
    dx: 0.0,
    dy: 0.0,
    sx: 1.0,
    sy: 1.0,
    rot: 0.0,
};

/// A single smooth hump over 0..1, zero elsewhere.
fn bump(u: f64) -> f64 {
    if u > 0.0 && u < 1.0 {
        (PI * u).sin()
    } else {
        0.0
    }
}

/// A synthetic mouth, 0..1: a steady syllable beat with phrase-length swells
/// (Wawi 1's fallback when the real voice level is not known).
pub fn talk_envelope(t: f64) -> f64 {
    let syllable = 0.5 - 0.5 * (2.0 * PI * TALK_HZ * t).cos();
    let phrase = 0.7 + 0.3 * (2.0 * PI * t / 1.9).sin();
    syllable.powf(1.4) * phrase
}

/// Wawi 1's `framed_pose`: the motion that supports the frames.
pub fn mood_pose(mood: Mood, t: f64) -> Pose {
    let breathe = (2.0 * PI * t / 3.6).sin();
    match mood {
        Mood::Listening => {
            let cock = bump((t % 4.4) / 1.1);
            Pose {
                dy: 2.0 + 0.3 * (2.0 * PI * t / 1.6).sin(),
                sy: 1.02 + 0.003 * breathe,
                rot: -2.0 - 1.5 * cock,
                ..REST
            }
        }
        Mood::Speaking => {
            let env = talk_envelope(t);
            let sway = (2.0 * PI * t / 1.15).sin();
            Pose {
                dx: 0.6 * sway,
                dy: 0.2 + 2.5 * env,
                sx: 1.01 - 0.02 * env,
                sy: 0.99 + 0.035 * env,
                rot: 1.2 * sway,
            }
        }
        Mood::Thinking => {
            let sway = (2.0 * PI * t / 2.6).sin();
            Pose {
                dx: 0.8 * sway,
                rot: 2.0 * sway,
                ..REST
            }
        }
        Mood::Transcribing => {
            // Head cocked, very slowly rocking: "let me get that".
            let sway = (2.0 * PI * t / 3.2).sin();
            Pose {
                rot: 1.0 * sway,
                ..REST
            }
        }
        // Idle stays still: the idle_a/idle_b frames breathe, and a still
        // pose lets the window sleep between frame changes.
        Mood::Idle => REST,
    }
}

/// Wawi 1's `_plateau`: up over the first 0.15 s of a step, held, and down
/// over its last 0.15 s.
fn plateau(t: f64, d: f64) -> f64 {
    if d <= 0.0 {
        return 0.0;
    }
    let edge = 0.15f64.min(d / 2.0);
    (t / edge).min((d - t) / edge).clamp(0.0, 1.0)
}

/// The pose of a step's motion, `t` seconds into a step of `d` seconds.
fn motion_pose(motion: Motion, t: f64, d: f64) -> Pose {
    let u = if d > 0.0 {
        (t / d).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let k = plateau(t, d);
    let tau = 2.0 * PI;
    match motion {
        Motion::Still => REST,
        Motion::Squash => {
            let b = bump(u);
            Pose {
                sx: 1.0 + 0.06 * b,
                sy: 1.0 - 0.08 * b,
                ..REST
            }
        }
        Motion::Tumble => Pose {
            rot: 6.0 * (tau * 1.3 * t).sin(),
            ..REST
        },
        Motion::Walk => Pose {
            dy: 0.9 * (tau * 3.1 * t).sin().abs(),
            ..REST
        },
        Motion::Gallop => Pose {
            dy: 1.5 * (tau * 5.5 * t).sin().abs(),
            ..REST
        },
        Motion::Climb => Pose {
            dx: 0.6 * (tau * 1.7 * t).sin(),
            ..REST
        },
        Motion::SwayLeft | Motion::SwayRight => {
            let side = if motion == Motion::SwayLeft {
                1.0
            } else {
                -1.0
            };
            let s = side * (PI * u).sin();
            Pose {
                dx: 1.5 * s,
                rot: 5.0 * s,
                ..REST
            }
        }
        Motion::Dizzy => Pose {
            rot: 2.5 * (tau * 1.5 * t).sin(),
            ..REST
        },
        Motion::Shake => {
            let w = (tau * 11.0 * t).sin();
            Pose {
                dx: 1.5 * w,
                rot: 3.0 * w,
                ..REST
            }
        }
        Motion::Proud => Pose {
            dy: 1.2 * k,
            rot: -1.5 * k,
            ..REST
        },
        Motion::Hop => Pose {
            dy: 14.0 * bump(u),
            sy: 1.0 + 0.04 * bump(u),
            ..REST
        },
        Motion::Recoil => {
            let k = bump(u);
            Pose {
                dx: 3.0 * k,
                rot: -4.0 * k,
                sy: 1.0 - 0.03 * k,
                ..REST
            }
        }
    }
}

fn combine(a: Pose, b: Pose) -> Pose {
    Pose {
        dx: a.dx + b.dx,
        dy: a.dy + b.dy,
        sx: a.sx * b.sx,
        sy: a.sy * b.sy,
        rot: a.rot + b.rot,
    }
}

/// Opacities `(previous, current)` for a crossfade at `mix` 0..1: the new
/// frame comes in over the first half while the old one stays solid, the old
/// one goes over the second half (no see-through moment mid-way).
pub fn crossfade_alphas(mix: f64) -> (f64, f64) {
    let mix = mix.clamp(0.0, 1.0);
    ((2.0 * (1.0 - mix)).min(1.0), (2.0 * mix).min(1.0))
}

/// The tiny mark over her head.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Badge {
    /// Green dot: this is being recorded.
    Listening,
    /// The microphone is off (the drawn icon, or a hollow grey ring).
    Muted,
    /// Three dots, one hopping at a time: heights 0..1.
    Musing([f64; 3]),
}

/// The "…" wave: one period, in seconds.
pub const MUSING_PERIOD: f64 = 1.2;

pub fn musing_dots(t: f64) -> [f64; 3] {
    let u = (t % MUSING_PERIOD) / MUSING_PERIOD;
    [0, 1, 2].map(|i| bump((u - f64::from(i) * 0.22) / 0.4))
}

/// Which badge, if any. The mute wins: nothing else on screen says it.
pub fn badge(mood: Mood, muted: bool, t: f64) -> Option<Badge> {
    if muted {
        return Some(Badge::Muted);
    }
    match mood {
        Mood::Listening => Some(Badge::Listening),
        Mood::Transcribing => Some(Badge::Musing(musing_dots(t))),
        _ => None,
    }
}

/// Everything the backend needs to draw her now.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub frame: Option<FrameId>,
    /// The frame dissolving away, and the crossfade 0..1.
    pub previous: Option<FrameId>,
    pub mix: f64,
    pub pose: Pose,
    pub badge: Option<Badge>,
    /// Drawn mirrored: she faces the other way from her drawings.
    pub mirror: bool,
    /// A frame drawn with her (see [`Layer`]).
    pub layer: Option<Layer>,
}

/// A tiny xorshift: enough to pick a behaviour, and seedable for tests.
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

    fn pick(&mut self, len: usize) -> usize {
        ((self.next() * len as f64) as usize).min(len.saturating_sub(1))
    }
}

/// What a one-shot is, which decides what may cut it short.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Her idle life (a blink, a gesture, lying down, waking, the click):
    /// dropped when the mood changes.
    Gesture,
    /// Asked for by name ([`Animator::play_action`]): plays over every mood
    /// until it ends.
    Action,
    /// Interrupted, error: plays over everything.
    Reaction,
}

/// One-shots chained through `siguiente` stop after this many hops, so a
/// cycle of one-shots cannot hold her forever.
const MAX_HOPS: u8 = 8;

/// A gesture of her idle life does not come back for this long (seconds),
/// as long as she has another one to do: variety without a loop.
pub const GESTURE_COOLDOWN: f64 = 60.0;

/// The body's clip while she moves (walking, falling, landing…): it wins
/// over everything while it lasts (Wawi 1's locomotion).
#[derive(Clone, Debug)]
struct Moving {
    clip: Clip,
    t: f64,
}

#[derive(Clone, Debug)]
struct Playing {
    clip: Clip,
    t: f64,
    kind: Kind,
    hops: u8,
}

/// The current look's clip, chained through `siguiente`.
#[derive(Clone, Debug)]
struct StateClip {
    look: Look,
    /// The clip the look asked for (before `siguiente` and variants).
    asked: String,
    /// The name asked for (the clip may be one of its variants).
    base: String,
    clip: Clip,
    t: f64,
}

pub struct Animator {
    pack: Arc<Pack>,
    mood: Mood,
    muted: bool,
    reduce_motion: bool,
    /// Seconds since the animator started (talking rhythm, sways).
    clock: f64,
    idle_for: f64,
    asleep: bool,
    state: Option<StateClip>,
    playing: Option<Playing>,
    next_blink: f64,
    next_behaviour: f64,
    last_behaviour: Option<String>,
    mouth: usize,
    mouth_hold: f64,
    shown: Option<FrameId>,
    previous: Option<FrameId>,
    xfade: f64,
    xfade_left: f64,
    rng: Rng,
    /// The body's clip, while she moves.
    moving: Option<Moving>,
    /// She faces right (the body turns her; the art says which way it is
    /// drawn).
    facing_right: bool,
    /// Resting on a window's top edge: idle uses the pack's `posada`.
    perched: bool,
    /// A squash drawn by code (a landing without a landing clip): seconds
    /// left and how long it lasts.
    squash_left: f64,
    squash_len: f64,
    /// When each gesture last played, on [`Animator::clock`] (the
    /// anti-repetition rule).
    gesture_played: Vec<(String, f64)>,
}

impl Animator {
    pub fn new(pack: Arc<Pack>) -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x5EED);
        Self::with_seed(pack, seed)
    }

    pub fn with_seed(pack: Arc<Pack>, seed: u64) -> Self {
        let mut rng = Rng(seed | 1);
        let next_blink = rng.between(pack.life.blink_gap);
        let next_behaviour = rng.between(pack.life.gesture_gap);
        let mut animator = Self {
            pack,
            mood: Mood::Idle,
            muted: false,
            reduce_motion: false,
            clock: 0.0,
            idle_for: 0.0,
            asleep: false,
            state: None,
            playing: None,
            next_blink,
            next_behaviour,
            last_behaviour: None,
            mouth: 0,
            mouth_hold: 0.0,
            shown: None,
            previous: None,
            xfade: 0.0,
            xfade_left: 0.0,
            rng,
            moving: None,
            facing_right: false,
            perched: false,
            squash_left: 0.0,
            squash_len: 0.0,
            gesture_played: Vec::new(),
        };
        animator.facing_right = animator.art_faces_right();
        animator.sync_state();
        animator
    }

    pub fn pack(&self) -> &Arc<Pack> {
        &self.pack
    }

    /// Whether there is anything to draw at all.
    pub fn has_frames(&self) -> bool {
        self.pack.look_clip(Look::Idle).is_some()
    }

    pub fn mood(&self) -> Mood {
        self.mood
    }

    pub fn asleep(&self) -> bool {
        self.asleep
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    pub fn set_reduce_motion(&mut self, on: bool) {
        self.reduce_motion = on;
    }

    fn art_faces_right(&self) -> bool {
        self.pack.body.as_ref().is_some_and(|b| b.faces_right)
    }

    /// Shows the body moving with the pack's clip `name` (walking, falling,
    /// landing…), or stops (`None`). It wins over everything while it lasts,
    /// even a voice turn (the body only moves then to finish a fall). The
    /// same name again keeps its clock. False if the pack has no such clip
    /// (then nothing changes on screen).
    pub fn set_locomotion(&mut self, name: Option<&str>) -> bool {
        let Some(name) = name else {
            self.moving = None;
            return true;
        };
        if self.moving.as_ref().is_some_and(|m| m.clip.name == name) {
            return true;
        }
        let Some(clip) = self.pack.clip(name).cloned() else {
            self.moving = None;
            return false;
        };
        if self
            .playing
            .as_ref()
            .is_some_and(|p| p.kind == Kind::Gesture)
        {
            self.playing = None;
        }
        if self.asleep {
            self.asleep = false;
            self.sync_state();
        }
        self.idle_for = 0.0;
        self.moving = Some(Moving { clip, t: 0.0 });
        true
    }

    /// The body's clip on screen, if any.
    pub fn locomotion(&self) -> Option<&str> {
        self.moving.as_ref().map(|m| m.clip.name.as_str())
    }

    /// Drawn mirrored now (she faces the other way from her drawings).
    pub fn mirrored(&self) -> bool {
        self.facing_right != self.art_faces_right()
    }

    /// Which way she faces; the drawing is mirrored when the pack's art
    /// faces the other way.
    pub fn set_facing(&mut self, right: bool) {
        self.facing_right = right;
    }

    /// Resting on a window's top edge: idle shows the pack's `posada`.
    pub fn set_perched(&mut self, perched: bool) {
        if perched != self.perched {
            self.perched = perched;
            self.sync_state();
        }
    }

    /// A squash drawn by code for `secs` (a landing, when the pack has no
    /// landing clip).
    pub fn squash(&mut self, secs: f64) {
        self.squash_len = secs.max(0.05);
        self.squash_left = self.squash_len;
    }

    /// An action or a reaction is playing: the body stays where it is.
    pub fn busy(&self) -> bool {
        self.playing
            .as_ref()
            .is_some_and(|p| p.kind != Kind::Gesture)
    }

    /// Awake, idle, and doing nothing: free to get up and go somewhere.
    pub fn free(&self) -> bool {
        self.mood == Mood::Idle && !self.asleep && self.playing.is_none()
    }

    /// Gets up if she is asleep (her `despertarse`): she is about to go
    /// somewhere.
    pub fn rouse(&mut self) {
        if self.asleep && self.mood == Mood::Idle {
            self.poke();
        }
    }

    pub fn set_mood(&mut self, mood: Mood) {
        if mood == self.mood {
            return;
        }
        self.mood = mood;
        self.idle_for = 0.0;
        if self
            .playing
            .as_ref()
            .is_some_and(|p| p.kind == Kind::Gesture)
        {
            self.playing = None;
        }
        if mood != Mood::Idle {
            // The key wakes her at once: no stretch first.
            self.asleep = false;
        } else {
            self.next_behaviour = self.rng.between(self.pack.life.gesture_gap);
        }
        self.sync_state();
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        self.sync_state();
    }

    pub fn flash(&mut self, flash: Flash) {
        let look = match flash {
            Flash::Interrupted => Look::Interrupted,
            Flash::Error => Look::Error,
        };
        if let Some(name) = self.pack.look_clip(look).map(str::to_owned) {
            self.play(&name, Kind::Reaction);
        }
    }

    /// A click on her: her `toque`, or waking up if she was asleep.
    pub fn poke(&mut self) {
        if self
            .playing
            .as_ref()
            .is_some_and(|p| p.kind != Kind::Gesture)
        {
            return;
        }
        self.idle_for = 0.0;
        if self.asleep {
            self.wake();
            if let Some(name) = self.pack.life.wake.clone() {
                self.play(&name, Kind::Gesture);
            }
        } else if let Some(name) = self.pack.life.poke.clone() {
            self.play(&name, Kind::Gesture);
        }
    }

    /// Plays a named action of the pack (or, for previews, any animation by
    /// name) once, over whatever she is doing. False if the pack has no such
    /// thing, or a reaction is playing.
    pub fn play_action(&mut self, name: &str) -> bool {
        if self
            .playing
            .as_ref()
            .is_some_and(|p| p.kind == Kind::Reaction)
        {
            return false;
        }
        let clip = match self.pack.action_clip(name) {
            Some(clip) => clip.to_owned(),
            None if self.pack.clip(name).is_some() => name.to_owned(),
            None => return false,
        };
        self.idle_for = 0.0;
        if self.asleep {
            self.wake();
        }
        self.play(&clip, Kind::Action)
    }

    fn wake(&mut self) {
        self.asleep = false;
        self.sync_state();
    }

    /// The clip `name` or one of its variants, at random.
    fn start(&mut self, name: &str) -> Option<Clip> {
        let clip = self.pack.clip(name)?;
        let mut pool: Vec<&Clip> = vec![clip];
        pool.extend(clip.variants.iter().filter_map(|v| self.pack.clip(v)));
        let i = if pool.len() > 1 {
            self.rng.pick(pool.len())
        } else {
            0
        };
        Some(pool[i].clone())
    }

    fn play(&mut self, name: &str, kind: Kind) -> bool {
        match self.start(name) {
            Some(clip) => {
                self.playing = Some(Playing {
                    clip,
                    t: 0.0,
                    kind,
                    hops: 0,
                });
                true
            }
            None => false,
        }
    }

    /// The look her state calls for now.
    fn current_look(&self) -> Look {
        match self.mood {
            Mood::Idle if self.asleep => Look::Sleeping,
            Mood::Idle if self.muted && self.muted_loops() => Look::Muted,
            Mood::Idle => Look::Idle,
            Mood::Listening => Look::Listening,
            Mood::Transcribing => Look::Transcribing,
            Mood::Thinking => Look::Thinking,
            Mood::Speaking => Look::Speaking,
        }
    }

    /// A looping `muted` animation replaces idle while muted; a one-shot
    /// one is a gesture she does often.
    fn muted_loops(&self) -> bool {
        self.pack
            .own_look(Look::Muted)
            .and_then(|name| self.pack.clip(name))
            .is_some_and(|clip| clip.looped)
    }

    /// The clip for a look: the pack's `posada` replaces idle on a window.
    fn look_name(&self, look: Look) -> Option<String> {
        if look == Look::Idle
            && self.perched
            && let Some(perch) = self.pack.body.as_ref().and_then(|b| b.perch.as_deref())
            && self.pack.clip(perch).is_some()
        {
            return Some(perch.to_owned());
        }
        self.pack.look_clip(look).map(str::to_owned)
    }

    fn sync_state(&mut self) {
        let look = self.current_look();
        let name = self.look_name(look);
        if self
            .state
            .as_ref()
            .is_some_and(|s| s.look == look && Some(&s.asked) == name.as_ref())
        {
            return;
        }
        self.state = name.and_then(|base| {
            let clip = self.start(&base)?;
            Some(StateClip {
                look,
                asked: base.clone(),
                base,
                clip,
                t: 0.0,
            })
        });
    }

    fn advance_state(&mut self, dt: f64) {
        let Some(state) = &mut self.state else {
            return;
        };
        state.t += dt;
        let total = state.clip.duration();
        if total <= 0.0 || state.t < total {
            return;
        }
        let (base, t) = if state.clip.looped {
            // A new lap: maybe another variant.
            let has_variants = self
                .pack
                .clip(&state.base)
                .is_some_and(|c| !c.variants.is_empty());
            if !has_variants {
                return;
            }
            (state.base.clone(), state.t % total)
        } else {
            match state.clip.next.clone() {
                Some(next) => (next, state.t - total),
                None => return,
            }
        };
        if let Some(clip) = self.start(&base)
            && let Some(state) = &mut self.state
        {
            state.t = t.min(clip.duration());
            state.clip = clip;
            state.base = base;
        }
    }

    /// Advances `dt` seconds and returns what to draw.
    pub fn update(&mut self, dt: f64) -> Scene {
        let dt = dt.clamp(0.0, 1.0);
        self.clock += dt;
        self.mouth_hold = (self.mouth_hold - dt).max(0.0);
        self.squash_left = (self.squash_left - dt).max(0.0);
        if let Some(moving) = &mut self.moving {
            moving.t += dt;
            self.idle_for = 0.0;
        }
        if let Some(playing) = &mut self.playing {
            playing.t += dt;
            if playing.clip.done_once(playing.t) {
                let ended = playing.clip.name.clone();
                let over = playing.t - playing.clip.duration();
                let (kind, hops) = (playing.kind, playing.hops);
                let next = playing.clip.next.clone();
                self.playing = None;
                if self.pack.life.lie_down.as_deref() == Some(ended.as_str()) {
                    self.asleep = true;
                }
                if let Some(next) = next
                    && hops < MAX_HOPS
                    && let Some(clip) = self.start(&next)
                {
                    self.playing = Some(Playing {
                        t: over.min(clip.duration()),
                        clip,
                        kind,
                        hops: hops + 1,
                    });
                }
            }
        }
        if self.mood == Mood::Idle && !self.asleep && self.moving.is_none() {
            self.idle_life(dt);
        }
        self.sync_state();
        self.advance_state(dt);
        let (want, xfade) = self.choose_frame();
        self.show(want, dt, xfade);
        Scene {
            frame: self.shown,
            previous: self.previous,
            mix: self.mix(),
            pose: self.pose(),
            badge: badge(self.mood, self.muted, self.clock),
            mirror: self.mirrored(),
            layer: self.current_clip().and_then(|c| c.layer),
        }
    }

    fn idle_life(&mut self, dt: f64) {
        self.idle_for += dt;
        if self.playing.is_some() {
            return;
        }
        let life = &self.pack.life;
        if life.sleep_after > 0.0 && self.idle_for >= life.sleep_after {
            let lie_down = life.lie_down.clone();
            if !lie_down.is_some_and(|name| self.play(&name, Kind::Gesture)) {
                self.asleep = true;
            }
            return;
        }
        self.next_blink -= dt;
        self.next_behaviour -= dt;
        if self.next_behaviour <= 0.0 {
            self.next_behaviour = self.rng.between(self.pack.life.gesture_gap);
            if let Some(name) = self.pick_behaviour() {
                self.play(&name, Kind::Gesture);
                self.gesture_played.retain(|(n, _)| *n != name);
                self.gesture_played.push((name.clone(), self.clock));
                self.last_behaviour = Some(name);
                return;
            }
        }
        if self.next_blink <= 0.0 {
            self.next_blink = self.rng.between(self.pack.life.blink_gap);
            if let Some(name) = self.pack.life.blink.clone() {
                self.play(&name, Kind::Gesture);
            }
        }
    }

    /// A gesture the pack can play, never the one she just did.
    fn pick_behaviour(&mut self) -> Option<String> {
        let mut pool: Vec<String> = self
            .pack
            .life
            .gestures
            .iter()
            .filter(|name| self.pack.clip(name).is_some())
            .cloned()
            .collect();
        if self.muted
            && !self.muted_loops()
            && let Some(name) = self.pack.own_look(Look::Muted)
        {
            // Muted, her muted gesture is the likeliest thing she does.
            pool.extend(std::iter::repeat_n(name.to_owned(), MUTED_WEIGHT));
        }
        if pool.len() > 1 {
            pool.retain(|name| Some(name) != self.last_behaviour.as_ref());
        }
        // Anti-repetition: what she did in the last minute waits its turn,
        // while there is something else to do.
        let recent = |name: &String| {
            self.gesture_played
                .iter()
                .any(|(n, at)| n == name && self.clock - at < GESTURE_COOLDOWN)
        };
        if pool.iter().any(|n| !recent(n)) {
            pool.retain(|n| !recent(n));
        }
        if pool.is_empty() {
            return None;
        }
        let i = self.rng.pick(pool.len());
        Some(pool.swap_remove(i))
    }

    fn choose_frame(&mut self) -> (Option<FrameId>, f64) {
        if let Some(moving) = &self.moving {
            if self.reduce_motion {
                return (Some(moving.clip.steps[0].frame), 0.0);
            }
            let (step, _) = moving.clip.step_at(moving.t);
            return (Some(step.frame), moving.clip.xfade);
        }
        if let Some(playing) = &self.playing {
            let (step, _) = playing.clip.step_at(playing.t);
            return (Some(step.frame), playing.clip.xfade);
        }
        let Some(state) = &self.state else {
            return (None, 0.0);
        };
        let speaking = state.look == Look::Speaking;
        if self.reduce_motion {
            let still = match state.clip.mouth {
                Some(mouth) if speaking => mouth[1],
                _ => state.clip.steps[0].frame,
            };
            return (Some(still), 0.0);
        }
        let xfade = state.clip.xfade;
        if speaking && let Some(mouth) = state.clip.mouth {
            return (Some(self.mouth_frame(mouth)), xfade);
        }
        (Some(state.clip.step_at(state.t).0.frame), xfade)
    }

    /// Lip-sync: closed, half open or open, by the talking rhythm.
    fn mouth_frame(&mut self, mouth: [FrameId; 3]) -> FrameId {
        let env = talk_envelope(self.clock);
        let want = if env >= MOUTH_OPEN {
            2
        } else if env >= MOUTH_HALF {
            1
        } else {
            0
        };
        if want != self.mouth && self.mouth_hold <= 0.0 {
            self.mouth = want;
            self.mouth_hold = MOUTH_HOLD;
        }
        mouth[self.mouth]
    }

    fn show(&mut self, want: Option<FrameId>, dt: f64, xfade: f64) {
        if want != self.shown {
            if self.shown.is_none() || want.is_none() || self.reduce_motion || xfade <= 0.0 {
                self.previous = None;
                self.xfade_left = 0.0;
            } else {
                self.previous = self.shown;
                self.xfade = xfade;
                self.xfade_left = xfade;
            }
            self.shown = want;
        } else if self.xfade_left > 0.0 {
            self.xfade_left = (self.xfade_left - dt).max(0.0);
        }
        if self.xfade_left <= 0.0 {
            self.previous = None;
        }
    }

    fn mix(&self) -> f64 {
        if self.xfade_left <= 0.0 || self.xfade <= 0.0 {
            1.0
        } else {
            1.0 - self.xfade_left / self.xfade
        }
    }

    /// The clip on screen and its clock.
    fn current(&self) -> Option<(&Clip, f64)> {
        match (&self.moving, &self.playing, &self.state) {
            (Some(m), _, _) => Some((&m.clip, m.t)),
            (None, Some(p), _) => Some((&p.clip, p.t)),
            (None, None, Some(s)) => Some((&s.clip, s.t)),
            (None, None, None) => None,
        }
    }

    fn current_clip(&self) -> Option<&Clip> {
        self.current().map(|(clip, _)| clip)
    }

    /// The step on screen, seconds into it.
    fn current_step(&self) -> Option<(&Step, f64)> {
        let (clip, t) = self.current()?;
        Some(clip.step_at(t))
    }

    fn pose(&self) -> Pose {
        if self.reduce_motion {
            return REST;
        }
        let base = if self.asleep || !self.pack.life.sway || self.moving.is_some() {
            REST
        } else {
            mood_pose(self.mood, self.clock)
        };
        let mut pose = match self.current_step() {
            Some((step, inside)) => combine(base, motion_pose(step.motion, inside, step.secs)),
            None => base,
        };
        if self.squash_left > 0.0 {
            let done = self.squash_len - self.squash_left;
            pose = combine(pose, motion_pose(Motion::Squash, done, self.squash_len));
        }
        pose
    }

    /// Every frame she may show soon: the current look's clip and what
    /// follows it, and the one-shot playing. The backend keeps these decoded.
    pub fn wanted(&self, out: &mut Vec<FrameId>) {
        if let Some(moving) = &self.moving {
            self.pack.clip_frames(&moving.clip.name, out);
        }
        if let Some(state) = &self.state {
            self.pack.clip_frames(&state.clip.name, out);
        }
        if let Some(playing) = &self.playing {
            self.pack.clip_frames(&playing.clip.name, out);
        }
    }

    /// Seconds until the picture may change: `1 / FPS` while something
    /// moves, longer while she just sits or sleeps.
    pub fn next_frame_in(&self) -> f64 {
        let fast = 1.0 / FPS;
        if !self.has_frames() {
            return 3600.0;
        }
        if self.moving.is_some() || self.squash_left > 0.0 {
            return fast;
        }
        if self.xfade_left > 0.0 {
            return if self.xfade >= 0.4 {
                1.0 / SLOW_FPS
            } else {
                fast
            };
        }
        let lip_sync = self.mood == Mood::Speaking
            && self.playing.is_none()
            && self.state.as_ref().is_some_and(|s| s.clip.mouth.is_some());
        if !self.reduce_motion && self.mood != Mood::Idle && (self.pack.life.sway || lip_sync) {
            return fast;
        }
        let mut wait: f64 = 60.0;
        if let Some(playing) = &self.playing {
            let (step, _) = playing.clip.step_at(playing.t);
            if step.motion.moves() && !self.reduce_motion {
                return fast;
            }
            wait = wait.min(playing.clip.until_next_step(playing.t));
        } else if let Some(state) = &self.state {
            let (step, _) = state.clip.step_at(state.t);
            if step.motion.moves() && !self.reduce_motion {
                return fast;
            }
            let held = !state.clip.looped && state.t >= state.clip.duration();
            if !(held || self.reduce_motion && self.mood != Mood::Idle) {
                wait = wait.min(state.clip.until_next_step(state.t));
            } else if !state.clip.looped && state.clip.next.is_some() {
                wait = wait.min(fast);
            }
        }
        if self.mood == Mood::Idle && !self.asleep {
            let life = &self.pack.life;
            wait = wait.min(self.next_blink).min(self.next_behaviour);
            if life.sleep_after > 0.0 {
                wait = wait.min(life.sleep_after - self.idle_for);
            }
        } else if self.reduce_motion && lip_sync {
            wait = wait.min(MOUTH_HOLD.max(fast));
        }
        // A hair past the change, so the update lands on the new step.
        (wait + 0.005).max(fast)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn wawi() -> Arc<Pack> {
        Arc::new(Pack::load(&crate::default_assets().join("wawi")).expect("the Wawi pack loads"))
    }

    fn animator() -> Animator {
        Animator::with_seed(wawi(), 7)
    }

    fn name(a: &Animator, id: Option<FrameId>) -> Option<String> {
        id.map(|id| a.pack().frame_name(id).to_owned())
    }

    /// Runs `secs` in 30 fps steps and returns the last scene.
    fn run(a: &mut Animator, secs: f64) -> Scene {
        let mut scene = a.update(0.0);
        let steps = (secs * FPS).round() as usize;
        for _ in 0..steps {
            scene = a.update(1.0 / FPS);
        }
        scene
    }

    fn frames_over(a: &mut Animator, secs: f64) -> HashSet<String> {
        let mut seen = HashSet::new();
        for _ in 0..(secs * FPS) as usize {
            let frame = a.update(1.0 / FPS).frame;
            if let Some(frame) = name(a, frame) {
                seen.insert(frame);
            }
        }
        seen
    }

    #[test]
    fn every_mood_has_its_own_frames() {
        let cases: &[(Mood, &[&str])] = &[
            (Mood::Listening, &["listening", "listening_b"]),
            (Mood::Transcribing, &["tilt"]),
            (Mood::Thinking, &["thinking", "thinking_b"]),
            (Mood::Speaking, &["speaking_a", "speaking_b", "speaking_c"]),
        ];
        for (mood, expected) in cases {
            let mut a = animator();
            a.set_mood(*mood);
            let seen = frames_over(&mut a, 4.0);
            for frame in *expected {
                assert!(
                    seen.contains(*frame),
                    "{mood:?} never showed {frame}: {seen:?}"
                );
            }
            assert!(
                seen.iter().all(|f| expected.contains(&f.as_str())),
                "{mood:?} showed something else: {seen:?}"
            );
        }
    }

    #[test]
    fn idle_breathes_with_the_two_idle_frames_and_falls_asleep() {
        let mut a = animator();
        let seen = frames_over(&mut a, 4.0);
        assert!(
            seen.contains("idle_a") && seen.contains("idle_b"),
            "{seen:?}"
        );
        let nap = a.pack().life.sleep_after + 2.0;
        run(&mut a, nap);
        assert!(a.asleep());
        let seen = frames_over(&mut a, 5.0);
        assert!(
            seen.contains("sleeping") && seen.contains("sleeping_b"),
            "{seen:?}"
        );
    }

    #[test]
    fn the_key_wakes_her_at_once() {
        let mut a = animator();
        let nap = a.pack().life.sleep_after + 3.0;
        run(&mut a, nap);
        assert!(a.asleep());
        a.set_mood(Mood::Listening);
        let scene = a.update(1.0 / FPS);
        assert!(!a.asleep());
        assert_eq!(name(&a, scene.frame).as_deref(), Some("listening_b"));
        assert_eq!(scene.badge, Some(Badge::Listening));
    }

    #[test]
    fn transcribing_shows_the_dots_and_muted_wins_over_everything() {
        let mut a = animator();
        a.set_mood(Mood::Transcribing);
        assert!(matches!(a.update(0.1).badge, Some(Badge::Musing(_))));
        a.set_muted(true);
        assert_eq!(a.update(0.1).badge, Some(Badge::Muted));
        a.set_mood(Mood::Listening);
        assert_eq!(a.update(0.1).badge, Some(Badge::Muted));
        a.set_muted(false);
        a.set_mood(Mood::Thinking);
        assert_eq!(a.update(0.1).badge, None);
    }

    #[test]
    fn flashes_play_over_the_mood_and_then_give_it_back() {
        let mut a = animator();
        a.set_mood(Mood::Speaking);
        a.flash(Flash::Interrupted);
        a.set_mood(Mood::Listening);
        let frame = a.update(0.05).frame;
        assert_eq!(name(&a, frame).as_deref(), Some("boop"));
        let scene = run(&mut a, 1.0);
        assert!(name(&a, scene.frame).is_some_and(|f| f.starts_with("listening")));

        a.set_mood(Mood::Idle);
        a.flash(Flash::Error);
        let frame = a.update(0.05).frame;
        assert_eq!(name(&a, frame).as_deref(), Some("shrug"));
    }

    #[test]
    fn a_click_hops_and_wakes_her() {
        let mut a = animator();
        a.poke();
        let mut lifted = false;
        for _ in 0..30 {
            lifted |= a.update(1.0 / FPS).pose.dy > 5.0;
        }
        assert!(lifted, "the hop lifts her");
        let nap = a.pack().life.sleep_after + 3.0;
        run(&mut a, nap);
        assert!(a.asleep());
        a.poke();
        assert!(!a.asleep());
        let frame = a.update(0.05).frame;
        assert_eq!(name(&a, frame).as_deref(), Some("stretch"));
    }

    #[test]
    fn actions_play_by_name_over_the_mood_and_end() {
        let mut a = animator();
        assert!(a.pack().actions().contains(&"cafe"));
        a.set_mood(Mood::Thinking);
        assert!(a.play_action("cafe"));
        let frame = a.update(0.05).frame;
        assert_eq!(name(&a, frame).as_deref(), Some("coffee"));
        // A mood change does not cut an action (it does cut a gesture).
        a.set_mood(Mood::Speaking);
        let frame = a.update(0.05).frame;
        assert_eq!(name(&a, frame).as_deref(), Some("coffee"));
        let scene = run(&mut a, 4.0);
        assert!(name(&a, scene.frame).is_some_and(|f| f.starts_with("speaking")));
        assert!(!a.play_action("volar"));
        // A reaction wins over an action.
        a.flash(Flash::Error);
        assert!(!a.play_action("cafe"));
    }

    #[test]
    fn a_minimal_pack_with_only_idle_does_everything() {
        let dir = std::env::temp_dir().join(format!("wawi-anim-min-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("cuadros")).unwrap();
        std::fs::write(
            dir.join("cuadros/solo.png"),
            crate::pack::png::encode_rgba(4, 4, &[255; 64]),
        )
        .unwrap();
        let pack = Pack::from_text(
            &dir,
            "id = \"min\"\nnombre = \"Min\"\n[animaciones.idle]\ncuadros = [\"solo\"]\n",
        )
        .unwrap();
        let mut a = Animator::with_seed(Arc::new(pack), 1);
        for mood in [
            Mood::Listening,
            Mood::Transcribing,
            Mood::Thinking,
            Mood::Speaking,
            Mood::Idle,
        ] {
            a.set_mood(mood);
            let frame = a.update(0.1).frame;
            assert_eq!(name(&a, frame).as_deref(), Some("solo"), "{mood:?}");
        }
        a.flash(Flash::Error);
        a.poke();
        a.set_muted(true);
        let frame = run(&mut a, 30.0).frame;
        assert_eq!(name(&a, frame).as_deref(), Some("solo"));
        assert!(a.asleep(), "asleep, still with the idle frame");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn she_only_asks_for_redraws_when_something_moves() {
        let mut a = animator();
        a.update(0.0);
        a.update(0.5);
        let idle = a.next_frame_in();
        assert!(idle > 0.1, "sitting still: {idle}");
        a.set_mood(Mood::Speaking);
        a.update(0.1);
        assert!((a.next_frame_in() - 1.0 / FPS).abs() < 1e-9);
        a.set_mood(Mood::Idle);
        let nap = a.pack().life.sleep_after + 5.0;
        run(&mut a, nap);
        assert!(a.asleep());
        // Asleep, the redraws over a minute are a fraction of 30 fps.
        let mut t = 0.0;
        let mut redraws = 0;
        while t < 60.0 {
            let wait = a.next_frame_in();
            a.update(wait);
            t += wait;
            redraws += 1;
        }
        assert!(redraws < 60 * 10, "{redraws} redraws in a minute asleep");
    }

    #[test]
    fn crossfades_never_leave_her_see_through() {
        for i in 0..=10 {
            let (old, new) = crossfade_alphas(f64::from(i) / 10.0);
            assert!(old.max(new) >= 1.0 - 1e-9);
        }
        assert_eq!(crossfade_alphas(0.0), (1.0, 0.0));
        assert_eq!(crossfade_alphas(1.0), (0.0, 1.0));
    }

    #[test]
    fn reduce_motion_holds_her_still() {
        let mut a = animator();
        a.set_reduce_motion(true);
        a.set_mood(Mood::Speaking);
        let scene = a.update(0.3);
        assert_eq!(scene.pose, REST);
        assert_eq!(name(&a, scene.frame).as_deref(), Some("speaking_c"));
    }

    #[test]
    fn what_she_wants_decoded_follows_the_look() {
        let mut a = animator();
        let mut want = Vec::new();
        a.wanted(&mut want);
        let names: HashSet<String> = want
            .iter()
            .map(|id| a.pack().frame_name(*id).to_owned())
            .collect();
        assert!(
            names.contains("idle_a") && names.contains("idle_b"),
            "{names:?}"
        );
        a.set_mood(Mood::Listening);
        a.update(0.01);
        want.clear();
        a.wanted(&mut want);
        let names: HashSet<String> = want
            .iter()
            .map(|id| a.pack().frame_name(*id).to_owned())
            .collect();
        assert!(
            names.contains("listening") && names.contains("listening_b"),
            "{names:?}"
        );
    }

    #[test]
    fn the_body_clip_wins_and_turns_her_round() {
        let mut a = animator();
        a.set_mood(Mood::Listening);
        assert!(a.set_locomotion(Some("fall")));
        let scene = a.update(1.0 / FPS);
        assert_eq!(name(&a, scene.frame).as_deref(), Some("fall"));
        assert!(a.next_frame_in() <= 1.0 / FPS + 1e-9);
        // Wawi is drawn facing left: walking right, she is mirrored.
        assert!(!scene.mirror);
        a.set_facing(true);
        assert!(a.update(0.0).mirror);
        // The parachute brings its canopy along.
        a.set_locomotion(Some("chute"));
        let scene = a.update(1.0 / FPS);
        let canopy = scene.layer.map(|l| a.pack().frame_name(l.frame).to_owned());
        assert_eq!(canopy.as_deref(), Some("prop_chute"));
        assert!(!a.set_locomotion(Some("no_such_clip")));
        a.set_locomotion(None);
        let scene = a.update(1.0 / FPS);
        assert!(name(&a, scene.frame).is_some_and(|f| f.starts_with("listening")));
        assert_eq!(scene.layer, None);
    }

    #[test]
    fn on_a_window_she_sits_on_the_edge() {
        let mut a = animator();
        a.set_perched(true);
        let scene = run(&mut a, 0.2);
        assert_eq!(name(&a, scene.frame).as_deref(), Some("sit_edge"));
        a.set_perched(false);
        let scene = run(&mut a, 1.0);
        assert!(name(&a, scene.frame).is_some_and(|f| f.starts_with("idle")));
    }

    #[test]
    fn a_landing_without_a_clip_squashes_by_code() {
        let mut a = animator();
        a.squash(0.3);
        let scene = run(&mut a, 0.15);
        assert!(
            scene.pose.sy < 0.95 && scene.pose.sx > 1.03,
            "{:?}",
            scene.pose
        );
        let scene = run(&mut a, 0.3);
        assert_eq!(scene.pose, REST);
    }

    #[test]
    fn gestures_never_loop() {
        use crate::pack::png;
        let dir = std::env::temp_dir().join(format!("wawi-anim-gestures-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("cuadros")).unwrap();
        let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
        let mut toml = String::from(
            "id = \"g\"\n[vida]\ndormir_tras = 0\ncada_gesto = [8, 8]\ncada_parpadeo = [100, 100]\ngestos = [",
        );
        toml.push_str(
            &names
                .iter()
                .map(|n| format!("\"{n}\""))
                .collect::<Vec<_>>()
                .join(", "),
        );
        toml.push_str("]\n[animaciones.idle]\ncuadros = [\"idle\"]\n");
        for n in ["idle"].iter().chain(names.iter()) {
            std::fs::write(
                dir.join("cuadros").join(format!("{n}.png")),
                png::encode_rgba(2, 2, &[255; 16]),
            )
            .unwrap();
            if *n != "idle" {
                toml.push_str(&format!(
                    "[animaciones.{n}]\ncuadros = [\"{n}\"]\nduraciones = [1]\nrepetir = false\n"
                ));
            }
        }
        let pack = Arc::new(Pack::from_text(&dir, &toml).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let mut a = Animator::with_seed(pack, 11);
        let mut played: Vec<(String, f64)> = Vec::new();
        for i in 0..(20 * 60 * 10) {
            a.update(0.1);
            if let Some(p) = &a.playing
                && p.t < 1e-9
            {
                played.push((p.clip.name.clone(), f64::from(i) * 0.1));
            }
        }
        assert!(played.len() > 100, "{}", played.len());
        for (i, (name, at)) in played.iter().enumerate() {
            for (other, then) in &played[..i] {
                assert!(
                    other != name || at - then >= GESTURE_COOLDOWN,
                    "{name} again after {}s",
                    at - then
                );
            }
        }
    }
}
