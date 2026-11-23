//! Kokoro-82M on ONNX Runtime, and Wawi's voice on top of it.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;

use crate::dsp;
use crate::g2p::{self, Accent};
use crate::vocab::{self, MAX_TOKENS};
use crate::{SAMPLE_RATE, Synthesizer};

/// Floats per style vector.
const STYLE_DIM: usize = 256;

/// The loaded model and one voice. Loading takes well under a second; keep it
/// for the life of the program.
pub struct Kokoro {
    session: Session,
    /// One style vector per input length, `MAX_TOKENS` of them.
    voice: Vec<f32>,
}

impl Kokoro {
    /// Loads `model.onnx` and `<voice>.bin` from `dir` (see
    /// [`crate::model::ensure`]).
    pub fn load(dir: &Path, voice: &str) -> Result<Self, String> {
        let voice = read_voice(&dir.join(format!("{voice}.bin")))?;
        let session = Session::builder()
            .map_err(|e| e.to_string())?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| e.to_string())?
            .with_intra_threads(intra_threads())
            .map_err(|e| e.to_string())?
            .commit_from_file(dir.join("model.onnx"))
            .map_err(|e| format!("loading model.onnx: {e}"))?;
        Ok(Self { session, voice })
    }

    /// Audio for a token sequence of at most [`MAX_TOKENS`], at
    /// [`SAMPLE_RATE`], as the model makes it (silence included).
    pub fn infer(&mut self, tokens: &[i64], speed: f32) -> Result<Vec<f32>, String> {
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        if tokens.len() > MAX_TOKENS {
            return Err(format!(
                "{} tokens, the model takes {MAX_TOKENS}",
                tokens.len()
            ));
        }
        // Like Kokoro's own pipeline: pad with 0 on both sides, and use the
        // style vector for this many tokens.
        let mut ids = Vec::with_capacity(tokens.len() + 2);
        ids.push(0);
        ids.extend_from_slice(tokens);
        ids.push(0);
        let row = tokens.len() - 1;
        let style = self.voice[row * STYLE_DIM..(row + 1) * STYLE_DIM].to_vec();
        let ids = Tensor::from_array(([1_usize, ids.len()], ids)).map_err(|e| e.to_string())?;
        let style = Tensor::from_array(([1_usize, STYLE_DIM], style)).map_err(|e| e.to_string())?;
        let speed = Tensor::from_array(([1_usize], vec![speed])).map_err(|e| e.to_string())?;
        let outputs = self
            .session
            .run(ort::inputs!["input_ids" => ids, "style" => style, "speed" => speed])
            .map_err(|e| e.to_string())?;
        let (_, audio) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        Ok(audio.to_vec())
    }
}

/// One thread per core, up to eight: measured on an M4 Pro (8 performance +
/// 4 efficiency cores), eight is about 10 % faster than ONNX Runtime's
/// default of every core, and more only adds the slow cores.
fn intra_threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().min(8))
}

fn read_voice(path: &Path) -> Result<Vec<f32>, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() != MAX_TOKENS * STYLE_DIM * 4 {
        return Err(format!(
            "{}: {} bytes, expected {}",
            path.display(),
            bytes.len(),
            MAX_TOKENS * STYLE_DIM * 4
        ));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// How Wawi sounds.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceSettings {
    /// A Kokoro voice in the model folder.
    pub voice: String,
    /// Speaking rate, 1.0 = Kokoro's normal.
    pub speed: f32,
    /// Pitch (and formant) shift in semitones; 0 = the voice as Kokoro makes it.
    pub pitch_semitones: f32,
    pub accent: Accent,
}

impl Default for VoiceSettings {
    /// Wawi 1's voice, from its `private/config.yaml`: `jf_alpha` (Kokoro's
    /// Japanese voice speaking Spanish, "voz de cachorra"), speed 1.05, three
    /// semitones up, and espeak-ng's Castilian accent: the owner prefers
    /// how Wawi 1 sounded.
    fn default() -> Self {
        Self {
            voice: "jf_alpha".into(),
            speed: 1.05,
            pitch_semitones: 3.0,
            accent: Accent::default(),
        }
    }
}

impl VoiceSettings {
    /// The speed to ask the model for, so that after the pitch shift (which
    /// shortens the audio) the result comes out at `speed`. Kokoro accepts
    /// 0.5 to 2.0. Same formula as Wawi 1.
    pub fn model_speed(&self) -> f32 {
        let speed = f64::from(self.speed) / dsp::pitch_ratio(self.pitch_semitones);
        speed.clamp(0.5, 2.0) as f32
    }
}

/// Pause between two pieces of a sentence too long for one model call.
const JOIN_PAUSE_S: f32 = 0.1;

/// Kokoro with Wawi's voice: text in, finished 24 kHz audio out.
pub struct KokoroVoice {
    kokoro: Kokoro,
    settings: VoiceSettings,
}

