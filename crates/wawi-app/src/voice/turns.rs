//! Per-turn latency: from the key release to the transcript, to Wawi's first
//! sentence and to her first audio. "Felt" is release to first audio: the
//! silence the owner actually sits through.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use wawi_core::{SpeechId, SpeechKind};
use wawi_memory::looks_like_secret;

#[derive(Debug)]
struct Turn {
    released: Instant,
    heard: Vec<String>,
    transcript_at: Option<Instant>,
    first_speak_at: Option<Instant>,
    first_audio_at: Option<Instant>,
    said: Vec<String>,
    speeches: HashSet<SpeechId>,
    route: Option<String>,
    brain: bool,
    /// The recognizer's text was probably not Spanish: the core asked to
    /// repeat instead of calling the brain.
    unclear: bool,
}

impl Turn {
    fn new(released: Instant) -> Self {
        Self {
            released,
            heard: Vec::new(),
            transcript_at: None,
            first_speak_at: None,
            first_audio_at: None,
            said: Vec::new(),
            speeches: HashSet::new(),
            route: None,
            brain: false,
            unclear: false,
        }
    }

    fn report(self, interrupted: bool) -> TurnReport {
        let since = |at: Option<Instant>| at.map(|at| at.saturating_duration_since(self.released));
        TurnReport {
            heard: self.heard.join(" "),
            said: self.said.join(" "),
            transcript: since(self.transcript_at),
            first_sentence: since(self.first_speak_at),
            first_audio: since(self.first_audio_at),
            via: match (self.route, self.brain) {
                _ if self.unclear => "no entendido".into(),
                (Some(route), _) => format!("ruta {route}"),
                (None, true) => "cerebro".into(),
                (None, false) => "núcleo".into(),
            },
            interrupted,
        }
    }
}

/// One finished turn, with its latencies measured from the key release.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnReport {
    pub heard: String,
    pub said: String,
    pub transcript: Option<Duration>,
    pub first_sentence: Option<Duration>,
    pub first_audio: Option<Duration>,
    /// "cerebro", "ruta time", "núcleo" (a failure or confirmation phrase) or
    /// "no entendido" (probably not Spanish: the core asked to repeat).
    pub via: String,
    /// The owner talked over the answer.
    pub interrupted: bool,
}

fn ms(duration: Option<Duration>) -> String {
    duration.map_or("-".into(), |d| format!("{} ms", d.as_millis()))
}

fn raw_ms(duration: Option<Duration>) -> String {
    duration.map_or("-".into(), |d| d.as_millis().to_string())
}

/// Text that looks like a secret never reaches a log file.
fn loggable(text: &str) -> String {
    if looks_like_secret(text) {
        "[parece un secreto: no se guarda]".into()
    } else {
        text.replace(['"', '\n'], " ")
    }
}

impl TurnReport {
    /// One line for the console.
    pub fn console(&self) -> String {
        format!(
            "  tiempos: transcripción {} · primera frase {} · primer audio {} · total sentido {}{}",
            ms(self.transcript),
            ms(self.first_sentence),
            ms(self.first_audio),
            ms(self.first_audio),
            if self.interrupted {
                " (la cortaste)"
            } else {
                ""
            }
        )
    }

    /// One line for `logs/turns.log`, after the timestamp.
    pub fn log_line(&self) -> String {
        format!(
            "transcript_ms={} first_sentence_ms={} first_audio_ms={} felt_ms={} via=\"{}\" interrupted={} heard=\"{}\" said=\"{}\"",
            raw_ms(self.transcript),
            raw_ms(self.first_sentence),
            raw_ms(self.first_audio),
            raw_ms(self.first_audio),
            self.via,
            self.interrupted,
            loggable(&self.heard),
            loggable(&self.said)
        )
    }
}

/// Follows the owner's turns through the host's events.
#[derive(Debug, Default)]
pub struct TurnTracker {
    current: Option<Turn>,
    release: Option<Instant>,
}

impl TurnTracker {
    /// The key went up. The segment closes a bit later (the release tail).
    pub fn released(&mut self, at: Instant) {
        self.release = Some(at);
    }

    /// The owner pressed the key. If Wawi had already answered, that turn
    /// ends here, cut.
    pub fn speech_started(&mut self) -> Option<TurnReport> {
        if self.current.as_ref()?.first_speak_at.is_some() {
            return self.current.take().map(|turn| turn.report(true));
        }
        None
    }

