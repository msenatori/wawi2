//! The speech bubble over her head: what she heard, then what she says, and
//! a fade a few seconds after she is done (Wawi 1's `WawiBubbleView`).

use crate::mood::Mood;

/// How long the fade itself takes.
pub const FADE: f64 = 0.6;
/// Longer texts are cut with "…" (the bubble is 210 points wide).
pub const MAX_CHARS: usize = 220;

/// Whose words the bubble shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Voice {
    /// What the owner said, as the recognizer heard it (drawn lighter).
    Heard,
    /// What Wawi is saying.
    Said,
}

#[derive(Clone, Debug, Default)]
pub struct Bubble {
    line: Option<(Voice, String)>,
    /// When the fade starts (seconds on the caller's clock).
    fade_at: Option<f64>,
}

/// How long a finished line stays up before fading: time to read it.
pub fn hold_for(text: &str) -> f64 {
    (2.0 + 0.05 * text.chars().count() as f64).clamp(3.0, 10.0)
}

/// One line, whitespace collapsed, cut to [`MAX_CHARS`].
pub fn tidy(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let line = words.join(" ");
    if line.chars().count() <= MAX_CHARS {
        return line;
    }
    let cut: String = line.chars().take(MAX_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

impl Bubble {
    pub fn heard(&mut self, text: &str) {
        self.set(Voice::Heard, &format!("«{}»", text.trim()));
    }

    pub fn said(&mut self, text: &str) {
        self.set(Voice::Said, text);
    }

    fn set(&mut self, voice: Voice, text: &str) {
        let text = tidy(text);
        if text.is_empty() || text == "«»" {
            return;
        }
        self.line = Some((voice, text));
        self.fade_at = None;
    }

    /// Follows the dialogue: a new turn clears it, being idle starts the
    /// countdown, anything else keeps it up.
    pub fn mood(&mut self, mood: Mood, now: f64) {
        match mood {
            Mood::Listening => self.clear(),
            Mood::Idle => {
                if let Some((_, text)) = &self.line
                    && self.fade_at.is_none()
                {
                    self.fade_at = Some(now + hold_for(text));
                }
            }
            _ => self.fade_at = None,
        }
    }

    pub fn clear(&mut self) {
        self.line = None;
        self.fade_at = None;
    }

    /// Opacity now, 0..1.
    pub fn alpha(&self, now: f64) -> f64 {
        match (&self.line, self.fade_at) {
            (None, _) => 0.0,
            (Some(_), None) => 1.0,
            (Some(_), Some(at)) => (1.0 - (now - at) / FADE).clamp(0.0, 1.0),
        }
    }

    /// What to show now; drops the line once it has faded out.
    pub fn current(&mut self, now: f64) -> Option<(Voice, &str)> {
        if self.line.is_some() && self.alpha(now) <= 0.0 {
            self.clear();
        }
        self.line
            .as_ref()
            .map(|(voice, text)| (*voice, text.as_str()))
    }

    /// Seconds until the bubble changes on its own, if it will.
    pub fn next_change_in(&self, now: f64) -> Option<f64> {
        let at = self.fade_at?;
        self.line.as_ref()?;
        Some(if now >= at { 0.0 } else { at - now })
    }

    pub fn fading(&self, now: f64) -> bool {
        self.fade_at.is_some_and(|at| now >= at) && self.line.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heard_then_said_then_it_fades() {
        let mut b = Bubble::default();
        b.mood(Mood::Listening, 0.0);
        b.heard("  ¿qué hora es? ");
        assert_eq!(b.current(1.0), Some((Voice::Heard, "«¿qué hora es?»")));
        b.mood(Mood::Thinking, 1.2);
        b.said("Son las tres.");
        b.mood(Mood::Speaking, 2.0);
        assert_eq!(b.current(2.0), Some((Voice::Said, "Son las tres.")));
        assert_eq!(b.next_change_in(2.0), None, "up while she talks");

        b.mood(Mood::Idle, 3.0);
        let hold = hold_for("Son las tres.");
        assert_eq!(b.alpha(3.0 + hold - 0.1), 1.0);
        assert!((b.next_change_in(3.0).unwrap() - hold).abs() < 1e-9);
        let mid = b.alpha(3.0 + hold + FADE / 2.0);
        assert!(mid > 0.0 && mid < 1.0);
        assert!(b.fading(3.0 + hold + 0.1));
        assert_eq!(b.current(3.0 + hold + FADE + 0.01), None);
    }

    #[test]
    fn a_new_turn_clears_it_and_talking_again_keeps_it_up() {
        let mut b = Bubble::default();
        b.said("Hola");
        b.mood(Mood::Idle, 0.0);
        b.mood(Mood::Speaking, 1.0);
        assert_eq!(b.alpha(100.0), 1.0, "speaking again cancels the fade");
        b.mood(Mood::Listening, 2.0);
        assert_eq!(b.current(2.0), None);
    }

    #[test]
    fn long_answers_are_cut_and_empty_ones_ignored() {
        let long = "palabra ".repeat(100);
        let line = tidy(&long);
        assert_eq!(line.chars().count(), MAX_CHARS);
        assert!(line.ends_with('…'));
        let mut b = Bubble::default();
        b.heard("   ");
        assert_eq!(b.current(0.0), None);
        assert_eq!(tidy("hola\n\n  che"), "hola che");
    }

    #[test]
    fn reading_time_grows_with_the_text_but_is_bounded() {
        assert_eq!(hold_for("sí"), 3.0);
        assert!(hold_for(&"a".repeat(100)) > hold_for(&"a".repeat(40)));
        assert_eq!(hold_for(&"a".repeat(1000)), 10.0);
    }
}
