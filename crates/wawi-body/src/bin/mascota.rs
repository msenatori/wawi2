//! `mascota`: tools for making pets for Wawi (see `docs/MASCOTAS.md`).
//!
//!   mascota validar CARPETA          revisa una mascota y dice qué falta
//!   mascota nueva ID [--nombre N]    crea una mascota nueva con dibujos de ejemplo
//!   mascota ver CARPETA [NOMBRE]     la muestra en pantalla (sin audio ni cerebro)
//!   mascota lista                    las mascotas instaladas

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use wawi_body::pack::{self, Look, Pack};
use wawi_body::{BodyConfig, BodyLink, Command, Flash, Mood, Update};

const USAGE: &str = "\
uso:
  mascota validar CARPETA
      Revisa la mascota de CARPETA: el mascota.toml, los dibujos, los tamaños,
      los fps, los estados que faltan y la licencia.
  mascota nueva ID [--nombre NOMBRE] [--en CARPETA]
      Crea una mascota nueva con dibujos de ejemplo, lista para reemplazar.
      Va a ~/.wawi2/mascotas/ID (o a CARPETA/ID con --en).
  mascota ver CARPETA [NOMBRE] [--segundos N] [--foto ARCHIVO.png] [--cuando S]
      La muestra en pantalla: todos sus estados y acciones, o solo NOMBRE
      (un estado como speaking, una acción o una animación; con [cuerpo],
      también «caer» y «pasear»). Ctrl+C para salir.
      --segundos cierra sola; --foto guarda cómo se ve al segundo y medio
      (o a los S segundos, con --cuando).
  mascota lista
      Las mascotas instaladas (la que viene con Wawi y las de ~/.wawi2/mascotas).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        println!("{USAGE}");
        return ExitCode::from(2);
    };
    let rest = &args[1..];
    match command.as_str() {
        "validar" => validar(rest),
        "nueva" => nueva(rest),
        "ver" => ver(rest),
        "lista" => lista(),
        "ayuda" | "--help" | "-h" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("no conozco «{other}»\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// `--flag VALUE` or `--flag=VALUE` out of `args`; the rest stay positional.
fn split(args: &[String], flags: &[&str]) -> (Vec<String>, Vec<(String, String)>) {
    let mut positional = Vec::new();
    let mut named = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f, Some(v.to_owned())),
            None => (arg.as_str(), None),
        };
        if flags.contains(&flag) {
            let value = inline.or_else(|| it.next().cloned()).unwrap_or_default();
            named.push((flag.to_owned(), value));
        } else {
            positional.push(arg.clone());
        }
    }
    (positional, named)
}

fn flag<'a>(named: &'a [(String, String)], name: &str) -> Option<&'a str> {
    named
        .iter()
        .rev()
        .find(|(f, _)| f == name)
        .map(|(_, v)| v.as_str())
}

