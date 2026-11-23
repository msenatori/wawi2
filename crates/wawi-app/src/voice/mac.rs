//! Startup and shutdown of the voice program on macOS: permissions, models,
//! memory, brain, live audio and the right-Command key, then the host loop.

use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc as SharedArc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use wawi_audio_mac::hotkey::input_monitoring_granted;
use wawi_audio_mac::{
    AudioError, GateConfig, HotkeyError, LiveAudio, Playback, RightCommandKey, TTS_SAMPLE_RATE,
    ensure_microphone,
};
use wawi_brain_claude::ClaudeBrain;
use wawi_core::Event;
use wawi_memory::Home;
use wawi_stt_parakeet::model::{self as stt_model, PARAKEET_TDT_V3_INT8};
use wawi_stt_parakeet::{Parakeet, ParakeetStt, audio};
use wawi_tts_kokoro::model::{self as tts_model, KOKORO_82M_V1};
use wawi_tts_kokoro::{Accent, KokoroTts, KokoroVoice, VoiceSettings};

use wawi_body::{BodyConfig, BodyLink};
use wawi_plugins::{HostOptions, PluginHost};
use wawi_tools::Toolbox;

use super::body::{Controls, KeyGate, Reporter};
use super::clock::SystemClock;
use super::host::{self, Ended, Host, Input, InputSink, Logs, Parts, Speaker, Until};
use super::mute::NullPlayer;
use super::{VoiceOptions, exit};

/// How long startup waits for plugins before starting the brain.
const PLUGIN_WAIT: Duration = Duration::from_millis(1500);

/// Ctrl+C sets it (a second Ctrl+C exits at once), and so does «Salir».
pub(super) static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    if QUIT.swap(true, Ordering::SeqCst) {
        // SAFETY: async-signal-safe; the second Ctrl+C means "now".
        unsafe { libc::_exit(130) };
    }
}

