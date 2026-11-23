//! Text-to-speech for Wawi with Kokoro-82M on ONNX Runtime, in the CPU, with
//! Wawi 1's voice (`jf_alpha`, speed 1.05, three semitones up) and a
//! rule-based Spanish phonemizer (no espeak-ng, so no GPL code).
//!
//! The core's `Tts::synthesize(speech, text)` starts the work and returns at
//! once. A worker thread splits the text into sentences and synthesizes them
//! one by one, and every sentence goes out as soon as it is ready, as a
//! [`SpeechAudio`] chunk for that `SpeechId`, through an [`AudioSink`]. Each
//! `synthesize` call ends with exactly one chunk marked `last`, even when the
//! text had nothing to say, synthesis failed or the speech was discarded, so
//! the audio side always knows when a speech is complete and can report
//! `PlaybackFinished`.
//!
//! Audio is always [`SAMPLE_RATE`] (24 kHz), mono, `f32` in about [-1, 1];
//! the audio side resamples to its device rate.
//!
//! Stale speech never blocks new speech for long:
//! [`KokoroTts::discard_pending`] ends everything still queued, and what is
//! left of the speech being synthesized, with an empty `last` chunk, without
//! running the model.

pub mod dsp;
pub mod g2p;
pub mod kokoro;
pub mod model;
pub mod vocab;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use wawi_core::{SpeechId, Tts};

pub use g2p::Accent;
pub use kokoro::{Kokoro, KokoroVoice, VoiceSettings};

/// Kokoro's output rate. Every [`SpeechAudio`] is at this rate.
pub const SAMPLE_RATE: u32 = 24_000;

/// Turns one sentence into 24 kHz mono audio. Implemented by
/// [`KokoroVoice`]; tests use fakes so they never need the model.
pub trait Synthesizer: Send + 'static {
    fn synthesize(&mut self, sentence: &str) -> Result<Vec<f32>, String>;
}

/// A piece of synthesized speech: usually one sentence.
#[derive(Clone, Debug, PartialEq)]
pub struct SpeechAudio {
    pub speech: SpeechId,
    /// Mono `f32` at `sample_rate`. May be empty on the `last` chunk.
    pub samples: Vec<f32>,
    /// Always [`SAMPLE_RATE`]; carried so the receiver never has to assume.
    pub sample_rate: u32,
    /// Nothing more will come for this speech.
    pub last: bool,
}

/// Where synthesized audio goes, typically the `AudioOut` queue. Called from
/// the worker thread. Any `Fn(SpeechAudio)` closure is a sink.
pub trait AudioSink: Send + 'static {
    fn audio(&self, chunk: SpeechAudio);
}

impl<F: Fn(SpeechAudio) + Send + 'static> AudioSink for F {
    fn audio(&self, chunk: SpeechAudio) {
        self(chunk)
    }
}

struct Job {
    speech: SpeechId,
    text: String,
    generation: u64,
}

/// `Tts` on a worker thread, over any [`Synthesizer`].
pub struct KokoroTts {
    jobs: Option<Sender<Job>>,
    generation: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}

impl KokoroTts {
    /// Starts the worker thread, which owns `synthesizer` from now on.
    pub fn spawn(synthesizer: impl Synthesizer, sink: impl AudioSink) -> Self {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        let worker = thread::Builder::new()
            .name("wawi-tts".into())
            .spawn(move || {
                let mut synthesizer = synthesizer;
                for job in inbox {
                    run_job(&mut synthesizer, &sink, &job, || {
                        job.generation < current.load(Ordering::SeqCst)
                    });
                }
            })
            .expect("spawn the tts worker");
        Self {
            jobs: Some(jobs),
            generation,
            worker: Some(worker),
        }
    }

