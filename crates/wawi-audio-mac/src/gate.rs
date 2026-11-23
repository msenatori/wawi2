//! The push-to-talk gate: pure logic, no audio devices or threads.
//!
//! Key down opens a segment and asks for `SpeechStarted` (which also
//! interrupts Wawi: the core stops playback). The segment starts with the last
//! [`GateConfig::pre_roll`] of mic audio from before the press, because people
//! start talking as they press. Key up closes it after a tail, and the host
//! hands the audio to the STT and then sends `SpeechEnded`.
//!
//! Voice processing pushes the room's silence down to -100 dBFS or less, which
//! is exact zeros once written as 16-bit. A long run of that before the voice
//! made Parakeet drop or garble the first word, so a closing segment loses the
//! silence at both ends, keeping [`GateConfig::silence_margin`] of it.
//!
//! A tap shorter than [`GateConfig::min_hold`] still closes its segment, with
//! no audio. The core expects every `SpeechStarted` to be followed by a
//! `SpeechEnded`, and the STT answers an empty segment with empty text, which
//! the core drops (`EmptyTranscript`). So a tap interrupts Wawi and says
//! nothing, without leaving a segment open until the listen timeout.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateConfig {
    /// Mic audio from just before the key went down that each segment starts
    /// with. The gate keeps this much of the stream at all times.
    pub pre_roll: Duration,
    /// Holds shorter than this are taps: they interrupt, but carry no audio.
    pub min_hold: Duration,
    /// A segment closes by itself after this much audio, even if the key is
    /// still down. The key has to go up before the next segment can start.
    pub max_segment: Duration,
    /// Audio still taken after the key goes up. People let go while the last
    /// syllable still sounds, and the mic tap delivers audio a few tens of
    /// milliseconds late; with 80 ms the end of the phrase was cut.
    pub release_tail: Duration,
    /// Silence (under [`SILENCE_DBFS`]) kept before the first and after the
    /// last sound of a segment; the rest of it is cut. Zero keeps it all.
    pub silence_margin: Duration,
    /// If the tail never arrives (the mic stopped), the segment closes this
    /// long after the key went up, with what it has. Keep it well above
    /// `release_tail`.
    pub tail_timeout: Duration,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            pre_roll: Duration::from_millis(400),
            min_hold: Duration::from_millis(250),
            max_segment: Duration::from_secs(60),
            release_tail: Duration::from_millis(300),
            silence_margin: Duration::from_millis(200),
            tail_timeout: Duration::from_millis(900),
        }
    }
}

/// Peak level under which a stretch of a segment counts as silence. Voice
/// processing leaves the quiet room at -100 dBFS or lower; even soft speech is
/// far above this.
pub const SILENCE_DBFS: f64 = -80.0;
/// Length of the stretches [`SILENCE_DBFS`] is measured on.
const SILENCE_WINDOW: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    Released,
    /// A tap: shorter than `min_hold`. The segment has no audio.
    TooShort,
    /// Reached `max_segment` with the key still down.
    Capped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    /// Mono, at `sample_rate`: the pre-roll, the hold and the tail. Empty for
    /// [`EndReason::TooShort`].
    pub samples: Vec<f32>,
    /// How many of `samples` are pre-roll, from before the key went down.
    pub pre_roll: usize,
    pub sample_rate: u32,
    /// How long the key was down (up to the cap, for a capped segment).
    pub held: Duration,
    pub reason: EndReason,
}

