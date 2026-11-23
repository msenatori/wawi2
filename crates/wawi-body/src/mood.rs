//! The dialogue state as a look.

use wawi_core::State;

/// What she is doing, as far as her body shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mood {
    /// Nothing going on: her idle life (blinks, glances, a stretch) and,
    /// after a while, asleep.
    Idle,
    /// The key is held and the owner is talking: ears up, the green dot.
    Listening,
    /// The key went up: the recognizer is turning it into text.
    Transcribing,
    /// Claude Code is working on the answer.
    Thinking,
    /// She is talking.
    Speaking,
}

impl Mood {
    /// The look for a dialogue state.
    ///
    /// Waiting for a yes or no (`AwaitingConfirmation`) is listening while
    /// the owner talks, speaking while the question still plays, and idle
    /// otherwise: the green dot only ever means "this is being recorded".
    pub fn of(state: &State) -> Mood {
        match state {
            State::Idle => Mood::Idle,
            State::Listening { capture, .. } => {
                if capture.is_talking() {
                    Mood::Listening
                } else {
                    Mood::Transcribing
                }
            }
            State::Thinking { .. } => Mood::Thinking,
            State::Speaking { .. } => Mood::Speaking,
            State::AwaitingConfirmation {
                capture, playback, ..
            } => {
                if capture.is_talking() {
                    Mood::Listening
                } else if playback.is_some() {
                    Mood::Speaking
                } else {
                    Mood::Idle
                }
            }
        }
    }

    /// Spanish name, for the demo's console lines and the logs.
    pub fn label(self) -> &'static str {
        match self {
            Mood::Idle => "tranquila",
            Mood::Listening => "escuchando",
            Mood::Transcribing => "transcribiendo",
            Mood::Thinking => "pensando",
            Mood::Speaking => "hablando",
        }
    }
}

/// A short reaction played over whatever the mood shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Flash {
    /// She was cut off mid-sentence: a little recoil.
    Interrupted,
    /// Something failed (the brain, a watchdog): a shrug.
    Error,
}

impl Flash {
    pub fn label(self) -> &'static str {
        match self {
            Flash::Interrupted => "interrumpida",
            Flash::Error => "algo falló",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wawi_core::{Config, Dialogue, Effect, Event};

    fn dialogue() -> Dialogue {
        Dialogue::new(Config::default())
    }

    #[test]
    fn idle_is_idle() {
        assert_eq!(Mood::of(dialogue().state()), Mood::Idle);
    }

    #[test]
    fn talking_is_listening_and_the_key_up_is_transcribing() {
        let mut d = dialogue();
        d.handle(Event::SpeechStarted);
        assert_eq!(Mood::of(d.state()), Mood::Listening);
        d.handle(Event::SpeechEnded);
        assert_eq!(Mood::of(d.state()), Mood::Transcribing);
    }

    #[test]
    fn a_transcript_goes_to_thinking() {
        let mut d = dialogue();
        d.handle(Event::SpeechStarted);
        let segment = d
            .handle(Event::SpeechEnded)
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Transcribe { segment } => Some(segment),
                _ => None,
            })
            .expect("the core asks for the transcript");
        d.handle(Event::FinalTranscript {
            segment,
            text: "contame un chiste largo".into(),
        });
        assert!(
            matches!(Mood::of(d.state()), Mood::Thinking | Mood::Speaking),
            "{:?}",
            d.state().kind()
        );
    }
}
