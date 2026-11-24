//! `wawi-autoarreglo mcp`: the MCP server Wawi loads as a plugin, with the
//! worker inside the same process (if no other worker holds the lock).
//! `wawi-autoarreglo run`: the worker alone (for launchd, say).
//! `intentar [id]`, `revisar` and `estado` are for the owner, by hand.
//!
//! The home is `$WAWI2_HOME` or `~/.wawi2`.

use std::io::{BufReader, stdin, stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use wawi_autoarreglo::clock::SystemClock;
use wawi_autoarreglo::config::Config;
use wawi_autoarreglo::diario;
use wawi_autoarreglo::elegir;
use wawi_autoarreglo::intento::{self, EstadoServicio};
use wawi_autoarreglo::log::Log;
use wawi_autoarreglo::motor::{self, Cargo, ClaudeCli};
use wawi_autoarreglo::propuestas;
use wawi_autoarreglo::revision;
use wawi_autoarreglo::servidor::Servidor;
use wawi_autoarreglo::worker::{self, Worker};
use wawi_autoarreglo::{home, incidentes};
use wawi_tools::WallClock;

const USAGE: &str = "uso: wawi-autoarreglo mcp | run | intentar [id] | revisar | estado
  mcp       servidor MCP por stdio (el plugin de Wawi), con el worker adentro
  run       solo el worker (por ejemplo desde launchd)
  intentar  un intento ahora, fuera de la ventana horaria (respeta los topes)
  revisar   la revisión de la noche, ahora
  estado    qué problemas hay, qué arreglos y cuánto se gastó hoy
La carpeta es $WAWI2_HOME o ~/.wawi2. Viene apagado: ver docs/AUTOARREGLO.md.";

fn make_worker(home: &Path, config: &Config, log: &Log) -> Worker {
    Worker {
        home: home.to_path_buf(),
        clock: Rc::new(SystemClock::new()),
        motor: Box::new(ClaudeCli::new(config.nice).con_cupo(home)),
        comandos: Box::new(Cargo {
            target: config.target_dir(home),
            nice: config.nice,
        }),
        log: log.clone(),
        last_work: None,
    }
}

fn estado(home: &Path, config: &Config) {
    let clock = SystemClock::new();
    let today = clock.local_now().iso_date();
    let mut estado = EstadoServicio::load(home);
    let day = estado.dia(&today).clone();
    println!(
        "autoarreglo: {} · repo: {} · hoy: {} de {} intentos, US$ {:.2} de {:.2}",
        if config.activo { "prendido" } else { "apagado" },
        config
            .repo()
            .map(|r| r.display().to_string())
            .unwrap_or_else(|| "sin configurar".into()),
        day.intentos,
        config.max_intentos_dia,
        day.usd,
        config.presupuesto_dia_usd
    );
    let all = diario::load_all(home);
    println!("problemas ({}):", all.len());
    for p in &all {
        let why = elegir::elegible(p, config, clock.now())
            .err()
            .unwrap_or("elegible");
        println!(
            "  {} [{}] x{} {} ({why})",
            p.id,
            p.estado.nombre(),
            p.cuenta,
            p.titulo
        );
    }
    println!("arreglos:\n{}", propuestas::lista(home));
}

fn main() -> ExitCode {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let home: PathBuf = home();
    let (config, problem) = Config::load(&home);
    match mode.as_str() {
        "mcp" => {
            let log = Log::open(&home, false);
            if let Some(problem) = problem {
                log.line(&problem);
            }
            let _ = incidentes::ensure(&home);
            {
                let (home, config, log) = (home.clone(), config.clone(), log.clone());
                std::thread::spawn(move || {
                    loop {
                        match worker::lock(&home) {
                            Ok(Some(_lock)) => {
                                log.line("worker: arrancó (dentro del plugin)");
                                worker::run(
                                    make_worker(&home, &config, &log),
                                    Arc::new(AtomicBool::new(false)),
                                );
                                return;
                            }
                            Ok(None) => {}
                            Err(e) => log.line(&format!("worker: no pude tomar el lock: {e}")),
                        }
                        std::thread::sleep(Duration::from_secs(30));
                    }
                });
            }
            let servidor = Servidor::new(&home, log.clone());
            servidor.serve(BufReader::new(stdin().lock()), stdout().lock());
            // Wawi closed: nothing of ours keeps running behind her.
            motor::kill_all();
            log.line("mcp: stdin cerrado, salgo");
            ExitCode::SUCCESS
        }
        "run" => {
            let log = Log::open(&home, true);
            if let Some(problem) = problem {
                log.line(&problem);
            }
            let _lock = match worker::lock(&home) {
                Ok(Some(lock)) => lock,
                Ok(None) => {
                    eprintln!("ya hay otro worker de autoarreglo en {}", home.display());
                    return ExitCode::from(3);
                }
                Err(e) => {
                    eprintln!("no pude tomar el lock: {e}");
                    return ExitCode::from(1);
                }
            };
            log.line(&format!("worker: arrancó en {}", home.display()));
            worker::run(
                make_worker(&home, &config, &log),
                Arc::new(AtomicBool::new(false)),
            );
            ExitCode::SUCCESS
        }
        "intentar" => {
            let log = Log::open(&home, true);
            let Ok(Some(_lock)) = worker::lock(&home) else {
                eprintln!(
                    "hay un worker corriendo (el plugin o launchd); cerralo antes de intentar a mano"
                );
                return ExitCode::from(3);
            };
            let w = make_worker(&home, &config, &log);
            w.absorber();
            let solo = std::env::args().nth(2);
            let fin = intento::intentar(&w.contexto(&config), true, solo.as_deref());
            println!("{fin:?}");
            ExitCode::SUCCESS
        }
        "revisar" => {
            let log = Log::open(&home, true);
            let w = make_worker(&home, &config, &log);
            let n = revision::revisar(&home, &config, &*w.motor, &*w.clock, &log);
            w.absorber();
            println!("anoté {n} problemas");
            ExitCode::SUCCESS
        }
        "estado" => {
            if let Some(problem) = problem {
                eprintln!("{problem}");
            }
            estado(&home, &config);
            ExitCode::SUCCESS
        }
        "-h" | "--help" | "" => {
            println!("{USAGE}");
            if mode.is_empty() {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            }
        }
        "--version" | "-V" => {
            println!("wawi-autoarreglo {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("modo «{other}» desconocido\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
