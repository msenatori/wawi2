//! `wawi2 setup`: the first run. Creates the Wawi folder, downloads the
//! models (pinned revision, size and SHA-256 checked), checks that the
//! `claude` CLI is there and logged in, and checks the macOS permissions,
//! opening the right System Settings pane when one is missing.
//!
//! Safe to run again: what is already done is only checked.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use wawi_plugins::manifest::{self, Server};
use wawi_stt_parakeet::model::{self as stt_model, PARAKEET_TDT_V3_INT8};
use wawi_tts_kokoro::model::{self as tts_model, KOKORO_82M_V1};

use crate::launch;

/// How long `claude auth status` may take.
const CLAUDE_LIMIT: Duration = Duration::from_secs(20);

pub const DONE: &str = "Listo. Abrí Wawi y mantené ⌘ derecha para hablarle.";

/// `ask`: false with `--sin-abrir`, which only reports the permissions (no
/// system prompt, no System Settings).
pub fn main(home: Option<PathBuf>, ask: bool) -> ExitCode {
    println!("Wawi 2: preparando todo. Se puede correr las veces que quieras.\n");
    let Some(home) = home else {
        println!("No encuentro tu carpeta personal. Definí WAWI2_HOME y probá de nuevo.");
        return ExitCode::FAILURE;
    };
    let mut pending: Vec<String> = Vec::new();

    // 1. The folder.
    match fs::create_dir_all(home.join("logs")) {
        Ok(()) => println!("Carpeta: {}", home.display()),
        Err(error) => {
            println!("No pude crear {}: {error}", home.display());
            return ExitCode::FAILURE;
        }
    }

    // 2. The models.
    println!("\nModelos (se bajan una sola vez, unos 1000 MB en total):");
    if let Err(problem) = parakeet() {
        pending.push(problem);
    }
    if let Err(problem) = kokoro() {
        pending.push(problem);
    }

    // 3. Claude Code.
    println!("\nCerebro (Claude Code):");
    if let Err(problem) = claude(&home) {
        pending.push(problem);
    }

    // 4. Plugins: their commands have to be found without the shell's PATH.
    let (plugins, problems) = manifest::discover(&home);
    let enabled: Vec<_> = plugins.iter().filter(|p| p.enabled).collect();
    if !enabled.is_empty() || !problems.is_empty() {
        println!("\nPlugins:");
        for problem in problems {
            println!("  {problem}");
        }
        for plugin in &enabled {
            if let Server::Stdio { command, .. } = &plugin.server {
                if found(command) {
                    println!("  {}: ok", plugin.name);
                } else {
                    println!(
                        "  {}: no encuentro «{command}». Instalalo (está en su plugin.toml) \
                         o agregá su carpeta a {}.",
                        plugin.name,
                        home.join(launch::PATH_FILE).display()
                    );
                    pending.push(format!("el comando del plugin {}", plugin.name));
                }
            } else {
                println!("  {}: ok", plugin.name);
            }
        }
    }
    let curiosidad = enabled.iter().any(|plugin| {
        plugin.name == "curiosidad"
            || matches!(&plugin.server, Server::Stdio { command, .. } if command.ends_with("wawi-curiosidad"))
    });

    // 5. Permissions.
    println!("\nPermisos de macOS:");
    pending.extend(permissions::check(curiosidad, ask));

    println!();
    if pending.is_empty() {
        println!("{DONE}");
        ExitCode::SUCCESS
    } else {
        println!("Falta: {}.", pending.join("; "));
        println!("Cuando lo arregles, corré «wawi2 setup» de nuevo.");
        ExitCode::FAILURE
    }
}

fn parakeet() -> Result<(), String> {
    let dir = stt_model::model_dir(&PARAKEET_TDT_V3_INT8).map_err(|e| e.to_string())?;
    println!("  oído (Parakeet): {}", dir.display());
    stt_model::ensure(&PARAKEET_TDT_V3_INT8, &dir, &mut |line| {
        println!("    {line}")
    })
    .map(|()| println!("  oído: ok"))
    .map_err(|error| {
        println!("  oído: no se pudo ({error}). Revisá la conexión y probá de nuevo.");
        "el modelo del oído".to_owned()
    })
}

