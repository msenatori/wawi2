//! `wawi-curiosidad mcp`: the MCP server Wawi loads as a plugin. The home
//! is `$WAWI2_HOME` or `~/.wawi2`.

use std::io::{BufReader, stdin, stdout};
use std::process::ExitCode;

use wawi_curiosidad::log::Log;
use wawi_curiosidad::{Servidor, home};
use wawi_instinto::clock::SystemClock;

const USAGE: &str = "uso: wawi-curiosidad mcp | senales | medir IMAGEN\n  mcp      servidor MCP por stdio (el plugin de Wawi)\n  senales  muestra qué ve de la Mac ahora (sin capturar nada)\n  medir    mide una mirada con IMAGEN en vez de la pantalla (gasta una llamada)\nLa carpeta es $WAWI2_HOME o ~/.wawi2.";

fn main() -> ExitCode {
    match std::env::args().nth(1).unwrap_or_default().as_str() {
        "mcp" => {
            let home = home();
            let log = Log::open(&home, false);
            let servidor = Servidor::real(&home, Box::new(SystemClock::new()), log.clone());
            servidor.serve(BufReader::new(stdin().lock()), stdout().lock());
            log.line("mcp: stdin cerrado, salgo");
            ExitCode::SUCCESS
        }
        "senales" => {
            let started = std::time::Instant::now();
            let s = wawi_curiosidad::mac::senales();
            println!("{s:#?}");
            println!("tardó {} ms", started.elapsed().as_millis());
            ExitCode::SUCCESS
        }
        "medir" => {
            let Some(path) = std::env::args().nth(2) else {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            };
            let espontanea = std::env::args().nth(3).as_deref() == Some("espontanea");
            let home = home();
            let vista = wawi_curiosidad::vision::Vista::new(&home.join("curiosidad/claude"));
            for (label, warm) in [("en frío", false), ("con el proceso listo", true)] {
                if warm {
                    vista.preparar();
                    std::thread::sleep(std::time::Duration::from_secs(4));
                }
                let started = std::time::Instant::now();
                let jpeg = match wawi_curiosidad::mac::achicar(
                    std::path::Path::new(&path),
                    &std::env::temp_dir(),
                    wawi_curiosidad::MAX_EDGE,
                    wawi_curiosidad::QUALITY,
                ) {
                    Ok(jpeg) => jpeg,
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::from(1);
                    }
                };
                let shrunk = started.elapsed().as_millis();
                let answer = vista.ver(&wawi_curiosidad::base64(&jpeg), espontanea);
                println!(
                    "{label}: achicar {shrunk} ms, total {} ms, {} KB",
                    started.elapsed().as_millis(),
                    jpeg.len() / 1024
                );
                match answer {
                    Ok(text) => println!("{text}\n"),
                    Err(e) => println!("falló: {e}\n"),
                }
                // Let the spare that `ver` readies settle before the next run.
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            ExitCode::SUCCESS
        }
        "-h" | "--help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        "--version" | "-V" => {
            println!("wawi-curiosidad {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
