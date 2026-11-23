//! `voice-record`: guides the owner through the speech-to-text test phrases.
//! It shows one phrase at a time; hold right Command, read it, release. Each
//! take is saved as `name.wav` plus `name.txt` (the reference transcript), the
//! pairs `stt-bench run DIR` expects. macOS only.
//!
//! `voice-record [DIR] [PHRASE...]`: with phrases (number or name, like `3` or
//! `03_hora`) it records only those, to redo some takes. A take that already
//! exists is moved to `DIR/anteriores/` before being replaced, never lost.

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    mac::main()
}

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("voice-record solo funciona en macOS.");
    std::process::ExitCode::FAILURE
}

#[cfg(target_os = "macos")]
mod mac {
    use std::io::BufRead;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::mpsc::{self, Sender};
    use std::sync::{Arc, Mutex};

    use wawi_audio_mac::analysis::{peak_db, rms_db};
    use wawi_audio_mac::{
        GateConfig, LiveAudio, RightCommandKey, microphone_mode, show_microphone_modes,
    };
    use wawi_core::{Event, EventSink};

    const EXIT_NO_MIC: u8 = 3;
    const EXIT_NO_KEY: u8 = 5;
    const EXIT_ERROR: u8 = 1;
    /// Takes quieter than this are almost surely the mic not hearing the owner.
    const TOO_QUIET_DB: f64 = -50.0;
    const PHRASES: &str = include_str!("../../../wawi-stt-parakeet/bench/phrases.tsv");

    enum Msg {
        Core(Event),
        Segment(Vec<f32>, u32),
        Line(String),
    }

    struct ChannelSink(Mutex<Sender<Msg>>);

    impl EventSink for ChannelSink {
        fn send(&self, event: Event) {
            let _ = self
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .send(Msg::Core(event));
        }
    }

    struct Phrase {
        name: String,
        text: String,
    }

