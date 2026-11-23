//! Local speech-to-text for Wawi with NVIDIA Parakeet TDT 0.6B v3 (int8 ONNX),
//! run on the CPU through ONNX Runtime by `transcribe-rs`.
//!
//! The core's `Stt` trait only names the segment to transcribe; it carries no
//! audio. The host hands the audio of the segment that just closed to
//! [`ParakeetStt::hand_audio`] right before it sends `SpeechEnded`, and the
//! `Transcribe { segment }` effect that follows picks it up. Recognition runs
//! on one worker thread and the result comes back as a `FinalTranscript` for
//! that segment through the `EventSink`. A segment always gets an answer, even
//! with no audio or a failed recognition (empty text), so the core never waits
//! for its listen timeout.
//!
//! With [`ParakeetStt::set_language_guard`] on, a transcript that is
//! probably not Spanish (see [`language`]) comes back as an
//! `UnclearTranscript` instead, and the core asks the owner to repeat without
//! calling the brain.
//!
//! Stale work never blocks new work for long: [`ParakeetStt::discard_pending`]
//! answers everything still queued with empty text at once, without running
//! the model. The core drops those answers as stale anyway.

pub mod audio;
pub mod language;
pub mod model;
pub mod wer;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use transcribe_rs::onnx::Quantization;
use transcribe_rs::onnx::parakeet::{ParakeetModel, ParakeetParams};
use wawi_core::{Event, EventSink, SegmentId, Stt};

use audio::Clip;

/// Turns 16 kHz mono audio into text. Implemented by [`Parakeet`]; tests use
/// fakes so they never need the model.
pub trait Recognizer: Send + 'static {
    fn recognize(&mut self, samples_16k: &[f32]) -> Result<String, String>;
}

/// The loaded model. Loading takes about a second and close to 1 GB of
/// memory, so do it once at startup and keep it.
pub struct Parakeet {
    model: ParakeetModel,
}

impl Parakeet {
    /// Loads the int8 model from `dir` (see [`model::verify`] and
    /// [`model::ensure`] to get the files there).
    pub fn load(dir: &Path) -> Result<Self, String> {
        let model =
            ParakeetModel::load(dir, &Quantization::Int8).map_err(|error| error.to_string())?;
        Ok(Self { model })
    }

    /// [`Parakeet::load`], plus how long it took.
    pub fn load_timed(dir: &Path) -> Result<(Self, Duration), String> {
        let started = Instant::now();
        let parakeet = Self::load(dir)?;
        Ok((parakeet, started.elapsed()))
    }
}

impl Recognizer for Parakeet {
    fn recognize(&mut self, samples_16k: &[f32]) -> Result<String, String> {
        if samples_16k.is_empty() {
            return Ok(String::new());
        }
        // The model detects the language itself; v3 takes no language hint.
        self.model
            .transcribe_with(samples_16k, &ParakeetParams::default())
            .map(|result| tidy(&result.text))
            .map_err(|error| error.to_string())
    }
}