    /// Ends every queued speech, and the rest of the one being synthesized,
    /// with an empty `last` chunk, without running the model. For after a
    /// cancel or `StopPlayback`, when all of it is stale. Speech handed to
    /// `synthesize` after this call is not affected.
    pub fn discard_pending(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
}

fn run_job(
    synthesizer: &mut impl Synthesizer,
    sink: &impl AudioSink,
    job: &Job,
    stale: impl Fn() -> bool,
) {
    let chunk = |samples: Vec<f32>, last: bool| SpeechAudio {
        speech: job.speech,
        samples,
        sample_rate: SAMPLE_RATE,
        last,
    };
    let sentences = speech_pieces(&job.text);
    for (i, sentence) in sentences.iter().enumerate() {
        if stale() {
            break;
        }
        let samples = match synthesizer.synthesize(sentence) {
            Ok(samples) => samples,
            Err(error) => {
                // The core has no "TTS failed" event; the speech just ends
                // here and the core moves on when it finishes "playing".
                eprintln!("wawi-tts-kokoro: synthesis failed: {error}");
                break;
            }
        };
        let last = i + 1 == sentences.len();
        if stale() {
            break;
        }
        if last {
            sink.audio(chunk(samples, true));
            return;
        }
        if !samples.is_empty() {
            sink.audio(chunk(samples, false));
        }
    }
    sink.audio(chunk(Vec::new(), true));
}

/// Longest first piece synthesized whole. A longer opening sentence is cut
/// at its first comma, colon or semicolon, so its first words play after
/// ~300 ms instead of ~900 ms; the rest is ready before they end.
const FIRST_PIECE_CHARS: usize = 60;

/// The pieces a speech is synthesized in: its sentences, with a long first
/// one cut after its first clause.
pub fn speech_pieces(text: &str) -> Vec<String> {
    let mut sentences = split_sentences(text);
    if let Some(first) = sentences.first()
        && first.chars().count() > FIRST_PIECE_CHARS
        && let Some((head, tail)) = first_clause(first)
    {
        sentences.splice(0..1, [head, tail]);
    }
    sentences
}

/// Splits after the first ", ", ": " or "; " that leaves at least two words
/// on each side.
fn first_clause(sentence: &str) -> Option<(String, String)> {
    let chars: Vec<char> = sentence.chars().collect();
    for i in 0..chars.len().saturating_sub(1) {
        if matches!(chars[i], ',' | ':' | ';') && chars[i + 1] == ' ' {
            let head: String = chars[..=i].iter().collect();
            let tail: String = chars[i + 2..].iter().collect();
            if head.split_whitespace().count() >= 2 && tail.split_whitespace().count() >= 2 {
                return Some((head, tail));
            }
        }
    }
    None
}

/// Sentences, so the first one can start playing while the rest are made.
/// Cuts after `.`, `!`, `?` or `…` followed by a space, and at line breaks,
/// but not after an abbreviation ("Sr. Pérez").
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '\n' {
            push_sentence(&mut sentences, &mut current);
            continue;
        }
        current.push(c);
        let ends = matches!(c, '.' | '!' | '?' | '…')
            && chars.get(i + 1).is_none_or(|n| n.is_whitespace())
            && !(c == '.' && ends_with_abbreviation(&current));
        if ends {
            push_sentence(&mut sentences, &mut current);
        }
    }
    push_sentence(&mut sentences, &mut current);
    sentences
}

fn push_sentence(sentences: &mut Vec<String>, current: &mut String) {
    let sentence = current.trim();
    if sentence.chars().any(|c| c.is_alphanumeric()) {
        sentences.push(sentence.to_owned());
    }
    current.clear();
}

fn ends_with_abbreviation(text: &str) -> bool {
    let before = text.trim_end_matches('.');
    let word: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_alphabetic())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    g2p::normalize::is_abbreviation(&word)
}

impl Tts for KokoroTts {
    fn synthesize(&mut self, speech: SpeechId, text: &str) {
        let job = Job {
            speech,
            text: text.to_owned(),
            generation: self.generation.load(Ordering::SeqCst),
        };
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(job);
        }
    }
}

impl Drop for KokoroTts {
    /// Lets the current sentence finish, then stops the worker.
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc::{Receiver, RecvTimeoutError};
    use std::time::{Duration, Instant};

    /// One sample per character, after an optional delay.
    struct Fake {
        delay: Duration,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Synthesizer for Fake {
        fn synthesize(&mut self, sentence: &str) -> Result<Vec<f32>, String> {
            thread::sleep(self.delay);
            self.log.lock().unwrap().push(sentence.to_owned());
            if sentence.contains("rompe") {
                return Err("boom".into());
            }
            Ok(vec![0.1; sentence.chars().count()])
        }
    }

