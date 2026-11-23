//! `ptt-test`: hold right Command, speak, release, and it plays back what it
//! heard, with its length and level. Pressing again while it plays cuts the
//! playback, as the core does. Wires capture, hotkey and playback without the
//! core, so the owner can check them by ear. macOS only.

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    mac::main()
}

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("ptt-test solo funciona en macOS.");
    std::process::ExitCode::FAILURE
}

#[cfg(target_os = "macos")]
mod mac {
    use std::io::BufRead;
    use std::process::ExitCode;
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use wawi_audio_mac::analysis::{peak_db, rms_db};
    use wawi_audio_mac::devices::default_output;
    use wawi_audio_mac::dsp::resample;
    use wawi_audio_mac::{GateConfig, LiveAudio, RightCommandKey, TTS_SAMPLE_RATE};
    use wawi_core::{AudioOut, Event, EventSink, SpeechId};

    const EXIT_NO_MIC: u8 = 3;
    const EXIT_NO_KEY: u8 = 5;
    const EXIT_ERROR: u8 = 1;

    enum Msg {
        Core(Event),
        Segment(Vec<f32>, u32),
        Quit,
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

    fn volume() -> Option<f32> {
        default_output().and_then(|d| d.volume)
    }

    fn percent(v: Option<f32>) -> String {
        v.map_or("?".to_owned(), |v| format!("{:.0}%", v * 100.0))
    }

    pub fn main() -> ExitCode {
        println!("ptt-test: mantené apretada la tecla ⌘ derecha, hablá y soltala.");
        println!("Te devuelve lo que dijiste. Si apretás mientras suena, la corta.");
        println!("Enter para salir.\n");

        let start_volume = volume();
        if let Some(device) = default_output() {
            println!(
                "Salida: {} [{}], volumen {}",
                device.name,
                device.output_kind.label(),
                percent(start_volume)
            );
        }

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
        println!(
            "Micrófono con cancelación de eco a {} Hz.",
            live.input_rate()
        );
        if let Some(mode) = wawi_audio_mac::microphone_mode() {
            println!(
                "Modo del micrófono: {mode} (se cambia en Centro de control > Modo de micrófono)."
            );
        }
        let after_start = volume();
        if after_start != start_volume {
            println!(
                "OJO: el volumen cambió al arrancar el audio ({} → {}). Este programa no lo toca.",
                percent(start_volume),
                percent(after_start)
            );
        }

        let ptt = live.ptt();
        let hotkey = match RightCommandKey::spawn(move |down| ptt.set_held(down)) {
            Ok(hotkey) => hotkey,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(EXIT_NO_KEY);
            }
        };
        println!("Listo. Mantené ⌘ derecha para hablar.\n");

        let quit = tx.clone();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = std::io::stdin().lock().read_line(&mut line);
            let _ = quit.send(Msg::Quit);
        });

        let mut out = live.output();
        let mut pending: Option<Vec<f32>> = None;
        let mut next = 0;
        let mut last_volume = after_start;
        loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Msg::Core(Event::SpeechStarted)) => {
                    if out.is_playing() {
                        out.stop();
                        println!("  (cortada)");
                    }
                    println!("escuchando...");
                }
                Ok(Msg::Segment(samples, rate)) => {
                    if samples.is_empty() {
                        println!("toque corto: no se manda nada.");
                    } else {
                        println!(
                            "grabé {:.2} s, nivel {:.1} dBFS (pico {:.1} dBFS)",
                            samples.len() as f64 / f64::from(rate),
                            rms_db(&samples),
                            peak_db(&samples)
                        );
                        pending = Some(resample(
                            &samples,
                            f64::from(rate),
                            f64::from(TTS_SAMPLE_RATE),
                        ));
                    }
                }
                Ok(Msg::Core(Event::SpeechEnded)) => {
                    if let Some(audio) = pending.take() {
                        next += 1;
                        out.play(SpeechId(next), &audio);
                        println!("reproduciendo #{next}");
                    }
                }
                Ok(Msg::Core(Event::PlaybackFinished { up_to })) => {
                    println!("terminó #{}", up_to.0);
                }
                Ok(Msg::Core(_)) | Err(RecvTimeoutError::Timeout) => {}
                Ok(Msg::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            }
            let now = volume();
            if now != last_volume {
                println!(
                    "OJO: el volumen del sistema cambió ({} → {}). Este programa no lo toca.",
                    percent(last_volume),
                    percent(now)
                );
                last_volume = now;
            }
        }

        drop(hotkey);
        drop(out);
        drop(live);
        let end_volume = volume();
        println!(
            "Chau. Volumen al empezar {}, al terminar {}.",
            percent(start_volume),
            percent(end_volume)
        );
        ExitCode::SUCCESS
    }
}
