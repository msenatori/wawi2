//! `wawi-instinto mcp`: the MCP server Wawi loads as a plugin, with the
//! worker inside the same process (if no other worker holds the lock).
//! `wawi-instinto run`: the worker alone (for launchd, say).
//!
//! The home is `$WAWI2_HOME` or `~/.wawi2`.

use std::io::{BufReader, stdin, stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use wawi_instinto::claude::ClaudeCli;
use wawi_instinto::clock::SystemClock;
use wawi_instinto::config::Config;
use wawi_instinto::fuentes::{Registro, claude_dir};
use wawi_instinto::juez::ModeloJuez;
use wawi_instinto::log::Log;
use wawi_instinto::observador::{self, Observador};
use wawi_instinto::servidor::{DRY_RUN, Servidor};
use wawi_instinto::worker::{self, Worker};
use wawi_instinto::{colas, home};

const USAGE: &str = "uso: wawi-instinto mcp | run\n  mcp  servidor MCP por stdio (el plugin de Wawi), con el worker adentro\n  run  solo el worker (por ejemplo desde launchd)\nLa carpeta es $WAWI2_HOME o ~/.wawi2.";

fn make_worker(home: &Path, config: &Config, log: &Log) -> Worker {
    Worker {
        home: home.to_path_buf(),
        clock: Rc::new(SystemClock::new()),
        fuentes: Registro::base(),
        juez: Box::new(ModeloJuez {
            modelo: ClaudeCli::new(&claude_dir(home)).con_cupo(home, "instinto"),
            por_defecto: config.juzgar.modelo.clone(),
            max_dia: config.juzgar.max_dia,
            cupo: home.join("instinto/juez-cupo.json"),
        }),
        log: log.clone(),
        quiet: config.quiet().ok().flatten(),
    }
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
            let _ = colas::ensure(&home);
            // The worker: takes the lock if it can; if another worker has
            // it (launchd), retries every 30 s in case that one stops.
            {
                let (home, config, log) = (home.clone(), config.clone(), log.clone());
                std::thread::spawn(move || {
                    loop {
                        match worker::lock(&home) {
                            Ok(Some(_lock)) => {
                                log.line("worker: arrancó (dentro del plugin)");
                                let stop = Arc::new(AtomicBool::new(false));
                                worker::run(make_worker(&home, &config, &log), stop);
                                return;
                            }
                            Ok(None) => {}
                            Err(e) => log.line(&format!("worker: no pude tomar el lock: {e}")),
                        }
                        std::thread::sleep(Duration::from_secs(30));
                    }
                });
            }
            let observar = {
                let (home, cfg, log2) = (home.clone(), config.observador.clone(), log.clone());
                observador::spawn(
                    move || {
                        Observador::new(
                            &home,
                            cfg,
                            Box::new(
                                ClaudeCli::new(&claude_dir(&home)).con_cupo(&home, "instinto"),
                            ),
                            Rc::new(SystemClock::new()),
                            log2,
                        )
                    },
                    log.clone(),
                )
            };
            let servidor = Servidor {
                home: home.clone(),
                fuentes: Registro::base(),
                log: log.clone(),
                dry_run: DRY_RUN,
                observar: Box::new(observar),
            };
            servidor.serve(BufReader::new(stdin().lock()), stdout().lock());
            log.line("mcp: stdin cerrado, salgo");
            ExitCode::SUCCESS
        }
        "run" => {
            let log = Log::open(&home, true);
            if let Some(problem) = problem {
                log.line(&problem);
            }
            if let Err(e) = colas::ensure(&home) {
                eprintln!("no pude crear las colas en {}: {e}", home.display());
                return ExitCode::from(1);
            }
            let _lock = match worker::lock(&home) {
                Ok(Some(lock)) => lock,
                Ok(None) => {
                    eprintln!(
                        "ya hay otro worker del instinto corriendo en {}",
                        home.display()
                    );
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
        "-h" | "--help" | "" => {
            println!("{USAGE}");
            if mode.is_empty() {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            }
        }
        "--version" | "-V" => {
            println!("wawi-instinto {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("modo «{other}» desconocido\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
