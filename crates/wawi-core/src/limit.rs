//! The usage limit ("se quedó sin cupo"): what Wawi does while the owner's
//! Claude subscription refuses requests until a reset. Pure: time is the
//! dialogue's clock (`now_ms`), and the reset comes in as "in this many
//! milliseconds" from the brain adapter, which read it from the CLI.
//!
//! The rules:
//!
//! - **A hit** (`hit`) starts the limit. With a known reset, it lasts until
//!   the reset plus a small margin. Without one, it backs off: 15 minutes,
//!   then twice as long each time it is hit again, up to 2 hours.
//! - **While limited**, owner turns get a short local line instead of the
//!   brain (the first one hears the whole thing once; later ones a shorter
//!   one). With no known reset, an owner turn may still try the brain every
//!   few minutes, since the limit may already be over.
//! - **Background work** (initiative, instincts) waits: the dialogue
//!   postpones it until the limit ends.
//! - **The end** comes on a tick. If the owner was told, the host is asked
//!   to say she is back (through the instinct gate, never on top of the
//!   owner), and that turn is the first real try of the brain: if it is
//!   still limited, the limit starts again, silently.

const MINUTE_MS: u64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitConfig {
    /// The first wait when the reset time is unknown. It doubles each time
    /// the limit is hit again, up to `max_backoff_ms`.
    pub first_backoff_ms: u64,
    pub max_backoff_ms: u64,
    /// Added to a known reset, so a clock a bit off does not try too soon.
    pub reset_margin_ms: u64,
    /// With no known reset, how often an owner turn may try the brain anyway.
    pub owner_retry_ms: u64,
}

impl Default for LimitConfig {
    fn default() -> Self {
        Self {
            first_backoff_ms: 15 * MINUTE_MS,
            max_backoff_ms: 120 * MINUTE_MS,
            reset_margin_ms: MINUTE_MS,
            owner_retry_ms: 5 * MINUTE_MS,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Limited {
    /// When to try again.
    until_ms: u64,
    /// The reset came from the CLI (otherwise it is a backoff guess).
    known: bool,
    /// How the host said the reset in words ("las 18:00"), or empty.
    label: String,
    /// The last owner turn that went to the brain anyway (unknown reset).
    owner_tried_ms: u64,
}

/// What the dialogue should tell the owner about the limit right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LimitLine {
    /// The first time: «Me quedé sin cupo hasta las 18:00…».
    Hit { until: String },
    /// Later owner turns: «Sigo sin cupo hasta las 18:00…».
    Still { until: String },
}

/// The limit ended on this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitEnded {
    /// The owner heard about it, so Wawi should say she is back.
    pub told_owner: bool,
}

#[derive(Clone, Debug)]
pub struct UsageLimit {
    config: LimitConfig,
    state: Option<Limited>,
    /// The owner heard the limit line during this limit.
    told: bool,
    /// The next unknown-reset wait.
    backoff_ms: u64,
}

impl UsageLimit {
    pub fn new(config: LimitConfig) -> Self {
        Self {
            backoff_ms: config.first_backoff_ms,
            config,
            state: None,
            told: false,
        }
    }

    pub fn is_limited(&self) -> bool {
        self.state.is_some()
    }

    /// How long until the next try, while limited.
    pub fn left_ms(&self, now_ms: u64) -> Option<u64> {
        self.state
            .as_ref()
            .map(|limited| limited.until_ms.saturating_sub(now_ms))
    }

    /// Whether the reset time is known (from the CLI), while limited.
    pub fn known(&self) -> Option<bool> {
        self.state.as_ref().map(|limited| limited.known)
    }

    /// The brain was refused for the usage limit. `resets_in_ms` is when
    /// the CLI said it resets, if it did; `label` is that time in words.
    pub fn hit(&mut self, now_ms: u64, resets_in_ms: Option<u64>, label: &str) {
        let (until_ms, known) = match resets_in_ms {
            Some(resets_in) => {
                self.backoff_ms = self.config.first_backoff_ms;
                (now_ms + resets_in + self.config.reset_margin_ms, true)
            }
            None => {
                let wait = self.backoff_ms;
                self.backoff_ms = (wait * 2).min(self.config.max_backoff_ms);
                (now_ms + wait, false)
            }
        };
        self.state = Some(Limited {
            until_ms,
            known,
            label: if known {
                label.trim().to_owned()
            } else {
                String::new()
            },
            owner_tried_ms: now_ms,
        });
    }