    /// `name<TAB>voice<TAB>text` lines; `#` lines are comments.
    fn phrases() -> Vec<Phrase> {
        PHRASES
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let name = fields.next()?.trim();
                let _voice = fields.next()?;
                let text = fields.next()?.trim();
                Some(Phrase {
                    name: name.to_owned(),
                    text: text.to_owned(),
                })
            })
            .collect()
    }

    /// The phrase `arg` names: its number (`3`, `03`) or its name (`03_hora`).
    fn find(phrases: &[Phrase], arg: &str) -> Option<usize> {
        phrases.iter().position(|p| {
            let number = p.name.split('_').next().unwrap_or_default();
            let same_number = match (number.parse::<u32>(), arg.parse::<u32>()) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            };
            p.name == arg || same_number
        })
    }

    /// Output folder and the phrases to record, from the arguments.
    fn parse_args(all: Vec<Phrase>) -> Result<(PathBuf, Vec<Phrase>), String> {
        let mut dir = None;
        let mut chosen = Vec::new();
        for arg in std::env::args().skip(1) {
            if let Some(index) = find(&all, &arg) {
                if !chosen.contains(&index) {
                    chosen.push(index);
                }
            } else if dir.is_none() {
                dir = Some(PathBuf::from(arg));
            } else {
                return Err(format!(
                    "No conozco la frase «{arg}». Usá el número (3) o el nombre (03_hora)."
                ));
            }
        }
        let dir = dir.unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
            home.join("Desktop").join("wawi-voz")
        });
        if chosen.is_empty() {
            return Ok((dir, all));
        }
        chosen.sort_unstable();
        let mut all: Vec<Option<Phrase>> = all.into_iter().map(Some).collect();
        let phrases = chosen.into_iter().filter_map(|i| all[i].take()).collect();
        Ok((dir, phrases))
    }

    /// Moves an existing take to `dir/anteriores/name.N.wav` (and `.txt`),
    /// with the first free N.
    fn keep_previous(dir: &Path, name: &str) -> Result<Option<PathBuf>, String> {
        let wav = dir.join(format!("{name}.wav"));
        if !wav.exists() {
            return Ok(None);
        }
        let old = dir.join("anteriores");
        std::fs::create_dir_all(&old).map_err(|e| e.to_string())?;
        let mut n = 1;
        let target = loop {
            let candidate = old.join(format!("{name}.{n}.wav"));
            if !candidate.exists() {
                break candidate;
            }
            n += 1;
        };
        std::fs::rename(&wav, &target).map_err(|e| e.to_string())?;
        let txt = dir.join(format!("{name}.txt"));
        if txt.exists() {
            let _ = std::fs::rename(&txt, old.join(format!("{name}.{n}.txt")));
        }
        Ok(Some(target))
    }

    fn save(dir: &Path, phrase: &Phrase, samples: &[f32], rate: u32) -> Result<(), String> {
        if let Some(previous) = keep_previous(dir, &phrase.name)? {
            println!("  la toma anterior quedó en {}", previous.display());
        }
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let wav = dir.join(format!("{}.wav", phrase.name));
        let mut writer = hound::WavWriter::create(&wav, spec).map_err(|e| e.to_string())?;
        for sample in samples {
            let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
            writer.write_sample(value).map_err(|e| e.to_string())?;
        }
        writer.finalize().map_err(|e| e.to_string())?;
        std::fs::write(
            dir.join(format!("{}.txt", phrase.name)),
            format!("{}\n", phrase.text),
        )
        .map_err(|e| e.to_string())
    }

    fn prompt(index: usize, total: usize, phrase: &Phrase) {
        println!(
            "\n[{}/{}] Leé en voz alta, con tu forma de hablar:",
            index + 1,
            total
        );
        println!("    «{}»", phrase.text);
        println!("Mantené ⌘ derecha mientras hablás. Podés empezar a hablar apenas la apretás.");
        println!("(s + Enter salta esta frase, r + Enter repite la anterior, q + Enter termina)");
    }

    pub fn main() -> ExitCode {
        let (dir, phrases) = match parse_args(phrases()) {
            Ok(parsed) => parsed,
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::from(EXIT_ERROR);
            }
        };
        if let Err(error) = std::fs::create_dir_all(&dir) {
            eprintln!("No pude crear {}: {error}", dir.display());
            return ExitCode::from(EXIT_ERROR);
        }
        println!(
            "voice-record: {} frases, se guardan en {}",
            phrases.len(),
            dir.display()
        );
        println!(
            "Para rehacer algunas: voice-record {} 3 05_acordate (las anteriores van a anteriores/).",
            dir.display()
        );

        let (tx, rx) = mpsc::channel::<Msg>();
        let sink = Arc::new(ChannelSink(Mutex::new(tx.clone())));
        let segments = Mutex::new(tx.clone());
        let live = match LiveAudio::start(
            sink,
            move |samples, rate| {
                let _ = segments
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .send(Msg::Segment(samples, rate));
            },
            GateConfig::default(),
        ) {
            Ok(live) => live,
            Err(wawi_audio_mac::AudioError::Microphone(message)) => {
                eprintln!("{message}");
                return ExitCode::from(EXIT_NO_MIC);
            }
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(EXIT_ERROR);
            }
        };
        if let Some(mode) = microphone_mode() {
            println!("Modo del micrófono: {mode}. m + Enter abre el selector de macOS.");
        }
        let ptt = live.ptt();
        let hotkey = match RightCommandKey::spawn(move |down| ptt.set_held(down)) {
            Ok(hotkey) => hotkey,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(EXIT_NO_KEY);
            }
        };

        let lines = tx.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if lines.send(Msg::Line(line)).is_err() {
                    break;
                }
            }
        });

        let mut index = 0;
        let mut saved = 0;
        prompt(index, phrases.len(), &phrases[index]);
        while index < phrases.len() {
            let Ok(msg) = rx.recv() else { break };
            match msg {
                Msg::Core(Event::SpeechStarted) => println!("  grabando..."),
                Msg::Segment(samples, rate) => {
                    if samples.is_empty() {
                        println!("  toque corto, no grabé nada. Probá de nuevo.");
                        continue;
                    }
                    let level = rms_db(&samples);
                    let seconds = samples.len() as f64 / f64::from(rate);
                    if level < TOO_QUIET_DB {
                        println!(
                            "  muy bajo ({level:.1} dBFS): parece que el micrófono no te oyó. Probá de nuevo."
                        );
                        continue;
                    }
                    let phrase = &phrases[index];
                    match save(&dir, phrase, &samples, rate) {
                        Ok(()) => {
                            saved += 1;
                            println!(
                                "  guardada: {seconds:.1} s, nivel {level:.1} dBFS (pico {:.1})",
                                peak_db(&samples)
                            );
                            index += 1;
                            if index < phrases.len() {
                                prompt(index, phrases.len(), &phrases[index]);
                            }
                        }
                        Err(error) => println!("  no pude guardarla: {error}. Probá de nuevo."),
                    }
                }
                Msg::Line(line) => match line.trim() {
                    "q" => break,
                    "m" => show_microphone_modes(),
                    "s" => {
                        println!("  salteada.");
                        index += 1;
                        if index < phrases.len() {
                            prompt(index, phrases.len(), &phrases[index]);
                        }
                    }
                    "r" if index > 0 => {
                        index -= 1;
                        println!("  repetimos la anterior.");
                        prompt(index, phrases.len(), &phrases[index]);
                    }
                    _ => {}
                },
                Msg::Core(_) => {}
            }
        }

        drop(hotkey);
        drop(live);
        println!("\nListo: {saved} frases en {}.", dir.display());
        println!("Avisale a Claude y corre la comparación.");
        ExitCode::SUCCESS
    }
}
