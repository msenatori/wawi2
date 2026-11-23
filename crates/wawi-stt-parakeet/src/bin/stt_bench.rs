//! `stt-bench`: speed, latency, memory and word error rate of speech-to-text
//! engines on a folder of WAVs, each with its reference transcript next to it
//! (`name.wav` + `name.txt`).
//!
//! ```text
//! stt-bench download                         get the Parakeet model once
//! stt-bench make-synthetic [--out DIR]       synthetic test set with macOS `say`
//! stt-bench run DIR... [--json OUT]          Parakeet on every WAV, then the report
//! stt-bench report RESULT.json...            score saved results, side by side
//! ```
//!
//! `report` takes results from any engine in the same JSON shape, so another
//! engine (Wawi 1's Whisper, for instance) is scored by exactly the same code.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

use serde_json::{Value, json};
use wawi_stt_parakeet::audio::{self, Clip, MODEL_RATE};
use wawi_stt_parakeet::model::{self, PARAKEET_TDT_V3_INT8};
use wawi_stt_parakeet::wer::{self, WordErrors};
use wawi_stt_parakeet::{Parakeet, Recognizer};

const USAGE: &str = "\
usage:
  stt-bench download
      Downloads Parakeet TDT 0.6B v3 int8 (~670 MB) to $WAWI2_HOME/models/ (default ~/.wawi2).
  stt-bench make-synthetic [--phrases FILE] [--out DIR] [--snr DB]
      macOS only. Speaks every phrase with `say` at 16 kHz into DIR/clean, and the same
      audio with pink noise at DB dB SNR (default 15) into DIR/noisy, each WAV with its .txt.
  stt-bench run DIR... [--json OUT] [--model DIR]
      Transcribes every name.wav that has a name.txt beside it, with Parakeet, and reports.
  stt-bench report RESULT.json...
      Scores saved results (from `run --json` or any engine that writes the same shape).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("download") => download(),
        Some("make-synthetic") => make_synthetic(&args[1..]),
        Some("run") => run(&args[1..]),
        Some("report") => report_files(&args[1..]),
        _ => Err(USAGE.to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

type Parsed = (Vec<String>, BTreeMap<String, String>);

/// Splits `--flag value` options from positional arguments.
fn options(args: &[String], flags: &[&str]) -> Result<Parsed, String> {
    let mut positional = Vec::new();
    let mut found = BTreeMap::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(flag) = arg.strip_prefix("--") {
            if !flags.contains(&flag) {
                return Err(format!("unknown option --{flag}\n{USAGE}"));
            }
            let value = iter.next().ok_or(format!("--{flag} needs a value"))?;
            found.insert(flag.to_owned(), value.clone());
        } else {
            positional.push(arg.clone());
        }
    }
    Ok((positional, found))
}

fn download() -> Result<(), String> {
    let dir = model::model_dir(&PARAKEET_TDT_V3_INT8).map_err(|e| e.to_string())?;
    model::ensure(&PARAKEET_TDT_V3_INT8, &dir, &mut |line| eprintln!("{line}"))
        .map_err(|e| e.to_string())?;
    eprintln!(
        "Model ready in {} (source {}, license {}).",
        dir.display(),
        PARAKEET_TDT_V3_INT8.base_url,
        PARAKEET_TDT_V3_INT8.license
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Synthetic test set

fn make_synthetic(args: &[String]) -> Result<(), String> {
    let (_, opts) = options(args, &["phrases", "out", "snr"])?;
    let phrases = opts
        .get("phrases")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/phrases.tsv"));
    let out = opts
        .get("out")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("wawi-stt-bench"));
    let snr: f64 = match opts.get("snr") {
        Some(value) => value.parse().map_err(|_| "--snr DB, a number")?,
        None => 15.0,
    };
    let text = fs::read_to_string(&phrases).map_err(|e| format!("{}: {e}", phrases.display()))?;
    let clean_dir = out.join("clean");
    let noisy_dir = out.join("noisy");
    fs::create_dir_all(&clean_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&noisy_dir).map_err(|e| e.to_string())?;
    let spoken = out.join(".say.wav");
    let mut noise = PinkNoise::new(0x5eed_1234);
    let mut count = 0;
    for line in text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let mut fields = line.splitn(3, '\t');
        let (Some(name), Some(voice), Some(sentence)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(format!("bad line (want name<TAB>voice<TAB>text): {line}"));
        };
        let status = Command::new("say")
            .args(["-v", voice, "-o"])
            .arg(&spoken)
            .args([
                "--file-format=WAVE",
                &format!("--data-format=LEI16@{MODEL_RATE}"),
            ])
            .arg(sentence)
            .status()
            .map_err(|e| format!("cannot run `say` (macOS only): {e}"))?;
        if !status.success() {
            return Err(format!("`say -v {voice}` failed; is the voice installed?"));
        }
        let speech = audio::read_wav(&spoken)?.to_model_rate();
        // Some silence around the words, as a voice detector would leave it.
        let pad = vec![0.0_f32; MODEL_RATE as usize * 3 / 10];
        let clean: Vec<f32> = pad.iter().chain(&speech).chain(&pad).copied().collect();
        let noisy = mix_at_snr(&clean, rms(&speech), snr, &mut noise);
        for (dir, samples) in [(&clean_dir, clean), (&noisy_dir, noisy)] {
            let clip = Clip {
                samples,
                sample_rate: MODEL_RATE,
            };
            audio::write_wav(&dir.join(format!("{name}.wav")), &clip)?;
            fs::write(dir.join(format!("{name}.txt")), format!("{sentence}\n"))
                .map_err(|e| e.to_string())?;
        }
        count += 1;
    }
    let _ = fs::remove_file(&spoken);
    eprintln!(
        "{count} phrases in {} and {} (pink noise at {snr} dB SNR).\n\
         Synthetic voices: real-voice WER needs your own recordings.",
        clean_dir.display(),
        noisy_dir.display()
    );
    Ok(())
}