pub(super) fn catch_ctrl_c() {
    // SAFETY: the handler only touches an atomic (and `_exit`).
    unsafe {
        libc::signal(
            libc::SIGINT,
            on_sigint as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
}

/// How long the WAV run may take, all of it.
const WAV_LIMIT: Duration = Duration::from_secs(180);

/// The body's files: the pet packs, where she stands, the logs folder.
pub(super) fn body_config(home_dir: Option<&Path>) -> BodyConfig {
    BodyConfig::for_home(home_dir)
}

/// With the body (the default), AppKit takes the main thread and the voice
/// program runs on a worker; `--no-ui` keeps everything on this thread.
pub fn main(options: VoiceOptions, home_dir: Option<PathBuf>) -> ExitCode {
    // First, so a Ctrl+C during startup still ends through the clean path.
    catch_ctrl_c();
    if options.no_ui {
        return run(options, home_dir, None);
    }
    let controls = SharedArc::new(Controls::new(&QUIT));
    let code = SharedArc::new(Mutex::new(None));
    let config = body_config(home_dir.as_deref());
    let on_command = {
        let controls = controls.clone();
        Box::new(move |command, link: &BodyLink| controls.apply(command, link))
    };
    let worker_code = code.clone();
    let started = wawi_body::run(config, on_command, move |link| {
        thread::spawn(move || {
            // Closes the body however the worker ends, a panic included.
            struct CloseOnDrop(BodyLink);
            impl Drop for CloseOnDrop {
                fn drop(&mut self) {
                    self.0.close();
                }
            }
            let guard = CloseOnDrop(link.clone());
            let ended = run(options, home_dir, Some((link, controls)));
            if let Ok(mut slot) = worker_code.lock() {
                *slot = Some(ended);
            }
            drop(guard);
        });
    });
    if let Err(error) = started {
        eprintln!("{error}");
        return ExitCode::from(exit::ERROR);
    }
    let ended = code.lock().ok().and_then(|mut slot| slot.take());
    ended.unwrap_or(ExitCode::from(exit::ERROR))
}

fn run(
    options: VoiceOptions,
    home_dir: Option<PathBuf>,
    body: Option<(BodyLink, SharedArc<Controls>)>,
) -> ExitCode {
    println!("Wawi 2: arrancando.");
    if body.is_some() {
        println!("  cuerpo: Wawi en pantalla y en la barra de menú (--no-ui para no mostrarla)");
    }
    let listen_to_mic = options.input_wav.is_none();
    let live_needed = listen_to_mic || !options.mute;

    // 1. Permissions, before loading anything heavy.
    if live_needed {
        match ensure_microphone() {
            Ok(()) => println!("  micrófono: permitido"),
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::from(exit::NO_MIC);
            }
        }
    }
    if listen_to_mic {
        if !input_monitoring_granted() {
            // Asks macOS to list the app under Input Monitoring.
            let _ = RightCommandKey::spawn(|_| {});
            eprintln!("{}", HotkeyError::NoPermission);
            return ExitCode::from(exit::NO_KEY);
        }
        println!("  teclado: permitido (⌘ derecha)");
    }

    // 2. Models: never downloaded silently.
    let (stt_dir, tts_dir) = match model_dirs() {
        Ok(dirs) => dirs,
        Err(missing) => {
            eprintln!("Faltan modelos:\n{missing}");
            eprintln!(
                "Bajalos una vez con (desde la carpeta de wawi2):\n  \
                 cargo run --release -p wawi-stt-parakeet --bin stt-bench -- download\n  \
                 cargo run --release -p wawi-tts-kokoro --bin tts-say -- download"
            );
            return ExitCode::from(exit::NO_MODELS);
        }
    };
    println!("  modelos: Parakeet y Kokoro en su lugar");

    // 3. Load them.
    let parakeet = match Parakeet::load_timed(&stt_dir) {
        Ok((parakeet, took)) => {
            println!("  oído: Parakeet cargado en {} ms", took.as_millis());
            parakeet
        }
        Err(error) => {
            eprintln!("No pude cargar Parakeet: {error}");
            return ExitCode::from(exit::ERROR);
        }
    };
    let settings = VoiceSettings {
        accent: Accent::Castilian,
        ..VoiceSettings::default()
    };
    let voice = match KokoroVoice::load_warm(&tts_dir, settings) {
        Ok((voice, loaded, warmed)) => {
            let s = voice.settings();
            println!(
                "  voz: Kokoro {} (velocidad {}, +{} semitonos, acento {:?}) cargada en {} ms + {} ms de precalentado",
                s.voice,
                s.speed,
                s.pitch_semitones,
                s.accent,
                loaded.as_millis(),
                warmed.as_millis()
            );
            voice
        }
        Err(error) => {
            eprintln!("No pude cargar Kokoro: {error}");
            return ExitCode::from(exit::ERROR);
        }
    };

    // 4. Memory and logs.
    let home = match home_dir.as_deref().map(Home::open) {
        Some(Ok(home)) => {
            println!("  memoria: {}", home.root().display());
            Some(home)
        }
        Some(Err(error)) => {
            println!("  memoria: no la pude abrir ({error}); sigo sin memoria");
            None
        }
        None => {
            println!("  memoria: no encuentro la carpeta personal; sigo sin memoria");
            None
        }
    };
    let logs = match home_dir.as_deref().map(|dir| Logs::open(&dir.join("logs"))) {
        Some(Ok(logs)) => {
            if let Some(dir) = &home_dir {
                println!("  registros: {}", dir.join("logs").display());
            }
            logs
        }
        _ => {
            println!("  registros: no los pude abrir; sigo sin registros");
            Logs::default()
        }
    };

    // 5. The pieces, all posting to one channel.
    let (tx, rx) = mpsc::channel::<Input>();
    let mut config = super::claude_config(home_dir.as_deref());
    // Plugins: found now, connected in the background. Their notes go into
    // the brain's prompt; their tools register as each one connects.
    let plugins = home_dir.as_deref().map(|dir| {
        PluginHost::start(
            dir,
            Some(&dir.join("logs")),
            Rc::new(SystemClock::new()),
            Arc::new(InputSink(tx.clone())),
            HostOptions::default(),
        )
    });
    if let Some(plugins) = &plugins {
        let names = plugins.names();
        println!(
            "  plugins: {}{}",
            if names.is_empty() {
                "ninguno".to_owned()
            } else {
                names.join(", ")
            },
            if plugins.disabled().is_empty() {
                String::new()
            } else {
                format!(" (apagados: {})", plugins.disabled().join(", "))
            }
        );
        for problem in plugins.problems() {
            println!("  plugins: {problem}");
        }
        let notes = plugins.instructions();
        if !notes.is_empty() {
            config.style = format!("{}\n\n{notes}", config.style);
        }
    }
    println!(
        "  cerebro: Claude Code, modelo {}, esfuerzo {}",
        super::describe_choice(config.effective_model()),
        super::describe_choice(config.effective_effort())
    );
    let brain = ClaudeBrain::new(config, Box::new(InputSink(tx.clone())));
    // Her tools, on the system's clock and time zone. Reminders live in
    // <home>/reminders.json.
    let toolbox = Toolbox::open(home.clone(), Rc::new(SystemClock::new()));
    if let Some(warning) = toolbox.warning() {
        println!("  avisos: {warning}");
    }
    let tools = toolbox.tools(Rc::new(InputSink(tx.clone())));
    let specs: Vec<_> = tools.iter().map(|tool| tool.spec().clone()).collect();
    let names: Vec<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
    println!(
        "  herramientas: {} · avisos pendientes: {}",
        names.join(", "),
        toolbox.pending().len()
    );
    let stt = ParakeetStt::spawn(parakeet, Box::new(InputSink(tx.clone())));
    // A transcript that is probably not Spanish is not sent to the brain:
    // the core asks the owner to repeat.
    stt.set_language_guard(true);
    let audio_tx = tx.clone();
    let tts = KokoroTts::spawn(voice, move |chunk| {
        let _ = audio_tx.send(Input::Audio(chunk));
    });

    let sink = Arc::new(InputSink(tx.clone()));
    let live = if live_needed {
        let segments = tx.clone();
        match LiveAudio::start(
            sink.clone(),
            move |samples, sample_rate| {
                let _ = segments.send(Input::Segment {
                    samples,
                    sample_rate,
                });
            },
            GateConfig::default(),
        ) {
            Ok(live) => Some(live),
            Err(AudioError::Microphone(message)) => {
                eprintln!("{message}");
                return ExitCode::from(exit::NO_MIC);
            }
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(exit::ERROR);
            }
        }
    } else {
        None
    };
    let out: Box<dyn Speaker> = match (&live, options.mute) {
        (Some(live), false) => {
            println!(
                "  audio: un motor con cancelación de eco, micrófono a {} Hz",
                live.input_rate()
            );
            Box::new(live.output())
        }
        _ => {
            if let Some(live) = &live {
                println!(
                    "  audio: micrófono a {} Hz; reproducción muda (--mute)",
                    live.input_rate()
                );
            } else {
                println!("  audio: reproducción muda (--mute), sin micrófono");
            }
            Box::new(Playback::new(
                NullPlayer::new(f64::from(TTS_SAMPLE_RATE)),
                sink.clone(),
            ))
        }
    };

    let mut host = Host::new(
        Parts {
            brain: Box::new(brain),
            stt: Box::new(stt),
            tts: Box::new(tts),
            out,
        },
        home,
        Box::new(io::stdout()),
        logs,
    );
    host.install_tools(toolbox, tools);
    if let Some(plugins) = plugins {
        host.install_plugins(plugins);
    }
    // What goes wrong, for autoarreglo (docs/AUTOARREGLO.md). Fire and forget.
    if let Some(dir) = &home_dir {
        host.install_incidents(super::incidents::Incidents::open(dir));
    }
    // Starts claude now, with what turns will carry: the memory and every
    // tool, plugins included. Plugins get up to 1.5 s to connect first, so
    // usually the first process already has all their tools; one that comes
    // later warms a replacement in the background.
    host.warm_up(&rx, PLUGIN_WAIT);
    host.log("startup");
    let controls = body.as_ref().map(|(_, controls)| controls.clone());
    if let Some((link, _)) = body {
        host.set_body(Reporter::new(link));
    }

    let hotkey = if let (Some(live), true) = (&live, listen_to_mic) {
        let ptt = live.ptt();
        let keys = tx.clone();
        let gate = KeyGate::default();
        let menu = controls.clone();
        if let Some(controls) = &controls {
            let release = live.ptt();
            controls.on_release(move || release.set_held(false));
        }
        match RightCommandKey::spawn(move |down| {
            let deaf = menu.as_ref().is_some_and(|controls| controls.deaf());
            match gate.key(down, deaf) {
                Some(true) => ptt.set_held(true),
                Some(false) => {
                    ptt.set_held(false);
                    let _ = keys.send(Input::KeyReleased(Instant::now()));
                }
                None => {}
            }
        }) {
            Ok(hotkey) => Some(hotkey),
            Err(error) => {
                eprintln!("{error}");
                drop(host);
                return ExitCode::from(exit::NO_KEY);
            }
        }
    } else {
        None
    };

    spawn_enter_to_quit(tx.clone());
    // What came due while she was closed, once everything is up.
    host.announce_missed();

    let until = match &options.input_wav {
        None => {
            println!("Listo: mantené ⌘ derecha y hablá. Enter o Ctrl+C para salir.");
            Until::default()
        }
        Some(path) => match feed_wav(path, &tx) {
            Ok(()) => Until {
                turns: Some(1),
                limit: Some(WAV_LIMIT),
            },
            Err(error) => {
                eprintln!("{error}");
                drop(host);
                return ExitCode::from(exit::ERROR);
            }
        },
    };
    drop(tx);

    let ended = host::run(&mut host, &rx, &QUIT, until);
    host.log(&format!("ended: {ended:?}"));
    match ended {
        Ended::TimeLimit => println!("Se acabó el tiempo de la prueba."),
        Ended::Signal | Ended::Quit => println!("Cerrando."),
        Ended::TurnsDone | Ended::Disconnected => {}
    }

    // 6. Shutdown: no more keys, cut the audio, end claude and the workers,
    // then the engine.
    drop(hotkey);
    host.shutdown();
    drop(host);
    drop(live);
    println!("Chau: audio cerrado y Claude Code cerrado.");
    if ended == Ended::TimeLimit {
        ExitCode::from(exit::ERROR)
    } else {
        ExitCode::SUCCESS
    }
}