fn kokoro() -> Result<(), String> {
    let dir = tts_model::model_dir(&KOKORO_82M_V1).map_err(|e| e.to_string())?;
    println!("  voz (Kokoro): {}", dir.display());
    tts_model::ensure(&KOKORO_82M_V1, &dir, &mut |line| println!("    {line}"))
        .map(|()| println!("  voz: ok"))
        .map_err(|error| {
            println!("  voz: no se pudo ({error}). Revisá la conexión y probá de nuevo.");
            "el modelo de la voz".to_owned()
        })
}

/// A plugin command: a path that exists, or a name on the (extended) PATH.
fn found(command: &str) -> bool {
    if command.contains('/') {
        return Path::new(command).is_file();
    }
    launch::find_on_path(command).is_some()
}

/// Finds `claude` the way the brain will, remembers its folder in
/// `<home>/path` (for Wawi.app, which gets no shell PATH), and asks it
/// whether it is logged in.
fn claude(home: &Path) -> Result<(), String> {
    let program = wawi_brain_claude::default_program();
    let resolved = if program.components().count() > 1 {
        program.is_file().then_some(program)
    } else {
        launch::find_on_path(&program.to_string_lossy())
    };
    let Some(program) = resolved else {
        println!("  No encuentro el programa «claude».");
        println!("  Instalalo con:  curl -fsSL https://claude.ai/install.sh | bash");
        println!("  Después abrí «claude» una vez y entrá con tu cuenta (Pro o Max).");
        println!(
            "  Si ya lo tenés en otro lado, poné su carpeta en {}.",
            home.join(launch::PATH_FILE).display()
        );
        return Err("instalar Claude Code".to_owned());
    };
    println!("  programa: {}", program.display());
    remember_folder(home, &program);
    match logged_in(&program) {
        Some(true) => {
            println!("  sesión: iniciada");
            Ok(())
        }
        Some(false) => {
            println!("  sesión: no iniciada. Corré «claude auth login» (o abrí «claude»)");
            println!("  y entrá con tu cuenta. Sin navegador: «claude setup-token».");
            Err("iniciar sesión en Claude Code".to_owned())
        }
        None => {
            println!("  sesión: no la pude confirmar. Si «claude -p hola» contesta, está bien.");
            Ok(())
        }
    }
}

/// Adds the folder of `claude` to `<home>/path` unless the app would already
/// find it there. Never rewrites what the owner wrote.
fn remember_folder(home: &Path, program: &Path) {
    let Some(dir) = program.parent() else {
        return;
    };
    let file = home.join(launch::PATH_FILE);
    let text = fs::read_to_string(&file).unwrap_or_default();
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    let listed = launch::listed_dirs(&text, user_home.as_deref());
    // Folders the app always looks in (see `launch::extend_path`).
    let usual = user_home.as_deref().is_some_and(|h| {
        [".local/bin", ".claude/local", ".cargo/bin"]
            .iter()
            .any(|sub| h.join(sub) == dir)
    }) || [Path::new("/opt/homebrew/bin"), Path::new("/usr/local/bin")].contains(&dir);
    if usual || listed.iter().any(|known| known == dir) {
        return;
    }
    let mut text = text;
    if text.is_empty() {
        text.push_str("# Carpetas que Wawi.app agrega al PATH, una por línea.\n");
    } else if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("{}\n", dir.display()));
    match fs::write(&file, text) {
        Ok(()) => println!("  anotado en {}, para Wawi.app", file.display()),
        Err(error) => println!("  no pude anotarlo en {}: {error}", file.display()),
    }
}