impl Segment {
    pub fn seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.samples.len() as f64 / f64::from(self.sample_rate)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum GateEvent {
    /// Send `SpeechStarted`.
    Started,
    /// Hand `samples` to the STT, then send `SpeechEnded`.
    Ended(Segment),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Held {
        since: Instant,
    },
    Closing {
        held: Duration,
        released: Instant,
        tail_left: usize,
    },
    /// Closed by the cap; waits for the key to go up.
    Capped,
}

#[derive(Debug)]
pub struct PttGate {
    config: GateConfig,
    sample_rate: u32,
    max_samples: usize,
    tail_samples: usize,
    pre_roll_samples: usize,
    margin_samples: usize,
    window_samples: usize,
    phase: Phase,
    buffer: Vec<f32>,
    pre_roll: usize,
    /// The latest mic audio while no segment is open, at most
    /// `pre_roll_samples` long.
    history: VecDeque<f32>,
}

impl PttGate {
    pub fn new(sample_rate: u32, config: GateConfig) -> Self {
        let samples = |d: Duration| (d.as_secs_f64() * f64::from(sample_rate)).round() as usize;
        Self {
            config,
            sample_rate,
            max_samples: samples(config.max_segment).max(1),
            tail_samples: samples(config.release_tail),
            pre_roll_samples: samples(config.pre_roll),
            margin_samples: samples(config.silence_margin),
            window_samples: samples(SILENCE_WINDOW).max(1),
            phase: Phase::Idle,
            buffer: Vec::new(),
            pre_roll: 0,
            history: VecDeque::new(),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// True from key down until the segment closes.
    pub fn is_open(&self) -> bool {
        matches!(self.phase, Phase::Held { .. } | Phase::Closing { .. })
    }

    pub fn press(&mut self, now: Instant) -> Vec<GateEvent> {
        let mut out = Vec::new();
        match self.phase {
            // Key repeat, or still down after the cap.
            Phase::Held { .. } | Phase::Capped => return out,
            // Pressed again during the tail: the old segment closes first, and
            // took the audio the new one would have used as pre-roll.
            Phase::Closing { held, .. } => out.push(self.close(held, EndReason::Released)),
            Phase::Idle => {}
        }
        self.buffer.clear();
        // Never a full cap of pre-roll: the segment would close at once.
        let lead = self.history.len().min(self.max_samples - 1);
        let old = self.history.len() - lead;
        self.buffer.extend(self.history.drain(..).skip(old));
        self.pre_roll = lead;
        self.phase = Phase::Held { since: now };
        out.push(GateEvent::Started);
        out
    }

    pub fn release(&mut self, now: Instant) -> Vec<GateEvent> {
        match self.phase {
            Phase::Held { since } => {
                let held = now.saturating_duration_since(since);
                if held < self.config.min_hold {
                    self.buffer.clear();
                    return vec![self.close(held, EndReason::TooShort)];
                }
                if self.tail_samples == 0 {
                    return vec![self.close(held, EndReason::Released)];
                }
                self.phase = Phase::Closing {
                    held,
                    released: now,
                    tail_left: self.tail_samples,
                };
                Vec::new()
            }
            Phase::Capped => {
                self.phase = Phase::Idle;
                Vec::new()
            }
            Phase::Idle | Phase::Closing { .. } => Vec::new(),
        }
    }

    /// Mono mic audio, in arrival order: the whole stream, not only while the
    /// key is down. Outside a segment only the pre-roll window is kept.
    pub fn push(&mut self, samples: &[f32], now: Instant) -> Vec<GateEvent> {
        match self.phase {
            Phase::Held { since } => {
                let room = self.max_samples - self.buffer.len();
                self.buffer
                    .extend_from_slice(&samples[..samples.len().min(room)]);
                if self.buffer.len() >= self.max_samples {
                    let held = now
                        .saturating_duration_since(since)
                        .min(self.config.max_segment);
                    let event = self.close(held, EndReason::Capped);
                    self.phase = Phase::Capped;
                    return vec![event];
                }
                Vec::new()
            }
            Phase::Closing {
                held,
                released,
                tail_left,
            } => {
                let room = self.max_samples - self.buffer.len();
                let take = samples.len().min(tail_left).min(room);
                self.buffer.extend_from_slice(&samples[..take]);
                let tail_left = tail_left - take;
                if tail_left == 0 || self.buffer.len() >= self.max_samples {
                    return vec![self.close(held, EndReason::Released)];
                }
                self.phase = Phase::Closing {
                    held,
                    released,
                    tail_left,
                };
                Vec::new()
            }
            Phase::Idle | Phase::Capped => {
                self.remember(samples);
                Vec::new()
            }
        }
    }

    fn remember(&mut self, samples: &[f32]) {
        let keep = self.pre_roll_samples;
        if keep == 0 {
            return;
        }
        let recent = &samples[samples.len().saturating_sub(keep)..];
        let overflow = (self.history.len() + recent.len()).saturating_sub(keep);
        self.history.drain(..overflow);
        self.history.extend(recent);
    }

    /// The range to keep: from `margin` before the first window with sound to
    /// `margin` after the last one. All of it if nothing has sound.
    fn voiced(&self, samples: &[f32]) -> (usize, usize) {
        let floor = 10f32.powf(SILENCE_DBFS as f32 / 20.0);
        let loud = |w: &[f32]| w.iter().any(|s| s.abs() > floor);
        let w = self.window_samples;
        let Some(first) = samples.chunks(w).position(loud) else {
            return (0, samples.len());
        };
        let last = samples.chunks(w).rposition(loud).unwrap_or(first);
        let start = (first * w).saturating_sub(self.margin_samples);
        let end = ((last + 1) * w + self.margin_samples).min(samples.len());
        (start, end)
    }

    /// Closes a tail that never arrived. Call it now and then.
    pub fn tick(&mut self, now: Instant) -> Vec<GateEvent> {
        match self.phase {
            Phase::Closing { held, released, .. }
                if now.saturating_duration_since(released) >= self.config.tail_timeout =>
            {
                vec![self.close(held, EndReason::Released)]
            }
            _ => Vec::new(),
        }
    }

    fn close(&mut self, held: Duration, reason: EndReason) -> GateEvent {
        self.phase = Phase::Idle;
        let mut pre_roll = std::mem::take(&mut self.pre_roll);
        let mut samples = std::mem::take(&mut self.buffer);
        if self.config.silence_margin > Duration::ZERO {
            let (start, end) = self.voiced(&samples);
            samples.truncate(end);
            samples.drain(..start);
            pre_roll = pre_roll.saturating_sub(start);
        }
        GateEvent::Ended(Segment {
            pre_roll: pre_roll.min(samples.len()),
            samples,
            sample_rate: self.sample_rate,
            held,
            reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 1000;

    fn config() -> GateConfig {
        GateConfig {
            pre_roll: Duration::ZERO,
            silence_margin: Duration::ZERO,
            min_hold: Duration::from_millis(250),
            max_segment: Duration::from_secs(2),
            release_tail: Duration::from_millis(50),
            tail_timeout: Duration::from_millis(300),
        }
    }

    fn with_pre_roll(ms: u64) -> GateConfig {
        GateConfig {
            pre_roll: Duration::from_millis(ms),
            ..config()
        }
    }

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    fn ended(events: Vec<GateEvent>) -> Segment {
        match events.as_slice() {
            [GateEvent::Ended(segment)] => segment.clone(),
            other => panic!("expected one Ended, got {other:?}"),
        }
    }

    #[test]
    fn without_pre_roll_audio_before_press_is_dropped() {
        let t = Instant::now();
        let mut gate = PttGate::new(RATE, config());
        assert!(gate.push(&[1.0; 100], t).is_empty());
        assert_eq!(gate.press(ms(t, 0)), vec![GateEvent::Started]);
        gate.push(&[0.5; 400], ms(t, 400));
        assert!(gate.release(ms(t, 400)).is_empty(), "waits for the tail");
        let segment = ended(gate.push(&[0.25; 100], ms(t, 500)));
        assert_eq!(segment.reason, EndReason::Released);
        assert_eq!(segment.held, Duration::from_millis(400));
        // 400 held + 50 of tail; the rest of the last chunk is not taken.
        assert_eq!(segment.samples.len(), 450);
        assert!(segment.samples[..400].iter().all(|&s| s == 0.5));
        assert!(segment.samples[400..].iter().all(|&s| s == 0.25));
        assert!(!gate.is_open());
    }

    #[test]
    fn a_tap_interrupts_but_carries_no_audio() {
        let t = Instant::now();
        let mut gate = PttGate::new(RATE, config());
        assert_eq!(gate.press(t), vec![GateEvent::Started]);
        gate.push(&[0.5; 100], ms(t, 100));
        let segment = ended(gate.release(ms(t, 120)));
        assert_eq!(segment.reason, EndReason::TooShort);
        assert!(segment.samples.is_empty());
        assert!(gate.push(&[0.5; 100], ms(t, 200)).is_empty());
    }

    #[test]
    fn key_repeat_does_not_restart() {
        let t = Instant::now();
        let mut gate = PttGate::new(RATE, config());
        gate.press(t);
        gate.push(&[0.5; 300], ms(t, 300));
        assert!(gate.press(ms(t, 300)).is_empty());
        gate.release(ms(t, 400));
        let segment = ended(gate.tick(ms(t, 800)));
        assert_eq!(segment.samples.len(), 300);
    }

    #[test]
    fn the_cap_closes_and_waits_for_release() {
        let t = Instant::now();
        let mut gate = PttGate::new(RATE, config());
        gate.press(t);
        assert!(gate.push(&[0.5; 1500], ms(t, 1500)).is_empty());
        let segment = ended(gate.push(&[0.5; 1500], ms(t, 3000)));
        assert_eq!(segment.reason, EndReason::Capped);
        assert_eq!(segment.samples.len(), 2000);
        assert_eq!(segment.held, Duration::from_secs(2));
        // Still down: nothing more is kept, and a repeat does not reopen.
        assert!(gate.push(&[0.5; 100], ms(t, 3100)).is_empty());
        assert!(gate.press(ms(t, 3100)).is_empty());
        assert!(gate.release(ms(t, 3200)).is_empty());
        assert_eq!(gate.press(ms(t, 3300)), vec![GateEvent::Started]);
    }

    #[test]
    fn a_missing_tail_times_out() {
        let t = Instant::now();
        let mut gate = PttGate::new(RATE, config());
        gate.press(t);
        gate.push(&[0.5; 500], ms(t, 500));
        gate.release(ms(t, 500));
        gate.push(&[0.5; 10], ms(t, 510));
        assert!(gate.tick(ms(t, 700)).is_empty());
        let segment = ended(gate.tick(ms(t, 800)));
        assert_eq!(segment.samples.len(), 510);
        assert!(gate.tick(ms(t, 900)).is_empty());
    }

    #[test]
    fn pressing_during_the_tail_closes_the_old_segment_first() {
        let t = Instant::now();
        let mut gate = PttGate::new(RATE, config());
        gate.press(t);
        gate.push(&[0.5; 300], ms(t, 300));
        gate.release(ms(t, 300));
        let events = gate.press(ms(t, 310));
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], GateEvent::Ended(s) if s.samples.len() == 300));
        assert_eq!(events[1], GateEvent::Started);
        gate.push(&[0.75; 300], ms(t, 610));
        gate.release(ms(t, 610));
        let segment = ended(gate.push(&[0.75; 50], ms(t, 660)));
        assert_eq!(segment.samples.len(), 350, "the new segment starts empty");
    }

    #[test]
    fn release_without_press_does_nothing() {
        let t = Instant::now();
        let mut gate = PttGate::new(RATE, config());
        assert!(gate.release(t).is_empty());
        assert!(gate.tick(ms(t, 1000)).is_empty());
    }

    #[test]
    fn a_segment_starts_with_the_audio_just_before_the_press() {
        let t = Instant::now();
        let mut gate = PttGate::new(RATE, with_pre_roll(100));
        gate.push(&[1.0; 300], t);
        gate.push(&[0.75; 60], ms(t, 360));
        gate.press(ms(t, 360));
        gate.push(&[0.5; 400], ms(t, 760));
        gate.release(ms(t, 760));
        let segment = ended(gate.push(&[0.25; 50], ms(t, 810)));
        assert_eq!(segment.pre_roll, 100);
        assert_eq!(segment.samples.len(), 100 + 400 + 50);
        // The last 100 before the press, in order.
        assert!(segment.samples[..40].iter().all(|&s| s == 1.0));
        assert!(segment.samples[40..100].iter().all(|&s| s == 0.75));
        assert!(segment.samples[100..500].iter().all(|&s| s == 0.5));
        assert_eq!(segment.held, Duration::from_millis(400));
    }

    #[test]
    fn pre_roll_is_shorter_when_the_stream_just_started() {
        let t = Instant::now();
        let mut gate = PttGate::new(RATE, with_pre_roll(400));
        gate.push(&[1.0; 30], t);
        gate.press(ms(t, 30));
        gate.push(&[0.5; 300], ms(t, 330));
        let segment = ended(
            gate.release(ms(t, 330))
                .into_iter()
                .chain(gate.tick(ms(t, 700)))
                .collect(),
        );
        assert_eq!(segment.pre_roll, 30);
        assert_eq!(segment.samples.len(), 330);
    }

    #[test]
    fn each_segment_gets_fresh_pre_roll() {
        let t = Instant::now();
        let mut gate = PttGate::new(RATE, with_pre_roll(100));
        gate.push(&[1.0; 100], t);
        gate.press(ms(t, 100));
        gate.push(&[0.5; 300], ms(t, 400));
        gate.release(ms(t, 400));
        ended(gate.push(&[0.5; 50], ms(t, 450)));
        // Only 20 samples arrive between the close and the next press.
        gate.push(&[0.25; 20], ms(t, 470));
        gate.press(ms(t, 470));
        gate.push(&[0.75; 300], ms(t, 770));
        gate.release(ms(t, 770));
        let segment = ended(gate.push(&[0.75; 50], ms(t, 820)));
        assert_eq!(segment.pre_roll, 20);
        assert!(segment.samples[..20].iter().all(|&s| s == 0.25));
        assert_eq!(segment.samples.len(), 20 + 300 + 50);
    }

    #[test]
    fn a_tap_carries_no_audio_even_with_pre_roll() {
        let t = Instant::now();
        let mut gate = PttGate::new(RATE, with_pre_roll(100));
        gate.push(&[1.0; 200], t);
        gate.press(ms(t, 200));
        let segment = ended(gate.release(ms(t, 300)));
        assert_eq!(segment.reason, EndReason::TooShort);
        assert!(segment.samples.is_empty());
        assert_eq!(segment.pre_roll, 0);
    }

    #[test]
    fn pressing_during_the_tail_gives_the_new_segment_no_pre_roll() {
        let t = Instant::now();
        let mut gate = PttGate::new(RATE, with_pre_roll(100));
        gate.push(&[1.0; 100], t);
        gate.press(ms(t, 100));
        gate.push(&[0.5; 300], ms(t, 400));
        gate.release(ms(t, 400));
        gate.push(&[0.5; 20], ms(t, 420));
        let events = gate.press(ms(t, 420));
        assert!(matches!(&events[0], GateEvent::Ended(s) if s.samples.len() == 420));
        gate.push(&[0.75; 300], ms(t, 720));
        gate.release(ms(t, 720));
        let segment = ended(gate.push(&[0.75; 50], ms(t, 770)));
        assert_eq!(segment.pre_roll, 0);
        assert_eq!(segment.samples.len(), 350);
    }

    #[test]
    fn default_keeps_400_ms_before_and_300_ms_after() {
        let t = Instant::now();
        let mut gate = PttGate::new(RATE, GateConfig::default());
        gate.push(&[0.1; 1000], t);
        gate.press(ms(t, 1000));
        gate.push(&[0.5; 1000], ms(t, 2000));
        gate.release(ms(t, 2000));
        assert!(gate.push(&[0.25; 200], ms(t, 2200)).is_empty());
        let segment = ended(gate.push(&[0.25; 200], ms(t, 2400)));
        assert_eq!(segment.pre_roll, 400);
        assert_eq!(segment.samples.len(), 400 + 1000 + 300);
    }

    #[test]
    fn silence_is_cut_down_to_the_margin_at_both_ends() {
        let t = Instant::now();
        let config = GateConfig {
            silence_margin: Duration::from_millis(50),
            ..with_pre_roll(100)
        };
        let mut gate = PttGate::new(RATE, config);
        gate.push(&[0.0; 100], t);
        gate.press(ms(t, 100));
        // 300 ms of suppressed room (-120 dBFS), then the voice, then silence.
        gate.push(&[1e-6; 300], ms(t, 400));
        gate.push(&[0.5; 200], ms(t, 600));
        gate.push(&[0.0; 100], ms(t, 700));
        gate.release(ms(t, 700));
        let segment = ended(gate.push(&[0.0; 50], ms(t, 750)));
        // 50 of margin, the voice, 50 of margin; the pre-roll was all cut.
        assert_eq!(segment.samples.len(), 50 + 200 + 50);
        assert!(segment.samples[50..250].iter().all(|&s| s == 0.5));
        assert_eq!(segment.pre_roll, 0);
    }

    #[test]
    fn voice_in_the_pre_roll_survives_the_cut() {
        let t = Instant::now();
        let config = GateConfig {
            silence_margin: Duration::from_millis(50),
            ..with_pre_roll(200)
        };
        let mut gate = PttGate::new(RATE, config);
        gate.push(&[0.0; 120], t);
        gate.push(&[0.5; 80], ms(t, 200));
        gate.press(ms(t, 200));
        gate.push(&[0.5; 300], ms(t, 500));
        gate.release(ms(t, 500));
        let segment = ended(gate.push(&[0.0; 50], ms(t, 550)));
        assert_eq!(segment.pre_roll, 50 + 80);
        assert_eq!(segment.samples.len(), 50 + 80 + 300 + 50);
    }

    #[test]
    fn an_all_silent_segment_is_kept_whole() {
        let t = Instant::now();
        let config = GateConfig {
            silence_margin: Duration::from_millis(50),
            ..config()
        };
        let mut gate = PttGate::new(RATE, config);
        gate.press(t);
        gate.push(&[0.0; 400], ms(t, 400));
        gate.release(ms(t, 400));
        let segment = ended(gate.push(&[0.0; 50], ms(t, 450)));
        assert_eq!(segment.samples.len(), 450);
    }
}