    /// The segment closed. Without a key release (a WAV fed in) this is the
    /// release.
    pub fn speech_ended(&mut self, now: Instant) {
        let released = self.release.take().unwrap_or(now);
        match &mut self.current {
            // Talking again before Wawi said anything: the core merges it
            // into one turn, timed from this last release.
            Some(turn) if turn.first_speak_at.is_none() => turn.released = released,
            _ => self.current = Some(Turn::new(released)),
        }
    }

    pub fn transcript(&mut self, text: &str, now: Instant) {
        let Some(turn) = &mut self.current else {
            return;
        };
        let text = text.trim();
        if text.is_empty() {
            // A tap or silence: nothing to time unless it joins earlier words.
            if turn.heard.is_empty() {
                self.current = None;
            }
            return;
        }
        turn.heard.push(text.to_owned());
        turn.transcript_at = Some(now);
    }

    /// Like [`transcript`](Self::transcript), for a transcript the core did
    /// not understand: the turn is logged with that text, "no entendido".
    pub fn unclear(&mut self, text: &str, now: Instant) {
        self.transcript(text, now);
        if let Some(turn) = &mut self.current {
            turn.unclear = true;
        }
    }

    pub fn brain_started(&mut self) {
        if let Some(turn) = &mut self.current {
            turn.brain = true;
        }
    }

    pub fn speak(&mut self, speech: SpeechId, text: &str, kind: &SpeechKind, now: Instant) {
        let Some(turn) = &mut self.current else {
            return;
        };
        if turn.transcript_at.is_none() {
            return;
        }
        turn.first_speak_at.get_or_insert(now);
        turn.said.push(text.to_owned());
        turn.speeches.insert(speech);
        if let SpeechKind::Route(name) = kind {
            turn.route = Some(name.clone());
        }
    }

    /// Audio for `speech` went to the speakers.
    pub fn audio(&mut self, speech: SpeechId, now: Instant) {
        if let Some(turn) = &mut self.current
            && turn.speeches.contains(&speech)
        {
            turn.first_audio_at.get_or_insert(now);
        }
    }

    /// The dialogue is idle again: the turn, if it heard something, is over.
    pub fn idle(&mut self) -> Option<TurnReport> {
        self.current.as_ref()?.transcript_at?;
        self.current.take().map(|turn| turn.report(false))
    }

    #[cfg(test)]
    pub fn is_active(&self) -> bool {
        self.current.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latencies_count_from_the_key_release() {
        let t = Instant::now();
        let at = |ms| t + Duration::from_millis(ms);
        let mut turns = TurnTracker::default();
        turns.released(at(0));
        turns.speech_ended(at(300));
        turns.transcript("hola", at(400));
        turns.brain_started();
        turns.speak(SpeechId(1), "¡Hola!", &SpeechKind::Answer, at(1400));
        turns.audio(SpeechId(1), at(1700));
        let report = turns.idle().unwrap();
        assert_eq!(report.transcript, Some(Duration::from_millis(400)));
        assert_eq!(report.first_sentence, Some(Duration::from_millis(1400)));
        assert_eq!(report.first_audio, Some(Duration::from_millis(1700)));
        assert_eq!(report.via, "cerebro");
        assert!(report.log_line().contains("felt_ms=1700"));
        assert!(!turns.is_active());
    }

    #[test]
    fn a_tap_is_not_a_turn() {
        let mut turns = TurnTracker::default();
        let t = Instant::now();
        turns.speech_ended(t);
        turns.transcript("", t);
        assert!(!turns.is_active());
        assert_eq!(turns.idle(), None);
    }

    #[test]
    fn talking_over_an_answer_ends_the_turn_as_cut() {
        let t = Instant::now();
        let mut turns = TurnTracker::default();
        turns.speech_ended(t);
        turns.transcript("contame algo", t);
        turns.speak(SpeechId(1), "Había una vez", &SpeechKind::Answer, t);
        let report = turns.speech_started().unwrap();
        assert!(report.interrupted);
        assert_eq!(report.first_audio, None);
    }

    #[test]
    fn secrets_stay_out_of_the_log() {
        let report = TurnReport {
            heard: "mi clave es sk-ant-abcdefghijklmnopqrstuvwxyz0123".into(),
            said: "Listo.".into(),
            transcript: None,
            first_sentence: None,
            first_audio: None,
            via: "cerebro".into(),
            interrupted: false,
        };
        assert!(
            !report.log_line().contains("sk-ant"),
            "{}",
            report.log_line()
        );
    }
}
