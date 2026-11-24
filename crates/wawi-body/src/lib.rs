//! Wawi's body: the mascot on screen and her menu bar item.
//!
//! Everything that decides what she looks like is pure and lives here, so it
//! is tested on any OS and shared by every backend:
//!
//! - [`mood`]: the dialogue state as a look (idle, listening, transcribing,
//!   thinking, speaking) and the short flashes (interrupted, error).
//! - [`pack`]: pet packs, the art and animations outside the code
//!   (`mascota.toml`): Wawi built in, others in `~/.wawi2/mascotas/<id>/`.
//! - [`anim`]: the flip-book animator, first a port of Wawi 1's
//!   (`wawi/mascot.py`), now playing whatever the active pack says: which
//!   frame to show, the crossfade, the whole-sprite pose and the badge.
//! - [`bubble`]: the speech bubble over her head (what she says, what she
//!   heard) and when it fades.
//! - [`place`]: where she stands (and which pet is on screen), remembered in
//!   `~/.wawi2/mascot.json`.
//! - [`menu`]: the menu (menu bar and right click) and what each item does.
//! - [`body`]: her body on the desktop (the pack's `[cuerpo]`): gravity,
//!   walking, climbing, the parachute, sitting on windows; pure, driven by
//!   the time and a world of screens and windows.
//!
//! A backend owns the OS's UI thread: it creates the windows, draws the
//! [`anim::Scene`] the animator hands it, forwards clicks and drags, and
//! turns menu picks into [`Command`]s. The only one today is AppKit
//! (`mac`). Another OS plugs in by adding its module and a branch in
//! [`run`]; nothing else changes.
//!
//! Threads: [`run`] must be called on the main thread and only returns when
//! the program ends. The rest of the program runs on a worker thread started
//! by `start`, and talks to the body through the [`BodyLink`] it receives.
//! Sending is a channel push: the voice thread never waits on the body, and
//! never decodes a picture (the backend decodes frames lazily, on its own
//! loader thread, and keeps the idle look ready from the start).
//!
//! Actions: [`BodyLink::play`] plays a named action of the active pack
//! ([`pack::Pack::actions`] lists them, [`pack::active`] loads the pack from
//! anywhere). It is the hook for a future `hacer(accion)` tool.

pub mod anim;
pub mod body;
pub mod bubble;
#[cfg(target_os = "macos")]
mod mac;
pub mod menu;
pub mod mood;
pub mod pack;
pub mod place;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

pub use mood::{Flash, Mood};

/// What the program tells the body. Sent from any thread.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// The dialogue's look changed.
    Mood(Mood),
    /// Something happened that deserves a short reaction.
    Flash(Flash),
    /// What the owner said, once transcribed.
    Heard(String),
    /// What Wawi is saying now.
    Said(String),
    /// The microphone gate: `true` = not listening (the key does nothing).
    Muted(bool),
    /// Play an action of the active pet by name (or, for previews, any of
    /// its animations). Unknown names are ignored.
    Play(String),
    /// Save a picture of her as she is drawn now, as PNG (`mascota ver
    /// --foto`; works with the screen locked too).
    Snapshot(PathBuf),
    /// Lift her this many points and let go (previews; a pet with a body).
    Drop(f64),
    /// Go for a walk now (previews; a pet with a body).
    Wander,
    /// Ask the owner for a secret (a plugin's API key) in a native dialog
    /// with a hidden field. The answer goes to [`SecretAsk::reply`] only:
    /// the body never logs, shows or keeps it.
    AskSecret(SecretAsk),
    /// The program is done: close the windows and return from [`run`].
    Close,
}

/// Where a [`SecretAsk`]'s answer goes: `Some(text)` or `None` (cancelled).
#[derive(Clone)]
pub struct SecretReply(pub Arc<dyn Fn(Option<String>) + Send + Sync>);

impl fmt::Debug for SecretReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretReply")
    }
}

impl PartialEq for SecretReply {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A secure input dialog: a title («Clave de Linear»), a sentence, an
/// optional «Dónde la consigo» link, and where the answer goes.
#[derive(Clone, Debug, PartialEq)]
pub struct SecretAsk {
    pub title: String,
    pub message: String,
    pub help_url: Option<String>,
    pub reply: SecretReply,
}

/// What the owner asked for from the menu, for the program to carry out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// `true` = listen to the key again, `false` = ignore it.
    SetListening(bool),
    /// End the program, the same way as Enter or Ctrl+C.
    Quit,
    /// «Ajustes…»: open the settings window (a hook; nothing yet).
    OpenSettings,
}

/// The program's end of the body: a channel plus a way to wake the UI thread.
#[derive(Clone)]
pub struct BodyLink {
    tx: Sender<Update>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl BodyLink {
    pub fn new(tx: Sender<Update>, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { tx, wake }
    }

    /// A link whose updates simply pile up in the returned receiver (tests,
    /// and the terminal mode, which has no body).
    pub fn detached() -> (Self, Receiver<Update>) {
        let (tx, rx) = mpsc::channel();
        (Self::new(tx, Arc::new(|| {})), rx)
    }

    pub fn send(&self, update: Update) {
        if self.tx.send(update).is_ok() {
            (self.wake)();
        }
    }

    pub fn close(&self) {
        self.send(Update::Close);
    }

    /// Plays the active pet's action `name` (see [`pack::Pack::actions`]).
    pub fn play(&self, name: &str) {
        self.send(Update::Play(name.to_owned()));
    }
}

impl fmt::Debug for BodyLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BodyLink")
    }
}