/// `claude auth status --json`: `loggedIn`. `None` if it did not answer.
fn logged_in(program: &Path) -> Option<bool> {
    let mut child = Command::new(program)
        .args(["auth", "status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < CLAUDE_LIMIT => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    parse_logged_in(&out)
}

fn parse_logged_in(json: &str) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    value.get("loggedIn")?.as_bool()
}

#[cfg(target_os = "macos")]
mod permissions {
    use wawi_audio_mac::hotkey::input_monitoring_granted;
    use wawi_audio_mac::permissions::{MicPermission, launching_app, microphone_permission};
    use wawi_audio_mac::{RightCommandKey, ensure_microphone};

    use crate::launch::{self, Pane};

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    /// What is missing, in short. With `ask`, asks macOS (which shows its
    /// prompt once) and opens the pane for anything still missing.
    pub fn check(curiosidad: bool, ask: bool) -> Vec<String> {
        let app = launching_app();
        println!("  Se le dan a {app}.");
        if !launch::from_bundle() {
            println!(
                "  Si después usás Wawi.app, ella pide los suyos la primera vez que la abrís."
            );
        }
        let mut pending = Vec::new();

        let mic = match microphone_permission() {
            MicPermission::Granted => true,
            MicPermission::NotDetermined => ask && ensure_microphone().is_ok(),
            MicPermission::Denied | MicPermission::Restricted => false,
        };
        report(Pane::Microphone, mic, &app, ask, &mut pending);

        let keys = input_monitoring_granted()
            || ask && {
                // Asks macOS to list the app under Input Monitoring.
                let _ = RightCommandKey::spawn(|_| {});
                input_monitoring_granted()
            };
        report(Pane::InputMonitoring, keys, &app, ask, &mut pending);

        if curiosidad {
            // SAFETY: plain queries with no arguments.
            let screen = unsafe {
                CGPreflightScreenCaptureAccess()
                    || ask && {
                        CGRequestScreenCaptureAccess();
                        CGPreflightScreenCaptureAccess()
                    }
            };
            report(Pane::ScreenRecording, screen, &app, ask, &mut pending);
        } else {
            println!(
                "  {}: no hace falta (el plugin curiosidad no está prendido)",
                Pane::ScreenRecording.name()
            );
        }
        pending
    }

    fn report(pane: Pane, granted: bool, app: &str, ask: bool, pending: &mut Vec<String>) {
        if granted {
            println!("  {}: permitido", pane.name());
            return;
        }
        println!(
            "  {}: falta. {} Ajustes del Sistema > Privacidad y seguridad > {}:",
            pane.name(),
            if ask { "Te abro" } else { "Abrí" },
            pane.name()
        );
        println!("    habilitá {app} (si no aparece, agregala con +).");
        println!("    Después cerrala del todo y volvé a abrirla.");
        if ask {
            pane.open();
        }
        pending.push(format!("el permiso de {}", pane.name()));
    }
}

#[cfg(not(target_os = "macos"))]
mod permissions {
    pub fn check(_curiosidad: bool, _ask: bool) -> Vec<String> {
        println!("  (solo en macOS)");
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_check_reads_logged_in_and_nothing_else() {
        assert_eq!(
            parse_logged_in(r#"{"loggedIn": true, "authMethod": "claude.ai"}"#),
            Some(true)
        );
        assert_eq!(parse_logged_in(r#"{"loggedIn": false}"#), Some(false));
        assert_eq!(parse_logged_in("Not logged in"), None);
        assert_eq!(parse_logged_in(r#"{"other": 1}"#), None);
    }

    #[test]
    fn claudes_folder_is_written_once_and_the_owners_lines_stay() {
        let home = std::env::temp_dir().join(format!("wawi-setup-path-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join(launch::PATH_FILE), "/mine/bin").unwrap();
        let program = Path::new("/opt/nvm/v22/bin/claude");
        remember_folder(&home, program);
        remember_folder(&home, program);
        assert_eq!(
            fs::read_to_string(home.join(launch::PATH_FILE)).unwrap(),
            "/mine/bin\n/opt/nvm/v22/bin\n"
        );
        remember_folder(&home, Path::new("/usr/local/bin/claude"));
        assert_eq!(
            fs::read_to_string(home.join(launch::PATH_FILE)).unwrap(),
            "/mine/bin\n/opt/nvm/v22/bin\n"
        );
        fs::remove_dir_all(&home).unwrap();
    }
}
