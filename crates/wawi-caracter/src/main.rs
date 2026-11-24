//! `wawi-caracter mcp`: the MCP server Wawi loads as a plugin, with the
//! learner inside. The home is `$WAWI2_HOME` or `~/.wawi2`.

use std::io::{BufReader, stdin, stdout};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;

use wawi_caracter::libro::{Aprendiz, Libro, spawn};
use wawi_caracter::log::Log;
use wawi_caracter::servidor::Servidor;
use wawi_caracter::{DIR, ESFUERZO, MODELO, home};
use wawi_instinto::claude::ClaudeCli;
use wawi_instinto::clock::SystemClock;

const USAGE: &str = "uso: wawi-caracter mcp\n  mcp  servidor MCP por stdio (el plugin de Wawi), con el aprendiz adentro\nLa carpeta es $WAWI2_HOME o ~/.wawi2.";

fn main() -> ExitCode {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "mcp" => {
            let home = home();
            let log = Log::open(&home, false);
            let libro = Arc::new(Libro::new(&home, log.clone()));
            let observar = {
                let (libro, home) = (libro.clone(), home.clone());
                spawn(
                    move || {
                        Aprendiz::new(
                            libro,
                            // An empty folder: no project file near the model.
                            Box::new(
                                ClaudeCli::new(&home.join(DIR).join("claude"))
                                    .con_cupo(&home, "caracter"),
                            ),
                            Rc::new(SystemClock::new()),
                            MODELO,
                            ESFUERZO,
                        )
                    },
                    log.clone(),
                )
            };
            let servidor = Servidor {
                libro,
                clock: Box::new(SystemClock::new()),
                observar: Box::new(observar),
            };
            servidor.serve(BufReader::new(stdin().lock()), stdout().lock());
            log.line("mcp: stdin cerrado, salgo");
            ExitCode::SUCCESS
        }
        "-h" | "--help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        "--version" | "-V" => {
            println!("wawi-caracter {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
