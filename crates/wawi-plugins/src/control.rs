//! The plugin API for screens and voice: what is installed and how it is
//! doing, what Wawi suggests adding, and the owner's actions (add from a
//! URL, connect, disconnect, turn on or off, remove).
//!
//! [`PluginControl`] is a cheap handle, `Clone + Send + Sync`, for any
//! thread (a settings window on the UI thread, say). Nothing in it blocks:
//!
//! - Reads ([`PluginControl::list`], [`PluginControl::suggested`]) copy a
//!   snapshot under a lock the host holds only to swap it.
//! - Actions only post a [`Request`] to the host's channel and return. The
//!   host does the file work on its thread and anything slow (the browser
//!   login, probing a URL, the Keychain) on short-lived threads.
//! - Changes come back through [`PluginControl::subscribe`]: the callback
//!   gets the whole new list every time a plugin's state changes. It runs
//!   on the host thread, so it must only hand the list over (post it to
//!   the UI's queue) and return.
//!
//! The voice tools [`ConnectTool`] (`conectar`) and [`ListTool`]
//! (`plugins`) are thin users of the same API. Both are owner-only: the
//! core's policy keeps any `Write` tool out of initiative turns.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wawi_core::{Event, PermissionLevel, Tool, ToolCall, ToolOutcome, ToolSpec, TurnId};

use crate::catalog::Suggestion;
use crate::{PluginEvent, PluginSink};

/// How a plugin is doing, as the owner sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginState {
    /// Connected: its tools are in Wawi's hands.
    Conectado,
    /// On its way: starting, or waiting for the browser or the key.
    Conectando,
    /// Needs the owner: never authorized, signed out, or the server
    /// revoked the permission.
    Desconectado,
    /// Off, or not installed (Wawi's own plugins).
    Apagado,
    /// Something is wrong: its program is missing, or it keeps failing.
    Error,
}

impl PluginState {
    pub fn label(self) -> &'static str {
        match self {
            PluginState::Conectado => "Conectado",
            PluginState::Conectando => "Conectando",
            PluginState::Desconectado => "Desconectado",
            PluginState::Apagado => "Apagado",
            PluginState::Error => "Error",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginKind {
    /// Ships with Wawi (instinto, caracter, curiosidad, autoarreglo).
    Propio,
    /// A server the owner added (a connector, from a URL).
    Conector,
}

/// One row for a settings screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginInfo {
    pub name: String,
    /// How to show it: «Linear», «Instinto».
    pub title: String,
    pub description: String,
    pub kind: PluginKind,
    /// It has a folder in `<home>/plugins`.
    pub installed: bool,
    /// Its plugin.toml says `enabled = true` (and it is installed).
    pub enabled: bool,
    pub state: PluginState,
    /// "ninguna", "oauth" or "clave".
    pub auth: &'static str,
    /// The server's address, for connectors.
    pub url: Option<String>,
    /// Why it is not connected, or how to fix it (a missing program).
    pub detail: Option<String>,
    /// Tools it gives the brain, when connected.
    pub tools: usize,
    /// A health check («Revisar») is running for it right now.
    pub checking: bool,
    /// What its last health check found, if one ran since its state last
    /// changed: `Ok(())` it answered, `Err(why)` it did not (no secrets).
    pub check: Option<Result<(), String>>,
}

/// Where to answer when the action came from the `conectar` tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub turn: TurnId,
    pub call_id: String,
}

/// An action for the host. Every one answers `reply` exactly once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// `target` is an installed plugin, one of Wawi's own, a suggestion
    /// (by name or title) or a server URL (added first).
    Connect {
        target: String,
        reply: Option<Reply>,
    },
    /// Forgets its credential (Keychain) and closes it. It stays installed.
    Disconnect { name: String, reply: Option<Reply> },
    /// Turns it on (installing one of Wawi's own if needed) or off.
    SetEnabled {
        name: String,
        enabled: bool,
        reply: Option<Reply>,
    },
    /// Closes it, forgets its credential and moves its folder away.
    Remove { name: String, reply: Option<Reply> },
    /// A health check: asks the running plugin `name` (every running one
    /// with `None`) to answer `tools/list` with a short timeout, renewing an
    /// OAuth token first if it is about to expire. A plugin whose check is
    /// already in flight joins it: never two at once for the same plugin.
    Check { name: Option<String> },
}

type Listener = Box<dyn Fn(&[PluginInfo]) + Send + Sync>;

#[derive(Default)]
pub(crate) struct Shared {
    pub infos: Mutex<Vec<PluginInfo>>,
    pub listeners: Mutex<Vec<Listener>>,
    pub suggestions: Vec<Suggestion>,
}