    /// An owner turn arrived while limited: whether it may try the brain
    /// anyway (only with an unknown reset, every `owner_retry_ms`). Trying
    /// counts as the try.
    pub fn owner_may_try(&mut self, now_ms: u64) -> bool {
        let retry = self.config.owner_retry_ms;
        match self.state.as_mut() {
            Some(limited)
                if !limited.known && now_ms.saturating_sub(limited.owner_tried_ms) >= retry =>
            {
                limited.owner_tried_ms = now_ms;
                true
            }
            Some(_) => false,
            None => true,
        }
    }

    /// The line for the owner now, and from now on the owner counts as told.
    pub fn line(&mut self) -> Option<LimitLine> {
        let until = self.state.as_ref()?.label.clone();
        let line = if self.told {
            LimitLine::Still { until }
        } else {
            LimitLine::Hit { until }
        };
        self.told = true;
        Some(line)
    }

    /// Call on every tick: the limit ends once its time comes.
    pub fn tick(&mut self, now_ms: u64) -> Option<LimitEnded> {
        let limited = self.state.as_ref()?;
        if now_ms < limited.until_ms {
            return None;
        }
        self.state = None;
        let told_owner = std::mem::take(&mut self.told);
        Some(LimitEnded { told_owner })
    }

    /// A brain turn worked: the backoff starts over.
    pub fn brain_worked(&mut self) {
        if self.state.is_none() {
            self.backoff_ms = self.config.first_backoff_ms;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = MINUTE_MS;

    fn limit() -> UsageLimit {
        UsageLimit::new(LimitConfig::default())
    }

    #[test]
    fn a_known_reset_lasts_until_it_plus_the_margin() {
        let mut l = limit();
        l.hit(0, Some(90 * MIN), "las 18:00");
        assert!(l.is_limited());
        assert_eq!(l.known(), Some(true));
        assert_eq!(l.left_ms(30 * MIN), Some(61 * MIN));
        assert_eq!(l.tick(90 * MIN), None);
        assert_eq!(l.tick(91 * MIN), Some(LimitEnded { told_owner: false }));
        assert!(!l.is_limited());
        assert_eq!(l.tick(92 * MIN), None);
    }

    #[test]
    fn an_unknown_reset_backs_off_doubling_up_to_two_hours() {
        let mut l = limit();
        let mut now = 0;
        let mut waits = Vec::new();
        for _ in 0..6 {
            l.hit(now, None, "ignored");
            let wait = l.left_ms(now).unwrap();
            waits.push(wait / MIN);
            now += wait;
            assert!(l.tick(now).is_some());
        }
        assert_eq!(waits, vec![15, 30, 60, 120, 120, 120]);
        // A turn that works starts the backoff over; a known reset too.
        l.brain_worked();
        l.hit(now, None, "");
        assert_eq!(l.left_ms(now), Some(15 * MIN));
        l.tick(now + 15 * MIN);
        l.hit(now, None, "");
        assert_eq!(l.left_ms(now), Some(30 * MIN));
        l.hit(now, Some(MIN), "las 9:00");
        l.tick(now + 2 * MIN);
        l.hit(now + 2 * MIN, None, "");
        assert_eq!(l.left_ms(now + 2 * MIN), Some(15 * MIN));
    }

    #[test]
    fn the_owner_hears_it_whole_once_then_short() {
        let mut l = limit();
        assert_eq!(l.line(), None);
        l.hit(0, Some(60 * MIN), "las 18:00");
        assert_eq!(
            l.line(),
            Some(LimitLine::Hit {
                until: "las 18:00".into()
            })
        );
        assert_eq!(
            l.line(),
            Some(LimitLine::Still {
                until: "las 18:00".into()
            })
        );
        // Told, so the end asks to say she is back; once.
        assert_eq!(l.tick(61 * MIN), Some(LimitEnded { told_owner: true }));
        l.hit(62 * MIN, Some(MIN), "las 19:00");
        assert_eq!(l.tick(64 * MIN), Some(LimitEnded { told_owner: false }));
    }

    #[test]
    fn a_guessed_reset_has_no_time_to_say() {
        let mut l = limit();
        l.hit(0, None, "las 18:00");
        assert_eq!(l.line(), Some(LimitLine::Hit { until: "".into() }));
    }

    #[test]
    fn owner_turns_try_the_brain_only_with_an_unknown_reset_and_not_too_often() {
        let mut l = limit();
        assert!(l.owner_may_try(0), "not limited: always");
        l.hit(0, Some(60 * MIN), "las 18:00");
        assert!(!l.owner_may_try(30 * MIN), "known reset: never");
        l.hit(0, None, "");
        assert!(!l.owner_may_try(MIN));
        assert!(l.owner_may_try(5 * MIN));
        assert!(!l.owner_may_try(6 * MIN));
        assert!(l.owner_may_try(10 * MIN));
    }
}