fn validar(args: &[String]) -> ExitCode {
    let [dir] = args else {
        eprintln!("uso: mascota validar CARPETA");
        return ExitCode::from(2);
    };
    let report = pack::validate(Path::new(dir));
    println!("{report}");
    if report.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn nueva(args: &[String]) -> ExitCode {
    let (positional, named) = split(args, &["--nombre", "--en"]);
    let [id] = positional.as_slice() else {
        eprintln!("uso: mascota nueva ID [--nombre NOMBRE] [--en CARPETA]");
        return ExitCode::from(2);
    };
    let nombre = flag(&named, "--nombre")
        .map(str::to_owned)
        .unwrap_or_else(|| capitalized(id));
    let parent = match flag(&named, "--en") {
        Some(dir) => PathBuf::from(dir),
        None => match pack::home_dir() {
            Some(home) => pack::pets_dir(&home),
            None => {
                eprintln!("no sé dónde está tu carpeta personal: usá --en CARPETA");
                return ExitCode::FAILURE;
            }
        },
    };
    match pack::scaffold(&parent, id, &nombre) {
        Ok(dir) => {
            println!("Listo: {nombre} está en {}", dir.display());
            println!();
            println!("Ahora:");
            println!(
                "  1. Reemplazá los dibujos de {}/cuadros por los tuyos.",
                dir.display()
            );
            println!("  2. Ajustá mascota.toml (los comentarios explican cada línea).");
            println!("  3. Revisala:  mascota validar {}", dir.display());
            println!("  4. Mirala:    mascota ver {}", dir.display());
            println!("  5. Elegila en el menú de Wawi: Mascota ▸ {nombre}.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn capitalized(id: &str) -> String {
    let mut chars = id.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn lista() -> ExitCode {
    let home = pack::home_dir();
    let assets = wawi_body::default_assets();
    let mut quiet = |_: String| {};
    let active = pack::active(&assets, home.as_deref(), None, &mut quiet).map(|p| p.dir);
    let pets = pack::installed(&assets, home.as_deref());
    if pets.is_empty() {
        println!(
            "No encuentro ninguna mascota (ni siquiera a Wawi en {}).",
            assets.display()
        );
        return ExitCode::FAILURE;
    }
    for pet in pets {
        let mark = if Some(&pet.dir) == active.as_ref() {
            "▸"
        } else {
            " "
        };
        let origin = if pet.builtin { " (viene con Wawi)" } else { "" };
        println!(
            "{mark} {:<12} {}{origin}  {}",
            pet.id,
            pet.nombre,
            pet.dir.display()
        );
    }
    ExitCode::SUCCESS
}

/// What the preview plays, one step at a time.
enum Beat {
    Mood(Mood),
    Flash(Flash),
    Muted(bool),
    Play(String),
    Wait(f64),
    /// A line on the console.
    Say(String),
    /// A line in her speech bubble.
    Bubble(String),
    /// Lift her this high and let go.
    Drop(f64),
    /// A walk, now.
    Wander,
}

/// Her body, for a pack with `[cuerpo]`: a fall from high up (the porrazo
/// if the pack has one), then a short one, then a walk.
fn body_beats(name: &str) -> Option<Vec<Beat>> {
    match name {
        "caer" => Some(vec![
            Beat::Mood(Mood::Idle),
            Beat::Drop(600.0),
            Beat::Wait(4.0),
            Beat::Drop(200.0),
            Beat::Wait(2.0),
        ]),
        "pasear" => Some(vec![Beat::Mood(Mood::Idle), Beat::Wander, Beat::Wait(12.0)]),
        _ => None,
    }
}

fn look_beats(look: Look) -> Vec<Beat> {
    match look {
        Look::Idle => vec![Beat::Mood(Mood::Idle), Beat::Wait(5.0)],
        // The pack's own sleep timer would take a while: she is shown idle
        // long enough to fall asleep only in the full tour.
        Look::Sleeping => vec![Beat::Mood(Mood::Idle), Beat::Wait(14.0)],
        Look::Listening => vec![Beat::Mood(Mood::Listening), Beat::Wait(3.0)],
        Look::Transcribing => vec![Beat::Mood(Mood::Transcribing), Beat::Wait(2.0)],
        Look::Thinking => vec![Beat::Mood(Mood::Thinking), Beat::Wait(3.0)],
        Look::Speaking => vec![
            Beat::Bubble("Hola, así hablo yo.".into()),
            Beat::Mood(Mood::Speaking),
            Beat::Wait(4.0),
            Beat::Mood(Mood::Idle),
        ],
        Look::Interrupted => vec![
            Beat::Mood(Mood::Speaking),
            Beat::Wait(1.0),
            Beat::Flash(Flash::Interrupted),
            Beat::Mood(Mood::Listening),
            Beat::Wait(1.5),
            Beat::Mood(Mood::Idle),
        ],
        Look::Error => vec![
            Beat::Mood(Mood::Thinking),
            Beat::Wait(1.0),
            Beat::Flash(Flash::Error),
            Beat::Mood(Mood::Idle),
            Beat::Wait(2.0),
        ],
        Look::Muted => vec![
            Beat::Muted(true),
            Beat::Mood(Mood::Idle),
            Beat::Wait(5.0),
            Beat::Muted(false),
        ],
    }
}

fn clip_secs(pack: &Pack, name: &str) -> f64 {
    let clip = pack
        .action_clip(name)
        .and_then(|c| pack.clip(c))
        .or_else(|| pack.clip(name));
    clip.map(|c| c.duration()).unwrap_or(1.0).clamp(0.3, 10.0)
}

fn script(pack: &Pack, only: Option<&str>) -> Result<Vec<Beat>, String> {
    let play = |name: &str| {
        vec![
            Beat::Mood(Mood::Idle),
            Beat::Play(name.to_owned()),
            Beat::Wait(clip_secs(pack, name) + 0.8),
        ]
    };
    match only {
        Some(name) => {
            if let Some(look) = Look::parse(name) {
                Ok(look_beats(look))
            } else if pack.body.is_some()
                && let Some(beats) = body_beats(name)
            {
                Ok(beats)
            } else if pack.action_clip(name).is_some() || pack.clip(name).is_some() {
                Ok(play(name))
            } else {
                let mut known: Vec<String> =
                    Look::ALL.iter().map(|l| l.name().to_owned()).collect();
                known.extend(pack.actions().iter().map(|a| a.to_string()));
                let mut clips: Vec<&str> = pack.clip_names().collect();
                clips.sort();
                known.extend(clips.iter().map(|c| c.to_string()));
                let mut seen = std::collections::HashSet::new();
                known.retain(|k| seen.insert(k.clone()));
                Err(format!(
                    "«{}» no tiene «{name}». Probá con: {}",
                    pack.name(),
                    known.join(", ")
                ))
            }
        }
        None => {
            let mut beats = Vec::new();
            for look in Look::ALL {
                beats.push(Beat::Say(format!("estado: {}", look.name())));
                beats.extend(look_beats(look));
            }
            for action in pack.actions() {
                beats.push(Beat::Say(format!("acción: {action}")));
                beats.extend(play(action));
            }
            if pack.body.is_some() {
                for name in ["caer", "pasear"] {
                    beats.push(Beat::Say(format!("cuerpo: {name}")));
                    beats.extend(body_beats(name).unwrap_or_default());
                }
            }
            Ok(beats)
        }
    }
}

fn ver(args: &[String]) -> ExitCode {
    let (positional, named) = split(args, &["--segundos", "--foto", "--cuando"]);
    let (dir, only) = match positional.as_slice() {
        [dir] => (PathBuf::from(dir), None),
        [dir, name] => (PathBuf::from(dir), Some(name.clone())),
        _ => {
            eprintln!(
                "uso: mascota ver CARPETA [NOMBRE] [--segundos N] [--foto ARCHIVO.png] [--cuando S]"
            );
            return ExitCode::from(2);
        }
    };
    let limit = flag(&named, "--segundos").and_then(|s| s.parse::<f64>().ok());
    let photo = flag(&named, "--foto").map(PathBuf::from);
    let photo_at = flag(&named, "--cuando")
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .unwrap_or(1.5);
    let report = pack::validate(&dir);
    let Some(pet) = report.pack.as_ref() else {
        println!("{report}");
        return ExitCode::FAILURE;
    };
    if !report.ok() {
        println!("{report}\n");
    }
    let beats = match script(pet, only.as_deref()) {
        Ok(beats) => beats,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    if !wawi_body::AVAILABLE {
        eprintln!("La vista previa por ahora solo existe en macOS.");
        return ExitCode::FAILURE;
    }
    println!(
        "Mostrando a {} ({}). Ctrl+C o «Salir» en el menú para terminar.",
        pet.name(),
        dir.display()
    );
    // No home: a preview never saves where she stood, nor switches the pet.
    let config = BodyConfig {
        home: None,
        state_file: None,
        logs_dir: None,
        pet_dir: Some(dir),
        ..BodyConfig::for_home(None)
    };
    let on_command = Box::new(|command, link: &BodyLink| match command {
        Command::SetListening(listening) => link.send(Update::Muted(!listening)),
        Command::Quit => link.close(),
    });
    let result = wawi_body::run(config, on_command, move |link| {
        if let Some(photo) = photo {
            let link = link.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_secs_f64(photo_at));
                link.send(Update::Snapshot(photo));
            });
        }
        thread::spawn(move || {
            let started = Instant::now();
            let over =
                |started: Instant| limit.is_some_and(|s| started.elapsed().as_secs_f64() >= s);
            'outer: loop {
                for beat in &beats {
                    match beat {
                        Beat::Mood(m) => link.send(Update::Mood(*m)),
                        Beat::Flash(f) => link.send(Update::Flash(*f)),
                        Beat::Muted(on) => link.send(Update::Muted(*on)),
                        Beat::Play(name) => {
                            println!("  {name}");
                            link.play(name);
                        }
                        Beat::Say(text) => println!("  {text}"),
                        Beat::Bubble(text) => link.send(Update::Said(text.clone())),
                        Beat::Drop(height) => link.send(Update::Drop(*height)),
                        Beat::Wander => link.send(Update::Wander),
                        Beat::Wait(secs) => {
                            let until = Instant::now() + Duration::from_secs_f64(*secs);
                            while Instant::now() < until {
                                if over(started) {
                                    break 'outer;
                                }
                                thread::sleep(Duration::from_millis(50));
                            }
                        }
                    }
                }
                if over(started) {
                    break;
                }
            }
            link.close();
        });
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