impl Shared {
    pub fn new(suggestions: Vec<Suggestion>) -> Self {
        Self {
            suggestions,
            ..Self::default()
        }
    }

    /// Swaps the snapshot and tells the listeners, if it changed.
    pub fn publish(&self, infos: Vec<PluginInfo>) {
        let changed = match self.infos.lock() {
            Ok(mut current) if *current != infos => {
                *current = infos.clone();
                true
            }
            _ => false,
        };
        if changed && let Ok(listeners) = self.listeners.lock() {
            for listener in listeners.iter() {
                listener(&infos);
            }
        }
    }
}

/// The handle. See the module docs.
#[derive(Clone)]
pub struct PluginControl {
    pub(crate) sink: Arc<dyn PluginSink>,
    pub(crate) shared: Arc<Shared>,
}

impl PluginControl {
    /// Installed plugins and Wawi's own (installed or not), with state.
    pub fn list(&self) -> Vec<PluginInfo> {
        self.shared
            .infos
            .lock()
            .map(|infos| infos.clone())
            .unwrap_or_default()
    }

    /// Suggested connectors that are not installed yet.
    pub fn suggested(&self) -> Vec<Suggestion> {
        let installed: Vec<String> = self
            .list()
            .into_iter()
            .filter(|p| p.installed)
            .flat_map(|p| [Some(p.name), p.url])
            .flatten()
            .collect();
        self.shared
            .suggestions
            .iter()
            .filter(|s| !installed.contains(&s.name) && !installed.contains(&s.url))
            .cloned()
            .collect()
    }

    /// Adds a connector from its URL and connects it (the browser opens if
    /// it uses OAuth; the key dialog if it uses a key).
    pub fn add_url(&self, url: &str) {
        self.connect(url);
    }

    /// Connects an installed plugin, one of Wawi's own, a suggestion or a URL.
    pub fn connect(&self, target: &str) {
        self.send(Request::Connect {
            target: target.to_owned(),
            reply: None,
        });
    }

    pub fn disconnect(&self, name: &str) {
        self.send(Request::Disconnect {
            name: name.to_owned(),
            reply: None,
        });
    }

    pub fn set_enabled(&self, name: &str, enabled: bool) {
        self.send(Request::SetEnabled {
            name: name.to_owned(),
            enabled,
            reply: None,
        });
    }

    pub fn remove(&self, name: &str) {
        self.send(Request::Remove {
            name: name.to_owned(),
            reply: None,
        });
    }

    /// Health check for every running plugin (see [`Request::Check`]). The
    /// result shows in [`PluginInfo::checking`] and [`PluginInfo::check`].
    pub fn check_all(&self) {
        self.send(Request::Check { name: None });
    }

    /// Health check for one plugin.
    pub fn check(&self, name: &str) {
        self.send(Request::Check {
            name: Some(name.to_owned()),
        });
    }

    /// `on_change` gets the whole list after every change, on the host
    /// thread: hand it over and return.
    pub fn subscribe(&self, on_change: impl Fn(&[PluginInfo]) + Send + Sync + 'static) {
        if let Ok(mut listeners) = self.shared.listeners.lock() {
            listeners.push(Box::new(on_change));
        }
    }

    pub fn send(&self, request: Request) {
        self.sink.plugin(PluginEvent::Control(request));
    }
}

/// The owner's answer to a [`KeyPrompt`]: `Some(key)` or `None` (cancel).
#[derive(Clone)]
pub struct KeyAnswer(pub(crate) Arc<dyn Fn(Option<String>) + Send + Sync>);

impl KeyAnswer {
    pub fn new(answer: impl Fn(Option<String>) + Send + Sync + 'static) -> Self {
        Self(Arc::new(answer))
    }

    /// Hand the key over and forget it: it goes to the Keychain only.
    pub fn answer(&self, key: Option<String>) {
        (self.0)(key);
    }
}

impl std::fmt::Debug for KeyAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeyAnswer")
    }
}

/// A request to show the secure key dialog (a native window with a hidden
/// field). The key never passes through the host, the brain or a log.
#[derive(Clone, Debug)]
pub struct KeyPrompt {
    pub plugin: String,
    /// «Clave de Linear».
    pub title: String,
    pub message: String,
    /// Where to create the key.
    pub help: Option<String>,
    pub answer: KeyAnswer,
}

/// Shows a [`KeyPrompt`] (the app wires it to the body's dialog).
pub type KeyPrompter = Box<dyn Fn(KeyPrompt)>;

fn finish(sink: &Arc<dyn PluginSink>, turn: TurnId, call_id: &str, outcome: ToolOutcome) {
    sink.core(Event::ToolFinished {
        turn,
        call_id: call_id.to_owned(),
        outcome,
    });
}

