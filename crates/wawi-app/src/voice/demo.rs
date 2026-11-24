//! `wawi2 --ui-demo[=LOOK]`: Wawi's body with no audio and no brain. A worker
//! thread plays a little script of updates, exactly what the host would send,
//! so every look can be seen (and screenshotted) without speaking.
//!
//! «Ajustes…» opens the real settings window. Its plugins come from a
//! plugin host over a scratch home in the temp folder, with credentials in
//! memory: the demo never starts the owner's plugins nor touches the
//! Keychain. `WAWI2_DEMO_AJUSTES=1` opens the window at start (to see it
//! or take a picture of it).

use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use wawi_body::{BodyLink, Command, Flash, Mood, Update};
use wawi_core::{Event, Registry};
use wawi_plugins::control::PluginControl;
use wawi_plugins::keychain::MemoryStore;
use wawi_plugins::{HostOptions, PluginEvent, PluginHost, PluginSink, Services};
use wawi_tts_kokoro::VoiceSettings;

use super::mac::{QUIT, body_config, catch_ctrl_c};
use super::{DemoLook, exit};

/// One moment of the script: updates to send, then how long to wait.
struct Beat {
    updates: Vec<Update>,
    secs: f64,
}

fn beat(updates: Vec<Update>, secs: f64) -> Beat {
    Beat { updates, secs }
}

fn mood(mood: Mood) -> Update {
    Update::Mood(mood)
}

/// The script for one look.
fn script(look: DemoLook) -> Vec<Beat> {
    match look {
        DemoLook::Idle => vec![beat(vec![mood(Mood::Idle)], 6.0)],
        DemoLook::Listening => vec![beat(vec![mood(Mood::Listening)], 3.0)],
        DemoLook::Transcribing => vec![beat(vec![mood(Mood::Transcribing)], 2.0)],
        DemoLook::Thinking => vec![beat(
            vec![
                Update::Heard("¿Me contás un chiste corto?".into()),
                mood(Mood::Thinking),
            ],
            3.0,
        )],
        DemoLook::Speaking => vec![
            beat(
                vec![
                    Update::Said(
                        "¿Sabés por qué los perros no usan reloj? ¡Porque ya saben la hora del paseo!"
                            .into(),
                    ),
                    mood(Mood::Speaking),
                ],
                5.0,
            ),
            beat(vec![mood(Mood::Idle)], 5.0),
        ],
        DemoLook::Interrupted => vec![
            beat(
                vec![
                    Update::Said("Te cuento lo que pasó ayer en la plaza…".into()),
                    mood(Mood::Speaking),
                ],
                1.5,
            ),
            beat(
                vec![Update::Flash(Flash::Interrupted), mood(Mood::Listening)],
                1.5,
            ),
            beat(vec![mood(Mood::Idle)], 1.0),
        ],
        DemoLook::Error => vec![
            beat(vec![mood(Mood::Thinking)], 1.0),
            beat(
                vec![
                    Update::Flash(Flash::Error),
                    Update::Said("Uy, algo falló y no pude contestarte.".into()),
                    mood(Mood::Speaking),
                ],
                2.5,
            ),
            beat(vec![mood(Mood::Idle)], 1.5),
        ],
        DemoLook::Muted => vec![
            beat(vec![mood(Mood::Idle), Update::Muted(true)], 5.0),
            beat(vec![Update::Muted(false)], 0.5),
        ],
        DemoLook::Sleeping => vec![beat(vec![mood(Mood::Idle)], 16.0)],
    }
}

/// The whole tour, in the order a real turn goes.
fn tour() -> Vec<Beat> {
    [
        DemoLook::Idle,
        DemoLook::Listening,
        DemoLook::Transcribing,
        DemoLook::Thinking,
        DemoLook::Speaking,
        DemoLook::Interrupted,
        DemoLook::Error,
        DemoLook::Muted,
        DemoLook::Sleeping,
    ]
    .into_iter()
    .flat_map(script)
    .collect()
}