/// The detokenizer glues Spanish opening marks to the word before them
/// ("Hola Wawi,¿cómo andás?"); put the space back.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 4);
    for c in text.trim().chars() {
        if matches!(c, '¿' | '¡') && out.chars().last().is_some_and(|p| !p.is_whitespace()) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

struct Job {
    segment: SegmentId,
    clip: Option<Clip>,
    generation: u64,
}

/// `Stt` on a worker thread, over any [`Recognizer`].
pub struct ParakeetStt {
    jobs: Option<Sender<Job>>,
    /// Audio of the segment that just closed, waiting for its `Transcribe`.
    pending: Option<Clip>,
    generation: Arc<AtomicU64>,
    language_guard: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ParakeetStt {
    /// Starts the worker thread, which owns `recognizer` from now on.
    pub fn spawn(recognizer: impl Recognizer, sink: Box<dyn EventSink + Send>) -> Self {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        let language_guard = Arc::new(AtomicBool::new(false));
        let guard = language_guard.clone();
        let worker = thread::Builder::new()
            .name("wawi-stt".into())
            .spawn(move || {
                let mut recognizer = recognizer;
                for job in inbox {
                    let text = if job.generation < current.load(Ordering::SeqCst) {
                        String::new()
                    } else {
                        recognize_clip(&mut recognizer, job.clip.as_ref())
                    };
                    let segment = job.segment;
                    let foreign =
                        guard.load(Ordering::SeqCst) && language::foreign_language(&text).is_some();
                    sink.send(if foreign {
                        Event::UnclearTranscript { segment, text }
                    } else {
                        Event::FinalTranscript { segment, text }
                    });
                }
            })
            .expect("spawn the stt worker");
        Self {
            jobs: Some(jobs),
            pending: None,
            generation,
            language_guard,
            worker: Some(worker),
        }
    }

    /// Off by default. On, a transcript that is probably not Spanish is sent
    /// as `UnclearTranscript` (with the text, for logs) instead of
    /// `FinalTranscript`. Takes effect from the next recognition.
    pub fn set_language_guard(&self, on: bool) {
        self.language_guard.store(on, Ordering::SeqCst);
    }

    /// The audio of the segment that is about to close, at any sample rate.
    /// Call it right before sending `SpeechEnded`; the next `transcribe` takes
    /// it. Handing new audio replaces audio nobody asked for.
    pub fn hand_audio(&mut self, samples: Vec<f32>, sample_rate: u32) {
        self.pending = Some(Clip {
            samples,
            sample_rate,
        });
    }

    /// Answers every queued segment with empty text right away, without
    /// running the model. For after a cancel, when they are all stale.
    pub fn discard_pending(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
}

fn recognize_clip(recognizer: &mut impl Recognizer, clip: Option<&Clip>) -> String {
    let Some(clip) = clip else {
        return String::new();
    };
    match recognizer.recognize(&clip.to_model_rate()) {
        Ok(text) => text,
        Err(error) => {
            // The core has no "STT failed" event; an empty transcript ends the
            // segment and the core reports it as such.
            eprintln!("wawi-stt-parakeet: recognition failed: {error}");
            String::new()
        }
    }
}

impl Stt for ParakeetStt {
    fn transcribe(&mut self, segment: SegmentId) {
        let job = Job {
            segment,
            clip: self.pending.take(),
            generation: self.generation.load(Ordering::SeqCst),
        };
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(job);
        }
    }
}

impl Drop for ParakeetStt {
    /// Lets the current recognition finish, then stops the worker.
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

    struct ChannelSink(Mutex<Sender<Event>>);

    impl EventSink for ChannelSink {
        fn send(&self, event: Event) {
            let _ = self.0.lock().unwrap().send(event);
        }
    }

    fn sink() -> (Box<dyn EventSink + Send>, Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        (Box::new(ChannelSink(Mutex::new(tx))), rx)
    }

    /// Says how many 16 kHz samples it got, after an optional delay.
    struct Counter {
        delay: Duration,
    }

    impl Recognizer for Counter {
        fn recognize(&mut self, samples: &[f32]) -> Result<String, String> {
            thread::sleep(self.delay);
            Ok(format!("{} muestras", samples.len()))
        }
    }

    fn counter(ms: u64) -> Counter {
        Counter {
            delay: Duration::from_millis(ms),
        }
    }

    struct Broken;

    impl Recognizer for Broken {
        fn recognize(&mut self, _: &[f32]) -> Result<String, String> {
            Err("boom".into())
        }
    }

    fn next(rx: &Receiver<Event>) -> (u64, String) {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Event::FinalTranscript { segment, text }) => (segment.0, text),
            other => panic!("expected a transcript, got {other:?}"),
        }
    }

    #[test]
    fn opening_marks_get_their_space_back() {
        assert_eq!(tidy(" Hola Wawi,¿cómo andás?"), "Hola Wawi, ¿cómo andás?");
        assert_eq!(tidy("¿Qué?¡Dale!"), "¿Qué? ¡Dale!");
        assert_eq!(tidy("¿Qué hora es?"), "¿Qué hora es?");
    }

    #[test]
    fn transcribes_the_handed_audio_at_16_khz_for_its_segment() {
        let (sink, rx) = sink();
        let mut stt = ParakeetStt::spawn(counter(0), sink);
        stt.hand_audio(vec![0.0; 48_000], 48_000);
        stt.transcribe(SegmentId(7));
        assert_eq!(next(&rx), (7, "16000 muestras".into()));
    }

    #[test]
    fn a_segment_without_audio_still_gets_an_empty_answer() {
        let (sink, rx) = sink();
        let mut stt = ParakeetStt::spawn(counter(0), sink);
        stt.hand_audio(vec![0.0; 160], 16_000);
        stt.transcribe(SegmentId(1));
        stt.transcribe(SegmentId(2));
        assert_eq!(next(&rx), (1, "160 muestras".into()));
        assert_eq!(next(&rx), (2, String::new()));
    }

    #[test]
    fn a_failed_recognition_answers_empty() {
        let (sink, rx) = sink();
        let mut stt = ParakeetStt::spawn(Broken, sink);
        stt.hand_audio(vec![0.0; 160], 16_000);
        stt.transcribe(SegmentId(3));
        assert_eq!(next(&rx), (3, String::new()));
    }

    #[test]
    fn transcribe_returns_at_once_and_answers_in_order() {
        let (sink, rx) = sink();
        let mut stt = ParakeetStt::spawn(counter(100), sink);
        let started = Instant::now();
        for segment in 1..=3_u64 {
            stt.hand_audio(vec![0.0; segment as usize * 10], 16_000);
            stt.transcribe(SegmentId(segment));
        }
        assert!(started.elapsed() < Duration::from_millis(50));
        for segment in 1..=3_u64 {
            assert_eq!(next(&rx), (segment, format!("{} muestras", segment * 10)));
        }
    }

    #[test]
    fn discarded_work_is_answered_without_waiting_for_the_model() {
        let (sink, rx) = sink();
        let mut stt = ParakeetStt::spawn(counter(300), sink);
        for segment in 1..=5 {
            stt.hand_audio(vec![0.0; 10], 16_000);
            stt.transcribe(SegmentId(segment));
        }
        stt.discard_pending();
        stt.hand_audio(vec![0.0; 20], 16_000);
        stt.transcribe(SegmentId(6));
        let started = Instant::now();
        // The first one may already be running; it finishes normally.
        let (first, _) = next(&rx);
        assert_eq!(first, 1);
        for segment in 2..=5 {
            assert_eq!(next(&rx), (segment, String::new()));
        }
        assert_eq!(next(&rx), (6, "20 muestras".into()));
        // One running job plus the new one, not six.
        assert!(started.elapsed() < Duration::from_millis(1_000));
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(10)),
            Err(RecvTimeoutError::Timeout)
        ));
    }

    struct Fixed(&'static str);

    impl Recognizer for Fixed {
        fn recognize(&mut self, _: &[f32]) -> Result<String, String> {
            Ok(self.0.to_owned())
        }
    }

    #[test]
    fn with_the_guard_on_foreign_speech_comes_back_unclear() {
        let (sink, rx) = sink();
        let mut stt = ParakeetStt::spawn(Fixed("Alors oui, mais vous"), sink);
        stt.hand_audio(vec![0.0; 160], 16_000);
        stt.transcribe(SegmentId(1));
        assert_eq!(next(&rx), (1, "Alors oui, mais vous".into()));
        stt.set_language_guard(true);
        stt.hand_audio(vec![0.0; 160], 16_000);
        stt.transcribe(SegmentId(2));
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Event::UnclearTranscript { segment, text }) => {
                assert_eq!(segment, SegmentId(2));
                assert_eq!(text, "Alors oui, mais vous");
            }
            other => panic!("expected an unclear transcript, got {other:?}"),
        }
        // Empty text is still a plain empty transcript.
        stt.transcribe(SegmentId(3));
        assert_eq!(next(&rx), (3, String::new()));
    }

    /// Needs the real model on disk (`stt-bench download`), then:
    /// `cargo test --release -p wawi-stt-parakeet -- --ignored --nocapture`.
    /// `WAWI2_STT_TEST_WAV=file.wav` also transcribes that file.
    #[test]
    #[ignore = "needs the ~670 MB model on disk"]
    fn the_real_model_hears_spanish() {
        let dir = model::model_dir(&model::PARAKEET_TDT_V3_INT8).unwrap();
        model::verify(&model::PARAKEET_TDT_V3_INT8, &dir).unwrap();
        let (mut parakeet, took) = Parakeet::load_timed(&dir).unwrap();
        eprintln!("loaded in {took:?}");
        // Silence must not invent words.
        let text = parakeet.recognize(&vec![0.0; 32_000]).unwrap();
        assert!(text.split_whitespace().count() <= 1, "{text}");
        if let Ok(path) = std::env::var("WAWI2_STT_TEST_WAV") {
            let clip = audio::read_wav(Path::new(&path)).unwrap();
            let text = parakeet.recognize(&clip.to_model_rate()).unwrap();
            eprintln!("{path}: {text}");
            assert!(!text.is_empty());
        }
    }

    /// Every take in a folder of the owner's Spanish recordings must pass the
    /// language guard. Needs the model, like the test above:
    /// `WAWI2_STT_GUARD_DIR=~/Desktop/wawi-voz cargo test --release -p
    /// wawi-stt-parakeet -- --ignored --nocapture guard`.
    #[test]
    #[ignore = "needs the model and a folder of recordings"]
    fn the_owners_spanish_takes_pass_the_language_guard() {
        let Some(folder) = std::env::var_os("WAWI2_STT_GUARD_DIR") else {
            eprintln!("WAWI2_STT_GUARD_DIR not set; nothing to check");
            return;
        };
        let dir = model::model_dir(&model::PARAKEET_TDT_V3_INT8).unwrap();
        let (mut parakeet, _) = Parakeet::load_timed(&dir).unwrap();
        let mut wavs: Vec<_> = std::fs::read_dir(&folder)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "wav"))
            .collect();
        wavs.sort();
        assert!(!wavs.is_empty());
        let mut rejected = Vec::new();
        for path in &wavs {
            let clip = audio::read_wav(path).unwrap();
            let text = parakeet.recognize(&clip.to_model_rate()).unwrap();
            let verdict = language::foreign_language(&text);
            eprintln!("{:?} {}: {text}", verdict, path.display());
            if verdict.is_some() {
                rejected.push(text);
            }
        }
        eprintln!("{} takes, {} rejected", wavs.len(), rejected.len());
        assert!(rejected.is_empty(), "{rejected:?}");
    }
}