/// `clean` plus noise whose RMS sits `snr_db` below `speech_rms`.
fn mix_at_snr(clean: &[f32], speech_rms: f64, snr_db: f64, noise: &mut PinkNoise) -> Vec<f32> {
    let raw: Vec<f32> = (0..clean.len()).map(|_| noise.next() as f32).collect();
    let gain = speech_rms / 10_f64.powf(snr_db / 20.0) / rms(&raw).max(1e-12);
    clean
        .iter()
        .zip(&raw)
        .map(|(&s, &n)| s + (f64::from(n) * gain) as f32)
        .collect()
}

fn rms(samples: &[f32]) -> f64 {
    let sum: f64 = samples.iter().map(|&s| f64::from(s).powi(2)).sum();
    (sum / samples.len().max(1) as f64).sqrt()
}

/// Deterministic pink noise (Paul Kellet's filter over xorshift white noise),
/// so every run mixes exactly the same noise. Not normalized: callers scale
/// it by its measured RMS.
struct PinkNoise {
    state: u64,
    b: [f64; 7],
}

impl PinkNoise {
    fn new(seed: u64) -> Self {
        Self {
            state: seed.max(1),
            b: [0.0; 7],
        }
    }

    fn white(&mut self) -> f64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        (self.state >> 11) as f64 / (1_u64 << 53) as f64 * 2.0 - 1.0
    }

    fn next(&mut self) -> f64 {
        let w = self.white();
        let b = &mut self.b;
        b[0] = 0.99886 * b[0] + w * 0.0555179;
        b[1] = 0.99332 * b[1] + w * 0.0750759;
        b[2] = 0.96900 * b[2] + w * 0.1538520;
        b[3] = 0.86650 * b[3] + w * 0.3104856;
        b[4] = 0.55000 * b[4] + w * 0.5329522;
        b[5] = -0.7616 * b[5] - w * 0.0168980;
        let pink = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
        b[6] = w * 0.115926;
        pink
    }
}

// ---------------------------------------------------------------------------
// Running Parakeet