/// Waits `secs`, or less if the owner quits. True if it should go on.
fn wait(secs: f64) -> bool {
    let until = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < until {
        if QUIT.load(Ordering::SeqCst) {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
    !QUIT.load(Ordering::SeqCst)
}

fn play(link: &BodyLink, hold: Option<DemoLook>) {
    let mut muted = false;
    loop {
        let beats = match hold {
            Some(look) => script(look),
            None => tour(),
        };
        for beat in beats {
            for update in beat.updates {
                match &update {
                    Update::Mood(mood) => println!("  demo: {}", mood.label()),
                    Update::Flash(flash) => println!("  demo: {}", flash.label()),
                    Update::Muted(on) if *on != muted => {
                        muted = *on;
                        println!(
                            "  demo: {}",
                            if muted {
                                "sin escuchar"
                            } else {
                                "escuchando de nuevo"
                            }
                        );
                    }
                    _ => {}
                }
                link.send(update);
            }
            if !wait(beat.secs) {
                return;
            }
        }
    }
}

/// `pet`: `--mascota DIR`, another pet pack to show instead of the saved one.
pub fn main(hold: Option<DemoLook>, home: Option<PathBuf>, pet: Option<PathBuf>) -> ExitCode {
    catch_ctrl_c();
    println!(
        "Wawi 2: el cuerpo solo, sin audio ni cerebro{}. «Salir» en el menú o Ctrl+C para terminar.",
        hold.map(|look| format!(" (quieta en «{}»)", look.name()))
            .unwrap_or_default()
    );
    let on_command = Box::new(|command, link: &BodyLink| match command {
        Command::SetListening(listening) => link.send(Update::Muted(!listening)),
        Command::Quit => QUIT.store(true, Ordering::SeqCst),
        Command::OpenSettings => link.send(Update::ShowSettings),
    });
    let mut config = body_config(home.as_deref());
    config.pet_dir = pet.or(config.pet_dir);
    let result = wawi_body::run(config, on_command, move |link| {
        link.send(Update::Settings(super::settings::source(
            scratch_plugins(),
            &VoiceSettings::default(),
            None,
            None,
            &wawi_core::Config::default().instinct,
        )));
        if std::env::var_os("WAWI2_DEMO_AJUSTES").is_some() {
            link.send(Update::ShowSettings);
        }
        thread::spawn(move || {
            play(&link, hold);
            link.close();
        });
    });
    let _ = std::fs::remove_dir_all(scratch_home());
    match result {
        Ok(()) => {
            println!("Chau.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(exit::ERROR)
        }
    }
}

struct ScratchSink(mpsc::Sender<PluginEvent>);

impl PluginSink for ScratchSink {
    fn core(&self, _event: Event) {}
    fn plugin(&self, event: PluginEvent) {
        let _ = self.0.send(event);
    }
}

/// A plugin host for the demo's «Ajustes»: a scratch home in the temp
/// folder, credentials in memory, Wawi's own catalog and suggestions.
fn scratch_home() -> PathBuf {
    std::env::temp_dir().join(format!("wawi2-ui-demo-{}", std::process::id()))
}

fn scratch_plugins() -> Option<PluginControl> {
    let home = scratch_home();
    let (tx, rx) = mpsc::channel();
    let (give, take) = mpsc::channel();
    thread::spawn(move || {
        let services = Services {
            store: Arc::new(MemoryStore::new()),
            ..Services::system()
        };
        let mut host = PluginHost::start_with(
            &home,
            None,
            Rc::new(super::clock::SystemClock::new()),
            Arc::new(ScratchSink(tx)),
            HostOptions::default(),
            services,
        );
        let _ = give.send(host.control());
        let mut registry = Registry::default();
        while let Ok(event) = rx.recv() {
            host.on_event(event, &mut registry);
        }
    });
    take.recv_timeout(Duration::from_secs(2)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_look_has_a_script_and_the_tour_has_them_all() {
        for look in DemoLook::ALL {
            assert!(!script(look).is_empty(), "{look:?}");
            assert_eq!(DemoLook::parse(look.name()), Some(look));
        }
        let moods: Vec<Mood> = tour()
            .iter()
            .flat_map(|b| b.updates.iter())
            .filter_map(|u| match u {
                Update::Mood(m) => Some(*m),
                _ => None,
            })
            .collect();
        for mood in [
            Mood::Idle,
            Mood::Listening,
            Mood::Transcribing,
            Mood::Thinking,
            Mood::Speaking,
        ] {
            assert!(moods.contains(&mood), "{mood:?}");
        }
    }
}