/// Where the body finds its art and keeps its files.
#[derive(Clone, Debug)]
pub struct BodyConfig {
    /// The built-in packs (`assets/mascotas`, with `wawi/` inside).
    pub assets: PathBuf,
    /// Wawi's home (`~/.wawi2`): its `mascotas/` folder holds the packs
    /// people install.
    pub home: Option<PathBuf>,
    /// Where she stands, whether she is shown and which pet is on screen
    /// (`~/.wawi2/mascot.json`).
    pub state_file: Option<PathBuf>,
    /// What «Abrir registros» opens (`~/.wawi2/logs`).
    pub logs_dir: Option<PathBuf>,
    /// Show this pack instead of the saved one (`--mascota DIR`,
    /// `$WAWI2_MASCOTA`); the choice is not saved.
    pub pet_dir: Option<PathBuf>,
}

impl BodyConfig {
    /// The usual files for a home folder, and `$WAWI2_MASCOTA` if set.
    pub fn for_home(home: Option<&Path>) -> Self {
        Self {
            assets: default_assets(),
            home: home.map(Path::to_owned),
            state_file: home.map(|dir| dir.join("mascot.json")),
            logs_dir: home.map(|dir| dir.join("logs")),
            pet_dir: std::env::var_os("WAWI2_MASCOTA").map(PathBuf::from),
        }
    }

    /// The pet that goes on screen (see [`pack::active`]).
    pub fn active_pack(&self, log: &mut dyn FnMut(String)) -> Option<pack::Pack> {
        pack::active(
            &self.assets,
            self.home.as_deref(),
            self.pet_dir.as_deref(),
            log,
        )
    }
}

/// The built-in packs: `$WAWI2_ASSETS/mascotas`, else Wawi.app's
/// `Contents/Resources/mascotas`, else next to the source tree (`cargo run`).
pub fn default_assets() -> PathBuf {
    if let Some(dir) = std::env::var_os("WAWI2_ASSETS") {
        return PathBuf::from(dir).join("mascotas");
    }
    // In the app: `Wawi.app/Contents/MacOS/wawi2` next to `../Resources`.
    let bundled = std::env::current_exe().ok().and_then(|exe| {
        exe.parent()
            .filter(|dir| dir.ends_with("Contents/MacOS"))
            .map(|dir| dir.join("../Resources/mascotas"))
    });
    if let Some(dir) = bundled.filter(|dir| dir.is_dir()) {
        return dir.canonicalize().unwrap_or(dir);
    }
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/mascotas");
    if source.is_dir() {
        return source.canonicalize().unwrap_or(source);
    }
    // Installed: `<prefix>/bin/wawi2` next to `<prefix>/assets/mascotas`.
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|bin| bin.join("../assets/mascotas")))
        .unwrap_or(source)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyError {
    /// This OS has no backend yet.
    Unsupported,
    /// Not called on the main thread.
    NotMainThread,
}

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BodyError::Unsupported => {
                f.write_str("el cuerpo de Wawi todavía no existe en este sistema")
            }
            BodyError::NotMainThread => {
                f.write_str("el cuerpo de Wawi tiene que correr en el hilo principal")
            }
        }
    }
}

impl std::error::Error for BodyError {}

/// Whether this OS has a body backend.
pub const AVAILABLE: bool = cfg!(target_os = "macos");

/// Called on the UI thread for every menu pick that the program must handle.
pub type OnCommand = Box<dyn Fn(Command, &BodyLink)>;

/// Runs the body on the main thread until [`Update::Close`]. `start` gets the
/// link right away (call it to spawn the worker); `on_command` gets the
/// owner's menu picks.
pub fn run(
    config: BodyConfig,
    on_command: OnCommand,
    start: impl FnOnce(BodyLink),
) -> Result<(), BodyError> {
    #[cfg(target_os = "macos")]
    {
        mac::run(config, on_command, start)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (config, on_command, start);
        Err(BodyError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detached_link_keeps_what_it_is_sent() {
        let (link, rx) = BodyLink::detached();
        link.send(Update::Mood(Mood::Thinking));
        link.close();
        assert_eq!(rx.try_recv(), Ok(Update::Mood(Mood::Thinking)));
        assert_eq!(rx.try_recv(), Ok(Update::Close));
    }

    #[test]
    fn the_link_wakes_the_ui_on_every_update() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let woken = Arc::new(AtomicUsize::new(0));
        let (tx, _rx) = mpsc::channel();
        let count = woken.clone();
        let link = BodyLink::new(
            tx,
            Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }),
        );
        link.send(Update::Muted(true));
        link.send(Update::Heard("hola".into()));
        assert_eq!(woken.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_source_tree_has_the_art() {
        let assets = default_assets();
        assert!(
            assets.join("wawi/cuadros/idle_a.png").is_file(),
            "{assets:?}"
        );
        assert!(assets.join("wawi/wawi_menubar.png").is_file());
        assert!(assets.join("wawi/mascota.toml").is_file());
    }

    #[test]
    fn play_sends_the_action_by_name() {
        let (link, rx) = BodyLink::detached();
        link.play("saludar");
        assert_eq!(rx.try_recv(), Ok(Update::Play("saludar".into())));
    }
}