/// Every `name.wav` with a `name.txt` beside it, sorted, from each folder.
fn test_files(dirs: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for dir in dirs {
        let mut found: Vec<PathBuf> = fs::read_dir(dir)
            .map_err(|e| format!("{dir}: {e}"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")))
            .filter(|p| p.with_extension("txt").is_file())
            .collect();
        found.sort();
        if found.is_empty() {
            return Err(format!("{dir}: no name.wav + name.txt pairs"));
        }
        files.extend(found);
    }
    Ok(files)
}

fn run(args: &[String]) -> Result<(), String> {
    let (dirs, opts) = options(args, &["json", "model"])?;
    if dirs.is_empty() {
        return Err(USAGE.to_owned());
    }
    let files = test_files(&dirs)?;
    let model_dir = match opts.get("model") {
        Some(dir) => PathBuf::from(dir),
        None => model::model_dir(&PARAKEET_TDT_V3_INT8).map_err(|e| e.to_string())?,
    };
    model::verify(&PARAKEET_TDT_V3_INT8, &model_dir)
        .map_err(|e| format!("{e}\nGet the model first: stt-bench download"))?;
    let rss_before = peak_rss_mb();
    let (mut parakeet, load) = Parakeet::load_timed(&model_dir)?;
    let rss_loaded = peak_rss_mb();
    eprintln!("model loaded in {} ms", load.as_millis());

    // Warm-up: the first run pays for allocations and lazy init. Not timed.
    let first = audio::read_wav(&files[0])?;
    parakeet.recognize(&first.to_model_rate())?;

    let mut results = Vec::new();
    for path in &files {
        let clip = audio::read_wav(path)?;
        // Timed from the clip as read, resampling included.
        let started = Instant::now();
        let text = parakeet.recognize(&clip.to_model_rate())?;
        let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "{:>7.0} ms  {:>5.2} s  {}: {text}",
            latency_ms,
            clip.seconds(),
            short_name(path)
        );
        results.push(json!({
            "wav": path.canonicalize().unwrap_or(path.clone()),
            "seconds": clip.seconds(),
            "latency_ms": latency_ms,
            "text": text,
        }));
    }
    let result = json!({
        "engine": "Parakeet TDT 0.6B v3 int8 (CPU)",
        "load_ms": load.as_secs_f64() * 1000.0,
        "rss_before_load_mb": rss_before,
        "rss_after_load_mb": rss_loaded,
        "peak_rss_mb": peak_rss_mb(),
        "files": results,
    });
    if let Some(out) = opts.get("json") {
        let text = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
        fs::write(out, text).map_err(|e| format!("{out}: {e}"))?;
        eprintln!("results in {out}");
    }
    report(&[result])
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

// ---------------------------------------------------------------------------
// Scoring

fn report_files(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Err(USAGE.to_owned());
    }
    let mut results = Vec::new();
    for path in paths {
        let text = fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        results.push(serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?);
    }
    report(&results)
}

struct Scored {
    name: String,
    set: String,
    seconds: f64,
    latency_ms: f64,
    reference: String,
    text: String,
    errors: WordErrors,
}

struct Summary {
    engine: String,
    load_ms: Option<f64>,
    peak_rss_mb: Option<f64>,
    files: Vec<Scored>,
}

impl Summary {
    fn latencies(&self, low: f64, high: f64) -> Vec<f64> {
        self.files
            .iter()
            .filter(|f| f.seconds >= low && f.seconds < high)
            .map(|f| f.latency_ms)
            .collect()
    }

    fn errors_in(&self, set: &str) -> WordErrors {
        let mut total = WordErrors::default();
        for file in self.files.iter().filter(|f| f.set == set) {
            total.add(file.errors);
        }
        total
    }

    fn real_time_factor(&self) -> Option<f64> {
        let audio: f64 = self.files.iter().map(|f| f.seconds).sum();
        let compute: f64 = self.files.iter().map(|f| f.latency_ms / 1000.0).sum();
        (audio > 0.0).then(|| compute / audio)
    }
}

fn score(result: &Value) -> Result<Summary, String> {
    let files = result["files"].as_array().ok_or("results without files")?;
    let mut scored = Vec::new();
    for file in files {
        let wav = PathBuf::from(file["wav"].as_str().ok_or("file without wav")?);
        let txt = wav.with_extension("txt");
        let reference = fs::read_to_string(&txt).map_err(|e| format!("{}: {e}", txt.display()))?;
        let text = file["text"].as_str().unwrap_or_default().to_owned();
        scored.push(Scored {
            name: short_name(&wav),
            set: wav
                .parent()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            seconds: file["seconds"].as_f64().unwrap_or(0.0),
            latency_ms: file["latency_ms"].as_f64().unwrap_or(0.0),
            errors: wer::word_errors(&reference, &text),
            reference: reference.trim().to_owned(),
            text,
        });
    }
    Ok(Summary {
        engine: result["engine"].as_str().unwrap_or("?").to_owned(),
        load_ms: result["load_ms"].as_f64(),
        peak_rss_mb: result["peak_rss_mb"].as_f64(),
        files: scored,
    })
}

fn short_name(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Nearest-rank percentile.
fn percentile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    Some(sorted[rank.min(sorted.len()) - 1])
}

fn fmt(value: Option<f64>, digits: usize, unit: &str) -> String {
    match value {
        Some(v) => format!("{v:.digits$}{unit}"),
        None => "n/a".into(),
    }
}

/// Short-utterance buckets, by audio length in seconds.
const BUCKETS: [(&str, f64, f64); 3] = [
    ("< 2 s", 0.0, 2.0),
    ("2-5 s", 2.0, 5.0),
    ("> 5 s", 5.0, f64::MAX),
];

fn report(results: &[Value]) -> Result<(), String> {
    let summaries: Vec<Summary> = results.iter().map(score).collect::<Result<_, _>>()?;
    let mut sets: Vec<String> = summaries
        .iter()
        .flat_map(|s| s.files.iter().map(|f| f.set.clone()))
        .collect();
    sets.sort();
    sets.dedup();

    let mut rows: Vec<(String, Vec<String>)> = Vec::new();
    let mut row = |label: String, cell: &dyn Fn(&Summary) -> String| {
        rows.push((label, summaries.iter().map(cell).collect()));
    };
    row("files".into(), &|s| s.files.len().to_string());
    row("load time".into(), &|s| fmt(s.load_ms, 0, " ms"));
    row("peak RSS".into(), &|s| fmt(s.peak_rss_mb, 0, " MB"));
    row("RTF (compute / audio)".into(), &|s| {
        fmt(s.real_time_factor(), 3, "")
    });
    for (label, low, high) in BUCKETS {
        row(format!("latency p50, {label}"), &|s| {
            let l = s.latencies(low, high);
            format!("{} (n={})", fmt(percentile(&l, 50.0), 0, " ms"), l.len())
        });
        row(format!("latency p90, {label}"), &|s| {
            fmt(percentile(&s.latencies(low, high), 90.0), 0, " ms")
        });
    }
    for set in &sets {
        row(format!("WER {set}"), &|s| {
            let total = s.errors_in(set);
            format!(
                "{:.1}% ({}/{})",
                total.rate() * 100.0,
                total.errors,
                total.reference_words
            )
        });
    }
    print_table(&summaries, &rows);

    println!("\nFiles with errors (normalized: lowercase, no punctuation, accents kept):");
    for (index, file) in summaries[0].files.iter().enumerate() {
        let all: Vec<Option<&Scored>> = summaries.iter().map(|s| s.files.get(index)).collect();
        if all.iter().flatten().all(|f| f.errors.errors == 0) {
            continue;
        }
        println!(
            "\n{}/{} ({:.1} s)  ref: {}",
            file.set, file.name, file.seconds, file.reference
        );
        for (summary, scored) in summaries.iter().zip(all) {
            let engine: String = summary.engine.chars().take(24).collect();
            match scored {
                Some(f) => println!(
                    "  {engine:24} {:>2} err {:>5.0} ms: {}",
                    f.errors.errors, f.latency_ms, f.text
                ),
                None => println!("  {engine:24} (missing)"),
            }
        }
    }
    Ok(())
}

fn print_table(summaries: &[Summary], rows: &[(String, Vec<String>)]) {
    let width = rows
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0);
    let names: Vec<String> = summaries
        .iter()
        .map(|s| s.engine.chars().take(40).collect())
        .collect();
    let columns: Vec<usize> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            rows.iter()
                .map(|(_, v)| v[i].chars().count())
                .chain([name.chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    println!();
    print!("| {:width$} |", "");
    for (name, w) in names.iter().zip(&columns) {
        print!(" {name:w$} |");
    }
    println!();
    print!("|{}|", "-".repeat(width + 2));
    for w in &columns {
        print!("{}|", "-".repeat(w + 2));
    }
    println!();
    for (label, values) in rows {
        print!("| {label:width$} |");
        for (v, w) in values.iter().zip(&columns) {
            print!(" {v:w$} |");
        }
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank() {
        let values = [5.0, 1.0, 3.0, 2.0, 4.0];
        assert_eq!(percentile(&values, 50.0), Some(3.0));
        assert_eq!(percentile(&values, 90.0), Some(5.0));
        assert_eq!(percentile(&[], 50.0), None);
    }

    #[test]
    fn noise_is_mixed_at_the_requested_snr() {
        let speech: Vec<f32> = (0..32_000)
            .map(|i| (0.2 * (i as f64 * 0.05).sin()) as f32)
            .collect();
        let noisy = mix_at_snr(&speech, rms(&speech), 15.0, &mut PinkNoise::new(1));
        let noise: Vec<f32> = noisy.iter().zip(&speech).map(|(n, s)| n - s).collect();
        let snr = 20.0 * (rms(&speech) / rms(&noise)).log10();
        assert!((snr - 15.0).abs() < 0.01, "{snr}");
    }
}
