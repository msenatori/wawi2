//! `tts-say`: what Wawi would say, to a WAV file, with timings.
//!
//! ```text
//! tts-say download                       get the Kokoro model once (~330 MB)
//! tts-say [OPTIONS] "texto"              write a WAV (and play it with --play)
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

use wawi_tts_kokoro::model::{self, KOKORO_82M_V1};
use wawi_tts_kokoro::{
    Accent, KokoroVoice, SAMPLE_RATE, Synthesizer, VoiceSettings, speech_pieces,
};

const USAGE: &str = "\
usage:
  tts-say download
      Downloads Kokoro-82M v1.0 and its voices (~330 MB) to $WAWI2_HOME/models/ (default ~/.wawi2).
  tts-say [OPTIONS] TEXT
      Synthesizes TEXT with Wawi's voice and writes a 24 kHz mono WAV.
      --out FILE        where to write it (default: tts-say.wav)
      --play            play it afterwards with afplay (macOS)
      --accent NAME     rioplatense (default), neutral or castilian (Wawi 1's espeak-ng)
      --voice NAME      jf_alpha (default, Wawi 1's) or ef_dora
      --speed X         default 1.05
      --pitch N         semitones, default 3
      --phonemes        only print the phonemes
      --bench           print load time, time to first audio, RTF and memory";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("download") => download(),
        Some("-h" | "--help") | None => Err(USAGE.to_owned()),
        Some(_) => say(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn download() -> Result<(), String> {
    let dir = model::model_dir(&KOKORO_82M_V1).map_err(|e| e.to_string())?;
    model::ensure(&KOKORO_82M_V1, &dir, &mut |line| eprintln!("{line}"))
        .map_err(|e| e.to_string())?;
    eprintln!("Model ready in {}", dir.display());
    Ok(())
}

struct Options {
    text: String,
    out: PathBuf,
    play: bool,
    phonemes_only: bool,
    bench: bool,
    settings: VoiceSettings,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        text: String::new(),
        out: PathBuf::from("tts-say.wav"),
        play: false,
        phonemes_only: false,
        bench: false,
        settings: VoiceSettings::default(),
    };
    let mut words: Vec<&str> = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--out" => options.out = PathBuf::from(value()?),
            "--play" => options.play = true,
            "--phonemes" => options.phonemes_only = true,
            "--bench" => options.bench = true,
            "--voice" => options.settings.voice = value()?,
            "--accent" => {
                let name = value()?;
                options.settings.accent =
                    Accent::parse(&name).ok_or_else(|| format!("unknown accent {name}"))?;
            }
            "--speed" => {
                options.settings.speed = value()?.parse().map_err(|_| "--speed: not a number")?;
            }
            "--pitch" => {
                options.settings.pitch_semitones =
                    value()?.parse().map_err(|_| "--pitch: not a number")?;
            }
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n{USAGE}"));
            }
            _ => words.push(arg),
        }
    }
    options.text = words.join(" ");
    if options.text.trim().is_empty() {
        return Err(USAGE.to_owned());
    }
    Ok(options)
}

fn say(args: &[String]) -> Result<(), String> {
    let options = parse(args)?;
    if options.phonemes_only {
        for sentence in speech_pieces(&options.text) {
            println!(
                "{}",
                wawi_tts_kokoro::g2p::phonemize(&sentence, options.settings.accent)
            );
        }
        return Ok(());
    }
    let dir = model::model_dir(&KOKORO_82M_V1).map_err(|e| e.to_string())?;
    model::verify(&KOKORO_82M_V1, &dir)
        .map_err(|e| format!("{e}\nRun `tts-say download` first."))?;
    let (mut voice, loaded, warmed) = KokoroVoice::load_warm(&dir, options.settings.clone())?;
    let mut audio: Vec<f32> = Vec::new();
    let mut first_audio = None;
    let started = Instant::now();
    for sentence in speech_pieces(&options.text) {
        let sentence_started = Instant::now();
        let samples = voice.synthesize(&sentence)?;
        let took = sentence_started.elapsed();
        first_audio.get_or_insert(took);
        if options.bench {
            eprintln!(
                "  {:>6.0} ms  {:>5.2} s  {}  [{}]",
                took.as_secs_f64() * 1000.0,
                samples.len() as f64 / f64::from(SAMPLE_RATE),
                sentence,
                voice.phonemes(&sentence)
            );
        }
        audio.extend(samples);
    }
    let total = started.elapsed();
    write_wav(&options.out, &audio)?;
    let seconds = audio.len() as f64 / f64::from(SAMPLE_RATE);
    println!("{} ({seconds:.2} s)", options.out.display());
    if options.bench {
        eprintln!(
            "load: {:.0} ms (+ {:.0} ms warm-up)",
            loaded.as_secs_f64() * 1000.0,
            warmed.as_secs_f64() * 1000.0
        );
        if let Some(first) = first_audio {
            eprintln!("first audio: {:.0} ms", first.as_secs_f64() * 1000.0);
        }
        eprintln!(
            "total: {:.0} ms for {seconds:.2} s of audio, RTF {:.3}",
            total.as_secs_f64() * 1000.0,
            total.as_secs_f64() / seconds.max(1e-9)
        );
        if let Some(rss) = peak_rss_mb() {
            eprintln!("peak memory: {rss:.0} MB RSS");
        }
    }
    if options.play {
        let status = Command::new("afplay")
            .arg(&options.out)
            .status()
            .map_err(|e| format!("afplay: {e}"))?;
        if !status.success() {
            return Err(format!("afplay exited with {status}"));
        }
    }
    Ok(())
}

/// 16-bit mono PCM at Kokoro's rate.
fn write_wav(path: &Path, samples: &[f32]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let fail = |e: hound::Error| format!("{}: {e}", path.display());
    let mut writer = hound::WavWriter::create(path, spec).map_err(fail)?;
    for &sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        writer.write_sample(value).map_err(fail)?;
    }
    writer.finalize().map_err(fail)
}

/// Peak resident memory of this process so far, in MB.
#[cfg(unix)]
fn peak_rss_mb() -> Option<f64> {
    // SAFETY: getrusage only writes into the zeroed struct we own.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return None;
    }
    // Bytes on macOS, kilobytes on Linux.
    let raw = usage.ru_maxrss as f64;
    let bytes = if cfg!(target_os = "macos") {
        raw
    } else {
        raw * 1024.0
    };
    Some(bytes / 1_048_576.0)
}

#[cfg(not(unix))]
fn peak_rss_mb() -> Option<f64> {
    None
}