pub(crate) fn answer(
    sink: &Arc<dyn PluginSink>,
    reply: Option<Reply>,
    outcome: Result<String, String>,
) {
    if let Some(reply) = reply {
        finish(
            sink,
            reply.turn,
            &reply.call_id,
            match outcome {
                Ok(text) => ToolOutcome::Success(text),
                Err(text) => ToolOutcome::Failure(text),
            },
        );
    }
}

/// `conectar(plugin)`: kind act without confirmation, because the browser
/// (or the key dialog) is the confirmation. Owner turns only.
pub struct ConnectTool {
    spec: ToolSpec,
    control: PluginControl,
}

impl ConnectTool {
    pub const NAME: &'static str = "conectar";

    pub fn new(control: PluginControl) -> Self {
        Self {
            spec: ToolSpec {
                name: Self::NAME.into(),
                description: "Conecta un plugin o un servicio (por ejemplo Linear) cuando el dueño lo pide: abre el navegador para que dé permiso, o una ventanita segura para pegar una clave. Acepta el nombre de un plugin instalado, uno de Wawi (instinto, caracter, curiosidad, autoarreglo), uno sugerido o la dirección https del servidor MCP. Nunca le pidas al dueño una clave en voz alta.".into(),
                params_schema_json: json!({
                    "type": "object",
                    "properties": { "plugin": { "type": "string", "description": "nombre del plugin o dirección https del servidor" } },
                    "required": ["plugin"],
                })
                .to_string(),
                permission: PermissionLevel::Write,
                needs_confirmation: false,
                ack: None,
            },
            control,
        }
    }
}

impl Tool for ConnectTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn start(&mut self, turn: TurnId, call: &ToolCall) {
        let target = serde_json::from_str::<Value>(&call.args_json)
            .ok()
            .and_then(|v| v.get("plugin").and_then(Value::as_str).map(str::to_owned))
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty());
        let Some(target) = target else {
            return finish(
                &self.control.sink,
                turn,
                &call.id,
                ToolOutcome::Failure("falta plugin: qué querés conectar".into()),
            );
        };
        self.control.send(Request::Connect {
            target,
            reply: Some(Reply {
                turn,
                call_id: call.id.clone(),
            }),
        });
    }

    fn cancel(&mut self, _turn: TurnId, _call_id: &str) {}
}

/// `plugins()`: what is installed, how it is doing, and what can be added.
/// A `Write` tool on purpose, so only the owner's turns can use it.
pub struct ListTool {
    spec: ToolSpec,
    control: PluginControl,
}

impl ListTool {
    pub const NAME: &'static str = "plugins";

    pub fn new(control: PluginControl) -> Self {
        Self {
            spec: ToolSpec {
                name: Self::NAME.into(),
                description: "Lista los plugins de Wawi: cuáles están conectados, desconectados o apagados, y cuáles se pueden agregar. Usala cuando el dueño pregunta qué tenés conectado o qué podés conectar.".into(),
                params_schema_json: json!({ "type": "object", "properties": {} }).to_string(),
                permission: PermissionLevel::Write,
                needs_confirmation: false,
                ack: None,
            },
            control,
        }
    }
}

/// The text `plugins()` gives the brain.
pub fn describe(infos: &[PluginInfo], suggested: &[Suggestion]) -> String {
    let mut lines = Vec::new();
    for state in [
        PluginState::Conectado,
        PluginState::Conectando,
        PluginState::Desconectado,
        PluginState::Error,
        PluginState::Apagado,
    ] {
        let rows: Vec<String> = infos
            .iter()
            .filter(|p| p.state == state)
            .map(|p| {
                let mut row = p.title.clone();
                if let Some(detail) = &p.detail {
                    row.push_str(&format!(" ({detail})"));
                }
                row
            })
            .collect();
        if !rows.is_empty() {
            lines.push(format!("{}: {}.", state.label(), rows.join("; ")));
        }
    }
    if !suggested.is_empty() {
        let rows: Vec<String> = suggested
            .iter()
            .map(|s| format!("{} ({})", s.title, s.description.trim_end_matches('.')))
            .collect();
        lines.push(format!("Se pueden agregar: {}.", rows.join("; ")));
    }
    lines.push("Para conectar uno, usá conectar con su nombre o la dirección del servidor.".into());
    lines.join("\n")
}

impl Tool for ListTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn start(&mut self, turn: TurnId, call: &ToolCall) {
        let text = describe(&self.control.list(), &self.control.suggested());
        finish(
            &self.control.sink,
            turn,
            &call.id,
            ToolOutcome::Success(text),
        );
    }

    fn cancel(&mut self, _turn: TurnId, _call_id: &str) {}
}