    fn spawn(ms: u64) -> (KokoroTts, Receiver<SpeechAudio>, Arc<Mutex<Vec<String>>>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let log = Arc::new(Mutex::new(Vec::new()));
        let fake = Fake {
            delay: Duration::from_millis(ms),
            log: log.clone(),
        };
        let tts = KokoroTts::spawn(fake, move |chunk| {
            let _ = tx.lock().unwrap().send(chunk);
        });
        (tts, rx, log)
    }

    fn next(rx: &Receiver<SpeechAudio>) -> (u64, usize, bool) {
        let chunk = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a chunk in time");
        assert_eq!(chunk.sample_rate, SAMPLE_RATE);
        (chunk.speech.0, chunk.samples.len(), chunk.last)
    }

    #[test]
    fn splits_sentences_but_not_abbreviations_or_numbers() {
        assert_eq!(
            split_sentences("Hola. ¿Cómo andás? ¡Bien!"),
            vec!["Hola.", "¿Cómo andás?", "¡Bien!"]
        );
        assert_eq!(
            split_sentences("El Sr. Pérez llegó a las 3.15 y se fue… Chau."),
            vec!["El Sr. Pérez llegó a las 3.15 y se fue…", "Chau."]
        );
        assert_eq!(split_sentences("uno\ndos"), vec!["uno", "dos"]);
        assert!(split_sentences("  ...  ").is_empty());
    }

    #[test]
    fn a_long_opening_sentence_starts_with_its_first_clause() {
        assert_eq!(
            speech_pieces(
                "Tenés tres reuniones hoy: la primera a las nueve y la última a la tarde. Listo."
            ),
            vec![
                "Tenés tres reuniones hoy:",
                "la primera a las nueve y la última a la tarde.",
                "Listo."
            ]
        );
        assert_eq!(speech_pieces("Sí, dale."), vec!["Sí, dale."]);
    }

    #[test]
    fn each_sentence_goes_out_as_soon_as_it_is_ready() {
        let (mut tts, rx, _) = spawn(0);
        tts.synthesize(SpeechId(1), "Hola. Chau.");
        assert_eq!(next(&rx), (1, 5, false));
        assert_eq!(next(&rx), (1, 5, true));
    }

    #[test]
    fn nothing_to_say_still_ends_the_speech() {
        let (mut tts, rx, log) = spawn(0);
        tts.synthesize(SpeechId(2), "  😀 ");
        assert_eq!(next(&rx), (2, 0, true));
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn a_failure_ends_the_speech_with_an_empty_last_chunk() {
        let (mut tts, rx, log) = spawn(0);
        tts.synthesize(SpeechId(3), "Bien. Esto se rompe. No llega.");
        assert_eq!(next(&rx), (3, 5, false));
        assert_eq!(next(&rx), (3, 0, true));
        assert_eq!(log.lock().unwrap().len(), 2);
    }

    #[test]
    fn synthesize_returns_at_once_and_speeches_stay_in_order() {
        let (mut tts, rx, _) = spawn(50);
        let started = Instant::now();
        for speech in 1..=3 {
            tts.synthesize(SpeechId(speech), "Hola.");
        }
        assert!(started.elapsed() < Duration::from_millis(30));
        for speech in 1..=3 {
            assert_eq!(next(&rx), (speech, 5, true));
        }
    }

    #[test]
    fn discarded_speech_ends_without_waiting_for_the_model() {
        let (mut tts, rx, log) = spawn(200);
        tts.synthesize(SpeechId(1), "Uno. Dos. Tres. Cuatro.");
        for speech in 2..=5 {
            tts.synthesize(SpeechId(speech), "Algo largo. Más.");
        }
        thread::sleep(Duration::from_millis(50));
        tts.discard_pending();
        tts.synthesize(SpeechId(6), "Nuevo.");
        let started = Instant::now();
        // The sentence already running finishes but is not sent.
        assert_eq!(next(&rx), (1, 0, true));
        for speech in 2..=5 {
            assert_eq!(next(&rx), (speech, 0, true));
        }
        assert_eq!(next(&rx), (6, 6, true));
        // One running sentence plus the new one, not eleven.
        assert!(started.elapsed() < Duration::from_millis(1_000));
        assert_eq!(log.lock().unwrap().len(), 2);
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(10)),
            Err(RecvTimeoutError::Timeout)
        ));
    }
}
