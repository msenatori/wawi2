//! The voice program and Wawi's body (`wawi-body`): what the host tells her
//! (her look follows the dialogue, the bubble shows what was said) and what
//! her menu asks of the program (stop listening, quit).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use wawi_body::{BodyLink, Command, Flash, Mood, Update};
use wawi_core::State;

/// The host's side of the body: sends each change once.
pub struct Reporter {
    link: BodyLink,
    mood: Mood,
}

impl Reporter {
    pub fn new(link: BodyLink) -> Self {
        Self {
            link,
            mood: Mood::Idle,
        }
    }

    /// After every core event: her look, if it changed.
    pub fn state(&mut self, state: &State) {
        let mood = Mood::of(state);
        if mood != self.mood {
            self.mood = mood;
            self.link.send(Update::Mood(mood));
        }
    }

    pub fn heard(&self, text: &str) {
        if !text.trim().is_empty() {
            self.link.send(Update::Heard(text.trim().to_owned()));
        }
    }

    pub fn said(&self, text: &str) {
        self.link.send(Update::Said(text.to_owned()));
    }

    pub fn flash(&self, flash: Flash) {
        self.link.send(Update::Flash(flash));
    }
}

/// What the menu can change while the program runs. Shared by the UI thread
/// (which applies commands) and the key's thread (which reads the gate).
pub struct Controls {
    /// The microphone gate: while set, the push-to-talk key does nothing.
    deaf: AtomicBool,
    /// Set by «Salir»: the same flag Ctrl+C sets, so the same exit.
    quit: &'static AtomicBool,
    /// To let go of the key if the owner mutes while holding it.
    release: Mutex<Option<Box<dyn Fn() + Send>>>,
}

impl Controls {
    pub fn new(quit: &'static AtomicBool) -> Self {
        Self {
            deaf: AtomicBool::new(false),
            quit,
            release: Mutex::new(None),
        }
    }

    pub fn deaf(&self) -> bool {
        self.deaf.load(Ordering::SeqCst)
    }

    /// How to release the key, once the audio is up.
    pub fn on_release(&self, release: impl Fn() + Send + 'static) {
        if let Ok(mut slot) = self.release.lock() {
            *slot = Some(Box::new(release));
        }
    }

    /// Carries out a menu command; the body hears back about the mute.
    pub fn apply(&self, command: Command, link: &BodyLink) {
        match command {
            Command::SetListening(listening) => {
                self.deaf.store(!listening, Ordering::SeqCst);
                if !listening
                    && let Ok(slot) = self.release.lock()
                    && let Some(release) = slot.as_ref()
                {
                    release();
                }
                link.send(Update::Muted(!listening));
            }
            Command::Quit => self.quit.store(true, Ordering::SeqCst),
        }
    }
}

/// The key, through the gate: a press while deaf is ignored, and so is its
/// release (it never started a turn).
pub struct KeyGate {
    honored: AtomicBool,
}

impl Default for KeyGate {
    fn default() -> Self {
        Self {
            honored: AtomicBool::new(false),
        }
    }
}

impl KeyGate {
    /// `Some(held)` to pass on to the audio, `None` to ignore the change.
    pub fn key(&self, down: bool, deaf: bool) -> Option<bool> {
        if down {
            if deaf {
                return None;
            }
            self.honored.store(true, Ordering::SeqCst);
            Some(true)
        } else if self.honored.swap(false, Ordering::SeqCst) {
            Some(false)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[test]
    fn stop_listening_closes_the_gate_and_tells_the_body() {
        static QUIT: AtomicBool = AtomicBool::new(false);
        let controls = Controls::new(&QUIT);
        let released = Arc::new(AtomicUsize::new(0));
        let count = released.clone();
        controls.on_release(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        let (link, rx) = BodyLink::detached();

        controls.apply(Command::SetListening(false), &link);
        assert!(controls.deaf());
        assert_eq!(released.load(Ordering::SeqCst), 1, "a held key lets go");
        assert_eq!(rx.try_recv(), Ok(Update::Muted(true)));

        controls.apply(Command::SetListening(true), &link);
        assert!(!controls.deaf());
        assert_eq!(released.load(Ordering::SeqCst), 1);
        assert_eq!(rx.try_recv(), Ok(Update::Muted(false)));
        assert!(!QUIT.load(Ordering::SeqCst));
    }

    #[test]
    fn quit_sets_the_same_flag_as_ctrl_c() {
        static QUIT: AtomicBool = AtomicBool::new(false);
        let controls = Controls::new(&QUIT);
        let (link, rx) = BodyLink::detached();
        controls.apply(Command::Quit, &link);
        assert!(QUIT.load(Ordering::SeqCst));
        assert!(rx.try_recv().is_err(), "the program closes the body itself");
    }

    #[test]
    fn a_deaf_key_does_nothing_not_even_on_release() {
        let gate = KeyGate::default();
        assert_eq!(gate.key(true, true), None);
        assert_eq!(gate.key(false, true), None);
        assert_eq!(gate.key(true, false), Some(true));
        // Muted while held: the release still goes through.
        assert_eq!(gate.key(false, true), Some(false));
        assert_eq!(gate.key(false, false), None, "no double release");
    }

    #[test]
    fn the_reporter_sends_each_look_once() {
        let (link, rx) = BodyLink::detached();
        let mut reporter = Reporter::new(link);
        let dialogue = wawi_core::Dialogue::new(wawi_core::Config::default());
        reporter.state(dialogue.state());
        assert!(rx.try_recv().is_err(), "idle was already the look");
        let mut dialogue = dialogue;
        dialogue.handle(wawi_core::Event::SpeechStarted);
        reporter.state(dialogue.state());
        reporter.state(dialogue.state());
        assert_eq!(rx.try_recv(), Ok(Update::Mood(Mood::Listening)));
        assert!(rx.try_recv().is_err());
        reporter.heard("   ");
        assert!(rx.try_recv().is_err());
    }
}
