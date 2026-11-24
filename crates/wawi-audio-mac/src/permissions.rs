//! Microphone permission, and naming the app that has to get it.
//!
//! macOS gives these permissions to the app that launched the process (the
//! terminal, the editor), not to the binary, so every message names that app.

use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

/// How long to wait for the owner to answer the system's microphone prompt.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicPermission {
    Granted,
    Denied,
    /// Blocked by a profile or parental controls; the owner cannot change it.
    Restricted,
    NotDetermined,
}

pub fn microphone_permission() -> MicPermission {
    // SAFETY: AVMediaTypeAudio is an immutable framework constant, and audio is
    // an accepted media type for this call.
    let status = unsafe {
        match AVMediaTypeAudio {
            Some(media) => AVCaptureDevice::authorizationStatusForMediaType(media),
            None => AVAuthorizationStatus::NotDetermined,
        }
    };
    match status {
        AVAuthorizationStatus::Authorized => MicPermission::Granted,
        AVAuthorizationStatus::Denied => MicPermission::Denied,
        AVAuthorizationStatus::Restricted => MicPermission::Restricted,
        _ => MicPermission::NotDetermined,
    }
}

/// Makes sure the mic can be used, asking the system (which shows its prompt
/// once) if nobody decided yet. Blocks while the prompt is up. On failure the
/// error is a message in Spanish for the owner.
pub fn ensure_microphone() -> Result<(), String> {
    match microphone_permission() {
        MicPermission::Granted => Ok(()),
        MicPermission::Denied => Err(microphone_help()),
        MicPermission::Restricted => Err(
            "El micrófono está bloqueado por una restricción del sistema (un perfil o los \
             controles parentales). Desde acá no se puede habilitar."
                .to_owned(),
        ),
        MicPermission::NotDetermined => {
            if request_microphone() {
                Ok(())
            } else {
                Err(microphone_help())
            }
        }
    }
}

fn request_microphone() -> bool {
    let Some(media) = (unsafe { AVMediaTypeAudio }) else {
        return false;
    };
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |granted: Bool| {
        let _ = tx.send(granted.as_bool());
    });
    // SAFETY: valid media type; the handler is copied by the framework and may
    // run on any queue, which only touches a channel.
    unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &handler) };
    rx.recv_timeout(PROMPT_TIMEOUT).unwrap_or(false)
}

pub fn microphone_help() -> String {
    format!(
        "Falta el permiso del micrófono. Abrí Ajustes del Sistema > Privacidad y seguridad > \
         Micrófono y habilitá {}. Después cerrala del todo y volvé a abrirla.",
        launching_app()
    )
}

/// The app macOS will ask about, as the owner sees it in System Settings.
pub fn launching_app() -> String {
    // Opened as Wawi.app (no terminal), the app itself is the one macOS asks
    // about.
    let in_bundle = std::env::current_exe()
        .is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"));
    if in_bundle && std::env::var_os("TERM_PROGRAM").is_none() {
        return "la app Wawi".to_owned();
    }
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let name = match program.as_str() {
        "Apple_Terminal" => "Terminal",
        "iTerm.app" => "iTerm",
        "vscode" => "Visual Studio Code (o el editor desde el que corrés esto)",
        "WarpTerminal" => "Warp",
        "ghostty" => "Ghostty",
        "WezTerm" => "WezTerm",
        "Hyper" => "Hyper",
        "zed" => "Zed",
        "" => return "la app desde la que corrés esto (la terminal o el editor)".to_owned(),
        other => return format!("la app desde la que corrés esto ({other})"),
    };
    format!("la app {name}")
}

/// The microphone mode macOS applies to this app (Control Center > Mic Mode),
/// in Spanish. `None` before macOS 12.
pub fn microphone_mode() -> Option<&'static str> {
    use objc2::{ClassType, sel};
    use objc2_av_foundation::AVCaptureMicrophoneMode;
    if !AVCaptureDevice::class().responds_to(sel!(activeMicrophoneMode)) {
        return None;
    }
    // SAFETY: a read-only class property, present (checked above).
    let mode = unsafe { AVCaptureDevice::activeMicrophoneMode() };
    Some(match mode {
        AVCaptureMicrophoneMode::Standard => "estándar",
        AVCaptureMicrophoneMode::WideSpectrum => "espectro amplio",
        AVCaptureMicrophoneMode::VoiceIsolation => "aislamiento de voz",
        _ => "desconocido",
    })
}

/// Opens the system's microphone-mode picker. It only shows while this
/// process is using the mic.
pub fn show_microphone_modes() {
    use objc2::{ClassType, sel};
    use objc2_av_foundation::AVCaptureSystemUserInterface;
    if AVCaptureDevice::class().responds_to(sel!(showSystemUserInterface:)) {
        // SAFETY: class method present (checked above); it only shows UI.
        unsafe {
            AVCaptureDevice::showSystemUserInterface(AVCaptureSystemUserInterface::MicrophoneModes)
        };
    }
}