impl KokoroVoice {
    pub fn load(dir: &Path, settings: VoiceSettings) -> Result<Self, String> {
        let kokoro = Kokoro::load(dir, &settings.voice)?;
        Ok(Self { kokoro, settings })
    }

    /// [`KokoroVoice::load`] plus one throwaway synthesis, so the first real
    /// sentence does not pay ONNX Runtime's first-run cost. Returns how long
    /// loading and warming took.
    pub fn load_warm(
        dir: &Path,
        settings: VoiceSettings,
    ) -> Result<(Self, Duration, Duration), String> {
        let started = Instant::now();
        let mut voice = Self::load(dir, settings)?;
        let loaded = started.elapsed();
        voice.synthesize("Sí.")?;
        Ok((voice, loaded, started.elapsed() - loaded))
    }

    pub fn settings(&self) -> &VoiceSettings {
        &self.settings
    }

    pub fn phonemes(&self, text: &str) -> String {
        g2p::phonemize(text, self.settings.accent)
    }
}

impl Synthesizer for KokoroVoice {
    fn synthesize(&mut self, sentence: &str) -> Result<Vec<f32>, String> {
        let phonemes = self.phonemes(sentence);
        let speed = self.settings.model_speed();
        let mut audio: Vec<f32> = Vec::new();
        for piece in split_tokens(&phonemes) {
            let raw = self.kokoro.infer(&piece, speed)?;
            if !audio.is_empty() {
                audio.extend(std::iter::repeat_n(
                    0.0,
                    (JOIN_PAUSE_S * SAMPLE_RATE as f32) as usize,
                ));
            }
            audio.extend_from_slice(dsp::trim_silence(&raw));
        }
        Ok(dsp::retune(&audio, self.settings.pitch_semitones))
    }
}

/// Token pieces of at most [`MAX_TOKENS`], cut after punctuation when
/// possible, otherwise at a space.
pub fn split_tokens(phonemes: &str) -> Vec<Vec<i64>> {
    let tokens = vocab::tokens(phonemes);
    let space = vocab::token(' ');
    let marks: Vec<i64> = ",.;:!?…".chars().filter_map(vocab::token).collect();
    let mut pieces = Vec::new();
    let mut rest: &[i64] = &tokens;
    while rest.len() > MAX_TOKENS {
        let window = &rest[..MAX_TOKENS];
        let cut = window
            .iter()
            .rposition(|t| marks.contains(t))
            .or_else(|| window.iter().rposition(|t| Some(*t) == space))
            .map_or(MAX_TOKENS, |i| i + 1);
        pieces.push(rest[..cut].to_vec());
        rest = &rest[cut..];
        while rest.first().is_some_and(|t| Some(*t) == space) {
            rest = &rest[1..];
        }
    }
    if !rest.is_empty() {
        pieces.push(rest.to_vec());
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wawi_1_voice_by_default() {
        let settings = VoiceSettings::default();
        assert_eq!(settings.voice, "jf_alpha");
        // 1.05 / 2^(3/12), as Wawi 1 asks Kokoro for.
        assert!((settings.model_speed() - 0.8829).abs() < 1e-3);
    }

    #[test]
    fn long_phoneme_strings_are_cut_at_punctuation() {
        let sentence = "ˈola, kˈomo andˈas. ".repeat(60);
        let pieces = split_tokens(&sentence);
        assert!(pieces.len() >= 2);
        let comma = vocab::token(',').unwrap();
        let period = vocab::token('.').unwrap();
        for piece in &pieces {
            assert!(piece.len() <= MAX_TOKENS);
            assert!(!piece.is_empty());
        }
        for piece in &pieces[..pieces.len() - 1] {
            let last = *piece.last().unwrap();
            assert!(last == comma || last == period);
        }
        let total: usize = pieces.iter().map(Vec::len).sum();
        assert!(total <= vocab::tokens(&sentence).len());
    }

    /// Needs the model (`tts-say download`), then
    /// `cargo test --release -p wawi-tts-kokoro -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs the ~330 MB model on disk"]
    fn the_real_model_speaks() {
        let spec = &crate::model::KOKORO_82M_V1;
        let dir = crate::model::model_dir(spec).unwrap();
        crate::model::verify(spec, &dir).unwrap();
        let (mut voice, loaded, warmed) =
            KokoroVoice::load_warm(&dir, VoiceSettings::default()).unwrap();
        eprintln!("loaded in {loaded:?}, warmed in {warmed:?}");
        let started = Instant::now();
        let audio = voice
            .synthesize("¿Querés que te lea las noticias?")
            .unwrap();
        let took = started.elapsed();
        let seconds = audio.len() as f32 / SAMPLE_RATE as f32;
        eprintln!("{seconds:.2} s of audio in {took:?}");
        assert!((1.0..5.0).contains(&seconds), "{seconds}");
        let peak = audio.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05 && peak <= 1.5, "{peak}");
    }
}