fn model_dirs() -> Result<(PathBuf, PathBuf), String> {
    let mut missing = String::new();
    let stt = stt_model::model_dir(&PARAKEET_TDT_V3_INT8).map_err(|e| e.to_string())?;
    if let Err(error) = stt_model::verify(&PARAKEET_TDT_V3_INT8, &stt) {
        missing.push_str(&format!("  Parakeet en {}: {error}\n", stt.display()));
    }
    let tts = tts_model::model_dir(&KOKORO_82M_V1).map_err(|e| e.to_string())?;
    if let Err(error) = tts_model::verify(&KOKORO_82M_V1, &tts) {
        missing.push_str(&format!("  Kokoro en {}: {error}\n", tts.display()));
    }
    if missing.is_empty() {
        Ok((stt, tts))
    } else {
        Err(missing)
    }
}

/// Enter ends the session. A closed stdin (a pipe) does not.
fn spawn_enter_to_quit(tx: Sender<Input>) {
    thread::spawn(move || {
        let mut line = String::new();
        if let Ok(read) = io::stdin().lock().read_line(&mut line)
            && read > 0
        {
            let _ = tx.send(Input::Quit);
        }
    });
}

/// The WAV, as if the owner had held the key, said it and let go.
fn feed_wav(path: &Path, tx: &Sender<Input>) -> Result<(), String> {
    let clip =
        audio::read_wav(path).map_err(|e| format!("No puedo leer {}: {e}", path.display()))?;
    println!(
        "Listo: le paso {} ({:.1} s) como si lo dijeras.",
        path.display(),
        clip.seconds()
    );
    // Gives claude its usual head start, as an owner would after "Listo".
    thread::sleep(Duration::from_millis(1500));
    let send = |input| {
        tx.send(input)
            .map_err(|_| "el programa ya se cerró".to_owned())
    };
    send(Input::Core(Event::SpeechStarted))?;
    send(Input::Segment {
        samples: clip.samples,
        sample_rate: clip.sample_rate,
    })?;
    send(Input::KeyReleased(Instant::now()))?;
    send(Input::Core(Event::SpeechEnded))
}
