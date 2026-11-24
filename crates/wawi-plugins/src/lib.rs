//! Wawi's plugin host. Every capability beyond the base is a plugin: a
//! folder `<home>/plugins/<name>/` with a `plugin.toml` (and maybe an
//! `instrucciones.md`) that points at an MCP server, local (stdio) or
//! remote (streamable HTTP). docs/PLUGINS.md is the reference for authors.
//!
//! - **Proxy.** Wawi is the MCP client. A plugin's tools become ordinary
//!   core tools named `<plugin>__<tool>`, so the brain reaches them through
//!   Wawi's own in-process `wawi` server and every call passes the core's
//!   policy (read or act, confirmation, initiative read-only).
//! - **Instinct.** `[[watch]]` calls a tool on a schedule with no model at
//!   all; a result that says something becomes an instinct candidate.
//!   `[[routine]]` becomes one at its time. A plugin can also push one
//!   (`notifications/wawi/instinto`), and any process can drop one in the
//!   file inbox (`<home>/cola/hechos`). The core's instinct gate decides.
//! - **Turn feed.** A plugin with `observa_charla = true` gets every
//!   finished turn (owner text and Wawi text only), fire and forget.
//! - **Robust.** One thread per plugin, a timeout on every call, restarts
//!   with backoff, every call logged without secrets. A broken plugin never
//!   takes Wawi down.
//!
//! - **Easy to connect.** Wawi's own plugins ship with her ([`catalog`]);
//!   connectors are remote MCP servers added from a URL. «Conectar» runs
//!   the MCP OAuth login in the browser ([`oauth`]) or asks for a key in a
//!   secure dialog, and keeps the credential in the Keychain
//!   ([`keychain`]). [`control::PluginControl`] is the API for a settings
//!   screen; the `conectar` and `plugins` tools are the voice side. A
//!   plugin turned on or connected loads while Wawi runs.
//!
//! Everything here runs on the host's thread except the workers and the
//! short-lived login and probe threads; results come back through a
//! [`PluginSink`].

pub mod avisos;
pub mod catalog;
pub mod control;
pub mod fallos;
pub mod feed;
pub mod inbox;
pub mod keychain;
pub mod log;
pub mod manifest;
pub mod mcp;
pub mod oauth;
pub mod preguntas;
pub mod schedule;
pub mod secrets;
pub mod tool;
mod worker;

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wawi_core::{
    Event, InstinctCandidate, InstinctOutcome, Priority, Registry, SpeechKind, Tool, TurnId,
};
use wawi_tools::{Civil, WallClock};

use crate::catalog::{BUILTIN, Suggestion};
use crate::control::{
    ConnectTool, KeyAnswer, KeyPrompt, KeyPrompter, ListTool, PluginControl, PluginInfo,
    PluginKind, PluginState, Reply, Request, Shared, answer,
};
use crate::feed::{FeedItem, TurnFeed};
use crate::inbox::Inbox;
use crate::keychain::{OAUTH_ACCOUNT, OAUTH_CLIENT_ACCOUNT, SecretStore, Vault};
use crate::log::CallLog;
use crate::manifest::{Auth, Manifest, Server, truncate};
use crate::oauth::Grant;
use crate::schedule::{Entry, Scheduler, When, expand, minute_of_day};
use crate::secrets::Secrets;
use crate::tool::{PluginTool, PluginToolSpec};
use crate::worker::{FeedQueue, Job, Origin, Worker, push_feed};

pub use worker::backoff;

/// The notification a plugin sends to push an instinct.
pub const PUSH_METHOD: &str = "notifications/wawi/instinto";
/// What the brain is told to do with a pushed instinct. The plugin's text
/// is data, never the instruction.
pub const PUSH_INSTRUCTION: &str = "Un plugin te pasó este aviso para el dueño. Si vale la pena, decíselo en una o dos frases, con tus palabras. Si no, no digas nada.";
/// Where the scheduler remembers which `at` entries already ran today.
pub const SCHEDULE_STATE: &str = "plugins/estado.json";
/// How often the inbox folder is read.
pub const INBOX_EVERY_S: i64 = 2;

/// Where the plugin host sends what happens off the host thread. The host
/// puts both into its one channel.
pub trait PluginSink: Send + Sync {
    /// For the core: a plugin tool finished.
    fn core(&self, event: Event);
    /// For the plugin host: give it back with [`PluginHost::on_event`].
    fn plugin(&self, event: PluginEvent);
}

#[derive(Clone, Debug, PartialEq)]
pub enum PluginEvent {
    /// Connected (or reconnected): these are its tools now.
    Ready {
        plugin: String,
        tools: Vec<PluginToolSpec>,
    },
    /// Down; it retries on its own.
    Down { plugin: String, reason: String },
    /// A watch's call came back.
    WatchResult {
        plugin: String,
        key: String,
        result: Result<String, String>,
    },
    /// The plugin pushed an instinct.
    Push { plugin: String, params: Value },
    /// Needs the owner: no credential, or the server refused or revoked it.
    /// It waits (no retries) until it is connected again.
    NeedsAuth { plugin: String, reason: String },
    /// An action from a settings screen or the `conectar` tool.
    Control(Request),
    /// The browser login ended; on success the grant is in the Keychain.
    LoginDone {
        plugin: String,
        result: Result<(), String>,
    },
    /// A connector from a URL was probed and written (or not).
    Added {
        plugin: String,
        reply: Option<Reply>,
        result: Result<(), String>,
    },
    /// The key dialog ended: `Ok(true)` saved, `Ok(false)` cancelled.
    KeySaved {
        plugin: String,
        result: Result<bool, String>,
    },
}

/// Opens a URL in the owner's browser.
pub type Opener = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// What the host uses from the outside world, injectable for tests.
pub struct Services {
    /// Where credentials live (the Keychain on macOS).
    pub store: Arc<dyn SecretStore>,
    pub open: Opener,
    /// Wawi's own plugins ([`catalog::default_dir`]).
    pub catalog: Option<PathBuf>,
    pub suggestions: Vec<Suggestion>,
    pub login_timeout: Duration,
}

impl Services {
    /// The Keychain, the real browser, the bundled catalog and suggestions.
    pub fn system() -> Self {
        Self {
            store: keychain::system(),
            open: Arc::new(oauth::open_browser),
            catalog: catalog::default_dir(),
            suggestions: catalog::suggestions(),
            login_timeout: oauth::LOGIN_TIMEOUT,
        }
    }
}

#[derive(Clone, Debug)]
pub struct HostOptions {
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
    /// Feed items a slow plugin may have waiting; beyond it the oldest go.
    pub feed_cap: usize,
    /// The longest watch result or pushed context given to the brain.
    pub max_context: usize,
}

impl Default for HostOptions {
    fn default() -> Self {
        Self {
            backoff_initial: Duration::from_secs(2),
            backoff_max: Duration::from_secs(300),
            feed_cap: 32,
            max_context: 4000,
        }
    }
}

struct Running {
    manifest: Manifest,
    jobs: Sender<Job>,
    feed: FeedQueue,
    ready: bool,
    /// It connected or failed at least once.
    settled: bool,
    /// Names registered in the core for this plugin.
    registered: Vec<String>,
    thread: Option<JoinHandle<()>>,
}

pub struct PluginHost {
    clock: Rc<dyn WallClock>,
    sink: Arc<dyn PluginSink>,
    log: CallLog,
    options: HostOptions,
    home: PathBuf,
    services: Services,
    secrets: Secrets,
    /// Every plugin under `<home>/plugins`, on or off.
    installed: Vec<Manifest>,
    /// The ones with a worker.
    plugins: Vec<Running>,
    shared: Arc<Shared>,
    prompter: Option<KeyPrompter>,
    /// A login, a key dialog or a probe under way, and what it is.
    pending: BTreeMap<String, String>,
    /// Waiting for the owner to connect it, and why.
    waiting: BTreeMap<String, String>,
    /// The last failure of a running plugin.
    errors: BTreeMap<String, String>,
    /// Plugins whose program is missing, and how to get it.
    missing: BTreeMap<String, String>,
    tools_changed: bool,
    scheduler: Scheduler,
    problems: Vec<String>,
    feed: TurnFeed,
    /// Questions Wawi asked, until the owner answers or they go unanswered.
    questions: preguntas::Tracker,
    /// Plugin downs, to spot a crash loop worth a notice.
    crashes: avisos::CrashWatch,
    inbox: Option<Inbox>,
    last_inbox_scan: Option<i64>,
    next_push: u64,
}

impl PluginHost {
    /// Finds the plugins under `<home>/plugins`, starts a worker for each
    /// enabled one (they connect in the background) and opens the inbox.
    /// Problems (a bad plugin.toml, a secrets file with loose permissions)
    /// are kept for [`Self::problems`]; nothing here fails. Credentials
    /// come from the Keychain.
    pub fn start(
        home: &Path,
        logs_dir: Option<&Path>,
        clock: Rc<dyn WallClock>,
        sink: Arc<dyn PluginSink>,
        options: HostOptions,
    ) -> Self {
        Self::start_with(home, logs_dir, clock, sink, options, Services::system())
    }

    /// [`Self::start`] with its outside world given (tests: a memory store,
    /// a fake browser).
    pub fn start_with(
        home: &Path,
        logs_dir: Option<&Path>,
        clock: Rc<dyn WallClock>,
        sink: Arc<dyn PluginSink>,
        options: HostOptions,
        services: Services,
    ) -> Self {
        let log = CallLog::open(logs_dir);
        let (manifests, mut problems) = manifest::discover(home);
        let (secrets, secrets_problem) = Secrets::load(home);
        problems.extend(secrets_problem);
        let inbox = match Inbox::open(home) {
            Ok(inbox) => Some(inbox),
            Err(error) => {
                problems.push(format!(
                    "no pude crear la cola en {}: {error}",
                    inbox::FACTS
                ));
                None
            }
        };
        let _ = std::fs::create_dir_all(home.join("plugins"));
        let shared = Arc::new(Shared::new(services.suggestions.clone()));
        let mut host = Self {
            scheduler: Scheduler::new(Vec::new(), Some(home.join(SCHEDULE_STATE))),
            clock,
            sink,
            log,
            options,
            home: home.to_path_buf(),
            services,
            secrets,
            installed: Vec::new(),
            plugins: Vec::new(),
            shared,
            prompter: None,
            pending: BTreeMap::new(),
            waiting: BTreeMap::new(),
            errors: BTreeMap::new(),
            missing: BTreeMap::new(),
            tools_changed: false,
            problems,
            feed: TurnFeed::default(),
            questions: preguntas::Tracker::default(),
            crashes: avisos::CrashWatch::default(),
            inbox,
            last_inbox_scan: None,
            next_push: 0,
        };
        for manifest in manifests {
            host.installed.push(manifest.clone());
            if manifest.enabled {
                host.start_worker(manifest);
            }
        }
        for problem in &host.problems {
            host.log.line(&format!("problema: {problem}"));
        }
        host.publish();
        host
    }

    /// Starts a plugin's worker (it connects in the background) and its
    /// watches and routines. A plugin whose program is missing is marked,
    /// not started.
    fn start_worker(&mut self, manifest: Manifest) {
        let name = manifest.name.clone();
        if self.plugins.iter().any(|p| p.manifest.name == name) {
            return;
        }
        if let Server::Stdio { command, .. } = &manifest.server
            && catalog::resolve_command(command, &manifest.dir).is_none()
        {
            self.log
                .line(&format!("{name}: falta el programa {command}"));
            self.missing.insert(name, catalog::how_to_get(command));
            return;
        }
        self.missing.remove(&name);
        let mut entries = Vec::new();
        for watch in &manifest.watches {
            entries.push(Entry {
                key: format!("{name}/{}", watch.name),
                plugin: name.clone(),
                when: watch.when,
            });
        }
        for routine in &manifest.routines {
            entries.push(Entry {
                key: format!("{name}/{}", routine.name),
                plugin: name.clone(),
                when: When::At(routine.at),
            });
        }
        let (jobs, rx) = mpsc::channel();
        let feed: FeedQueue = Arc::new(Mutex::new(VecDeque::new()));
        let worker = Worker::new(
            manifest.clone(),
            self.secrets.clone(),
            self.sink.clone(),
            self.log.clone(),
            jobs.clone(),
            self.options.clone(),
            feed.clone(),
            Vault::new(self.services.store.clone(), &name),
        );
        let thread = std::thread::Builder::new()
            .name(format!("wawi-plugin-{name}"))
            .spawn(move || worker.run(rx));
        match thread {
            Ok(thread) => {
                self.scheduler.add(entries);
                self.plugins.push(Running {
                    manifest,
                    jobs,
                    feed,
                    ready: false,
                    settled: false,
                    registered: Vec::new(),
                    thread: Some(thread),
                });
            }
            Err(error) => self
                .problems
                .push(format!("{name}: no pude arrancar su hilo: {error}")),
        }
    }

    /// Closes a plugin's worker (without waiting for it) and takes its tools
    /// out of the registry.
    fn stop_worker(&mut self, name: &str, registry: &mut Registry) {
        if let Some(index) = self.plugins.iter().position(|p| p.manifest.name == name) {
            let mut running = self.plugins.remove(index);
            for tool in running.registered.drain(..) {
                registry.remove(&tool);
                self.tools_changed = true;
            }
            let _ = running.jobs.send(Job::Shutdown);
        }
        self.scheduler.remove_plugin(name);
        self.errors.remove(name);
    }

    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// Enabled plugins, by name.
    pub fn names(&self) -> Vec<&str> {
        self.plugins
            .iter()
            .map(|p| p.manifest.name.as_str())
            .collect()
    }

    /// Installed plugins with `enabled = false`.
    pub fn disabled(&self) -> Vec<String> {
        self.installed
            .iter()
            .filter(|m| !m.enabled)
            .map(|m| m.name.clone())
            .collect()
    }

    /// The handle for settings screens (see [`control`]).
    pub fn control(&self) -> PluginControl {
        PluginControl {
            sink: self.sink.clone(),
            shared: self.shared.clone(),
        }
    }

    /// The voice tools `conectar` and `plugins`, for the core's registry.
    pub fn base_tools(&self) -> Vec<Box<dyn Tool>> {
        vec![
            Box::new(ConnectTool::new(self.control())),
            Box::new(ListTool::new(self.control())),
        ]
    }

    /// How to show the secure key dialog. Without one, key plugins can only
    /// use secrets.toml.
    pub fn set_key_prompter(&mut self, prompter: KeyPrompter) {
        self.prompter = Some(prompter);
    }

    /// Whether plugin tools came or went since the last call (the brain
    /// should get ready for the new list, off the turn path).
    pub fn take_tools_changed(&mut self) -> bool {
        std::mem::take(&mut self.tools_changed)
    }

    /// Every enabled plugin connected or failed at least once: the tool
    /// list is as complete as it will be soon.
    pub fn settled(&self) -> bool {
        self.plugins.iter().all(|p| p.settled)
    }

    pub fn is_ready(&self, plugin: &str) -> bool {
        self.plugins
            .iter()
            .any(|p| p.manifest.name == plugin && p.ready)
    }

    /// For the brain's system prompt: each plugin's `instrucciones.md`,
    /// under its name. Empty when no plugin has any.
    pub fn instructions(&self) -> String {
        let sections: Vec<String> = self
            .plugins
            .iter()
            .filter_map(|p| {
                let text = p.manifest.instructions.as_deref()?;
                let about = if p.manifest.description.trim().is_empty() {
                    String::new()
                } else {
                    format!(" {}", p.manifest.description.trim())
                };
                Some(format!(
                    "## Plugin {name}\nSus herramientas empiezan con {name}__.{about}\n{text}",
                    name = p.manifest.name
                ))
            })
            .collect();
        if sections.is_empty() {
            return String::new();
        }
        format!(
            "# Plugins\nEstas notas dicen cuándo usar cada plugin. Los permisos los decide Wawi, no estas notas.\n\n{}",
            sections.join("\n\n")
        )
    }

    /// Handles what a worker sent. Registers or replaces tools in the core
    /// registry and returns events for the core (instinct candidates).
    pub fn on_event(&mut self, event: PluginEvent, registry: &mut Registry) -> Vec<Event> {
        match event {
            PluginEvent::Ready { plugin, tools } => {
                let Some(running) = self.plugins.iter_mut().find(|p| p.manifest.name == plugin)
                else {
                    return Vec::new();
                };
                for name in running.registered.drain(..) {
                    registry.remove(&name);
                }
                for spec in tools {
                    running.registered.push(spec.spec.name.clone());
                    registry.register(Box::new(PluginTool::new(
                        spec,
                        running.jobs.clone(),
                        self.sink.clone(),
                    )));
                }
                running.ready = true;
                running.settled = true;
                self.tools_changed = true;
                self.crashes.up(&plugin);
                self.waiting.remove(&plugin);
                self.errors.remove(&plugin);
                self.publish();
                Vec::new()
            }
            PluginEvent::Down { plugin, reason } => {
                // Its tools stay registered: a call fails fast with the reason.
                if let Some(running) = self.plugins.iter_mut().find(|p| p.manifest.name == plugin) {
                    running.ready = false;
                    running.settled = true;
                    self.failed(&plugin, &reason, false);
                    self.errors.insert(plugin, reason);
                }
                self.publish();
                Vec::new()
            }
            PluginEvent::NeedsAuth { plugin, reason } => {
                if let Some(running) = self.plugins.iter_mut().find(|p| p.manifest.name == plugin) {
                    running.ready = false;
                    running.settled = true;
                    self.failed(&plugin, &reason, true);
                    self.waiting.insert(plugin, reason);
                }
                self.publish();
                Vec::new()
            }
            PluginEvent::Control(request) => {
                self.request(request, registry);
                self.publish();
                Vec::new()
            }
            PluginEvent::LoginDone { plugin, result } => {
                self.pending.remove(&plugin);
                let events = match result {
                    Ok(()) => {
                        self.log.line(&format!("{plugin}: permiso recibido"));
                        self.waiting.remove(&plugin);
                        self.restart(&plugin);
                        let title = self.title(&plugin);
                        let notice = json!({
                            "id": "conectado",
                            "decir": format!("Ya quedó conectado {title}: el dueño le dio permiso en el navegador."),
                        });
                        self.push(&plugin, &notice).into_iter().collect()
                    }
                    Err(reason) => {
                        self.log
                            .line(&format!("{plugin}: el login no terminó: {reason}"));
                        self.waiting.insert(plugin, reason);
                        Vec::new()
                    }
                };
                self.publish();
                events
            }
            PluginEvent::Added {
                plugin,
                reply,
                result,
            } => {
                self.pending.remove(&plugin);
                let loaded =
                    result.and_then(|()| manifest::load(&self.home.join("plugins").join(&plugin)));
                match loaded {
                    Ok(manifest) => {
                        self.log.line(&format!("{plugin}: agregado"));
                        self.installed.retain(|m| m.name != plugin);
                        self.installed.push(manifest);
                        self.connect(&plugin, reply, registry);
                    }
                    Err(reason) => answer(
                        &self.sink,
                        reply,
                        Err(format!("No pude agregar ese servidor: {reason}")),
                    ),
                }
                self.publish();
                Vec::new()
            }
            PluginEvent::KeySaved { plugin, result } => {
                self.pending.remove(&plugin);
                match result {
                    Ok(true) => {
                        self.log.line(&format!("{plugin}: clave guardada"));
                        self.waiting.remove(&plugin);
                        self.restart(&plugin);
                    }
                    Ok(false) => {
                        self.waiting.insert(plugin, "no pegaste la clave".into());
                    }
                    Err(reason) => {
                        self.waiting.insert(plugin, reason);
                    }
                }
                self.publish();
                Vec::new()
            }
            PluginEvent::WatchResult {
                plugin,
                key,
                result,
            } => self
                .watch_result(&plugin, &key, result)
                .into_iter()
                .collect(),
            PluginEvent::Push { plugin, params } => {
                self.push(&plugin, &params).into_iter().collect()
            }
        }
    }

    // -- connecting, from a settings screen or the `conectar` tool ------------

    /// How the owner sees a plugin's name: the suggestion's title, else the
    /// name with a capital.
    /// A running plugin failed: its last failure is recorded for the
    /// Settings window, and the owner gets a notice once a day when it needs
    /// them (signing in again) or keeps crashing. See `avisos.rs`.
    fn failed(&mut self, plugin: &str, reason: &str, auth: bool) {
        let clock = self.clock.clone();
        let now = clock.local_now();
        let title = self.title(plugin);
        let (tipo, notice) = if auth {
            let notice = avisos::auth_is_news(reason).then(|| {
                format!("{title} se desconectó y necesita que le vuelvas a dar permiso ({reason}). Se arregla diciendo «conectá {plugin}».")
            });
            (fallos::Tipo::Permiso, notice)
        } else if self.crashes.down(plugin, clock.now()) {
            let notice = format!(
                "{title} se cae una y otra vez ({reason}). Sigo reintentando sola, pero puede que haya que revisarlo."
            );
            (fallos::Tipo::Caidas, Some(notice))
        } else {
            (fallos::Tipo::Caida, None)
        };
        fallos::registrar(&self.home, plugin, tipo, reason, &now);
        if let Some(notice) = notice {
            match avisos::plugin_notice(&self.home, plugin, &notice, clock.as_ref()) {
                Ok(true) => self
                    .log
                    .line(&format!("{plugin}: aviso al dueño ({})", tipo.as_str())),
                Ok(false) => {}
                Err(error) => self
                    .log
                    .line(&format!("{plugin}: no pude dejar el aviso: {error}")),
            }
        }
    }

    fn title(&self, name: &str) -> String {
        if let Some(s) = self.services.suggestions.iter().find(|s| s.name == name) {
            return s.title.clone();
        }
        let mut chars = name.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().chain(chars).collect(),
            None => String::new(),
        }
    }

    fn manifest(&self, name: &str) -> Option<&Manifest> {
        self.installed.iter().find(|m| m.name == name)
    }

    /// Reads a plugin's folder again after a change on disk.
    fn reload(&mut self, name: &str) -> Result<Manifest, String> {
        let manifest = manifest::load(&self.home.join("plugins").join(name))?;
        self.installed.retain(|m| m.name != name);
        self.installed.push(manifest.clone());
        self.installed.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(manifest)
    }

    /// A new credential: reconnect the worker, or start it.
    fn restart(&mut self, name: &str) {
        if let Some(running) = self.plugins.iter_mut().find(|p| p.manifest.name == name) {
            running.settled = false;
            let _ = running.jobs.send(Job::Reconnect);
            return;
        }
        if let Some(manifest) = self.manifest(name).filter(|m| m.enabled).cloned() {
            self.start_worker(manifest);
        }
    }

    fn request(&mut self, request: Request, registry: &mut Registry) {
        match request {
            Request::Connect { target, reply } => self.connect_target(&target, reply, registry),
            Request::Disconnect { name, reply } => {
                let Some(manifest) = self.manifest(&name).cloned() else {
                    return answer(
                        &self.sink,
                        reply,
                        Err(format!("No tengo un plugin {name}.")),
                    );
                };
                self.stop_worker(&name, registry);
                self.pending.remove(&name);
                self.waiting.insert(name.clone(), "lo desconectaste".into());
                self.forget_credentials(&manifest);
                self.log.line(&format!("{name}: desconectado por el dueño"));
                answer(
                    &self.sink,
                    reply,
                    Ok(format!(
                        "Listo, desconecté {} y borré su permiso.",
                        self.title(&name)
                    )),
                );
            }
            Request::SetEnabled {
                name,
                enabled,
                reply,
            } => {
                let outcome = self.set_enabled(&name, enabled, registry);
                answer(&self.sink, reply, outcome);
            }
            Request::Remove { name, reply } => {
                let Some(manifest) = self.manifest(&name).cloned() else {
                    return answer(
                        &self.sink,
                        reply,
                        Err(format!("No tengo un plugin {name}.")),
                    );
                };
                self.stop_worker(&name, registry);
                self.forget_credentials(&manifest);
                let outcome = catalog::remove(&self.home, &name).map(|()| {
                    self.installed.retain(|m| m.name != name);
                    for map in [&mut self.pending, &mut self.waiting, &mut self.missing] {
                        map.remove(&name);
                    }
                    self.log.line(&format!("{name}: sacado"));
                    format!("Listo, saqué {}.", self.title(&name))
                });
                answer(&self.sink, reply, outcome);
            }
        }
    }

    /// Deletes the plugin's Keychain items (and tells the server to forget
    /// the OAuth grant), on a short-lived thread: the Keychain may block.
    fn forget_credentials(&self, manifest: &Manifest) {
        let vault = Vault::new(self.services.store.clone(), &manifest.name);
        let key = match &manifest.auth {
            Auth::Key { name, .. } => Some(name.clone()),
            _ => None,
        };
        let _ = std::thread::Builder::new()
            .name("wawi-plugin-forget".into())
            .spawn(move || {
                if let Some(grant) = vault
                    .get(OAUTH_ACCOUNT)
                    .ok()
                    .flatten()
                    .and_then(|text| Grant::from_json(&text))
                {
                    oauth::revoke(&grant);
                }
                let _ = vault.delete(OAUTH_ACCOUNT);
                let _ = vault.delete(OAUTH_CLIENT_ACCOUNT);
                if let Some(key) = key {
                    let _ = vault.delete(&key);
                }
            });
    }

    fn set_enabled(
        &mut self,
        name: &str,
        enabled: bool,
        registry: &mut Registry,
    ) -> Result<String, String> {
        if self.manifest(name).is_none() {
            if !enabled {
                return Err(format!("No tengo un plugin {name}."));
            }
            self.install_builtin(name)?;
        }
        let dir = self.home.join("plugins").join(name);
        catalog::set_enabled(&dir, enabled)?;
        let manifest = self.reload(name)?;
        let title = self.title(name);
        if enabled {
            self.start_worker(manifest);
            match self.missing.get(name) {
                Some(how) => Err(format!("Prendí {title}, pero no puede andar: {how}")),
                None => Ok(format!("Listo, prendí {title}.")),
            }
        } else {
            self.stop_worker(name, registry);
            self.waiting.remove(name);
            Ok(format!("Listo, apagué {title}."))
        }
    }

    fn install_builtin(&mut self, name: &str) -> Result<(), String> {
        let catalog = self
            .services
            .catalog
            .clone()
            .filter(|_| BUILTIN.contains(&name))
            .ok_or_else(|| format!("No tengo un plugin {name} para instalar."))?;
        catalog::install(&catalog, &self.home, name)?;
        self.log
            .line(&format!("{name}: instalado desde el catálogo"));
        self.reload(name).map(|_| ())
    }

    fn connect_target(&mut self, target: &str, reply: Option<Reply>, registry: &mut Registry) {
        let folded = fold(target);
        // «conectá el instinto», «la curiosidad»: the article is not the name.
        let wanted = ["el ", "la ", "los ", "las ", "a "]
            .iter()
            .find_map(|article| folded.strip_prefix(article))
            .map(str::to_owned)
            .unwrap_or(folded);
        let is_url = wanted.starts_with("https://") || wanted.starts_with("http://");
        let same_url = |url: &str| fold(url).trim_end_matches('/') == wanted.trim_end_matches('/');
        // An installed plugin, by name, title or server URL.
        let installed = self.installed.iter().find(|m| {
            fold(&m.name) == wanted
                || fold(&self.title(&m.name)) == wanted
                || matches!(&m.server, Server::Http { url, .. } if same_url(url))
        });
        if let Some(name) = installed.map(|m| m.name.clone()) {
            return self.connect(&name, reply, registry);
        }
        if !is_url && let Some(name) = BUILTIN.iter().find(|b| fold(b) == wanted) {
            return match self.set_enabled(name, true, registry) {
                Ok(_) => self.connect(name, reply, registry),
                Err(error) => answer(&self.sink, reply, Err(error)),
            };
        }
        let suggestion = self
            .services
            .suggestions
            .iter()
            .find(|s| fold(&s.name) == wanted || fold(&s.title) == wanted || same_url(&s.url))
            .cloned();
        match (suggestion, is_url) {
            (Some(suggestion), _) => self.add(&suggestion.url.clone(), Some(suggestion), reply),
            (None, true) => self.add(target.trim(), None, reply),
            (None, false) => answer(
                &self.sink,
                reply,
                Err(format!(
                    "No conozco «{target}». Decime la dirección https de su servidor MCP, o preguntame qué plugins hay."
                )),
            ),
        }
    }

    /// Probes a server and writes its plugin.toml, on a short-lived thread;
    /// `PluginEvent::Added` comes back and the connection goes on.
    fn add(&mut self, url: &str, suggestion: Option<Suggestion>, reply: Option<Reply>) {
        if let Err(error) = oauth::check_secure(url) {
            return answer(
                &self.sink,
                reply,
                Err(format!("No puedo agregarlo: {error}.")),
            );
        }
        let mut taken: Vec<String> = self.installed.iter().map(|m| m.name.clone()).collect();
        taken.extend(BUILTIN.iter().map(|b| b.to_string()));
        taken.extend(self.pending.keys().cloned());
        let name = match &suggestion {
            Some(s) if !taken.contains(&s.name) => s.name.clone(),
            _ => catalog::name_for_url(url, &taken),
        };
        self.pending
            .insert(name.clone(), "revisando el servidor".into());
        let (home, catalog_dir, sink) = (
            self.home.clone(),
            self.services.catalog.clone(),
            self.sink.clone(),
        );
        let url = url.to_owned();
        let plugin = name.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("wawi-plugin-add-{name}"))
            .spawn(move || {
                let description = suggestion
                    .as_ref()
                    .map(|s| s.description.clone())
                    .unwrap_or_else(|| format!("Conector de {url}"));
                let template = suggestion.as_ref().and_then(|s| s.template.clone());
                let result = catalog::probe(&url, &catalog::key_name(&plugin)).and_then(|auth| {
                    catalog::write_connector(
                        &home,
                        catalog_dir.as_deref(),
                        &plugin,
                        &url,
                        &description,
                        template.as_deref(),
                        &auth,
                    )
                    .map(|_| ())
                });
                sink.plugin(PluginEvent::Added {
                    plugin,
                    reply,
                    result,
                });
            });
        if let Err(error) = spawned {
            self.pending.remove(&name);
            self.log
                .line(&format!("no pude arrancar el hilo de {name}: {error}"));
        }
    }

    /// Connects an installed plugin: the browser for OAuth, the dialog for a
    /// key, a reconnect for the rest.
    fn connect(&mut self, name: &str, reply: Option<Reply>, registry: &mut Registry) {
        let Some(mut manifest) = self.manifest(name).cloned() else {
            return answer(
                &self.sink,
                reply,
                Err(format!("No tengo un plugin {name}.")),
            );
        };
        let title = self.title(name);
        if !manifest.enabled {
            match self.set_enabled(name, true, registry) {
                Ok(_) => {
                    if let Some(m) = self.manifest(name) {
                        manifest = m.clone();
                    }
                }
                Err(error) => return answer(&self.sink, reply, Err(error)),
            }
        }
        if let Some(how) = self.missing.get(name) {
            return answer(&self.sink, reply, Err(how.clone()));
        }
        let ready = self
            .plugins
            .iter()
            .any(|p| p.manifest.name == name && p.ready);
        if ready {
            return answer(&self.sink, reply, Ok(format!("{title} ya está conectado.")));
        }
        if let Some(what) = self.pending.get(name) {
            return answer(
                &self.sink,
                reply,
                Ok(format!("Ya estoy en eso con {title}: {what}.")),
            );
        }
        match manifest.auth.clone() {
            Auth::OAuth { client_id, scopes } => {
                let Server::Http { url, .. } = &manifest.server else {
                    return answer(
                        &self.sink,
                        reply,
                        Err("OAuth es solo para servidores con url".into()),
                    );
                };
                self.pending.insert(
                    name.to_owned(),
                    "esperando el permiso en el navegador".into(),
                );
                self.login(name, url.clone(), client_id, scopes, title, reply);
            }
            Auth::Key { name: key, help } => {
                let Some(prompter) = &self.prompter else {
                    return answer(
                        &self.sink,
                        reply,
                        Err(
                            "Para pedirte la clave necesito la ventana de Wawi (sin --no-ui)."
                                .into(),
                        ),
                    );
                };
                self.pending
                    .insert(name.to_owned(), "esperando la clave".into());
                let vault = Vault::new(self.services.store.clone(), name);
                let sink = self.sink.clone();
                let plugin = name.to_owned();
                let answer_key = KeyAnswer::new(move |given: Option<String>| {
                    let given = given.map(|k| k.trim().to_owned()).filter(|k| !k.is_empty());
                    let Some(given) = given else {
                        return sink.plugin(PluginEvent::KeySaved {
                            plugin: plugin.clone(),
                            result: Ok(false),
                        });
                    };
                    // The Keychain may block: not on the UI thread.
                    let (vault, sink, plugin, key) =
                        (vault.clone(), sink.clone(), plugin.clone(), key.clone());
                    let _ = std::thread::Builder::new()
                        .name("wawi-plugin-key".into())
                        .spawn(move || {
                            let result = vault.set(&key, &given).map(|()| true);
                            sink.plugin(PluginEvent::KeySaved { plugin, result });
                        });
                });
                prompter(KeyPrompt {
                    plugin: name.to_owned(),
                    title: format!("Clave de {title}"),
                    message: format!(
                        "Pegá tu clave de {title}. Queda guardada en el llavero de tu Mac; Wawi no la muestra ni la dice."
                    ),
                    help,
                    answer: answer_key,
                });
                answer(
                    &self.sink,
                    reply,
                    Ok(format!(
                        "Te abrí una ventanita para que pegues la clave de {title}. No me la digas en voz alta."
                    )),
                );
            }
            Auth::None => {
                self.waiting.remove(name);
                self.restart(name);
                answer(
                    &self.sink,
                    reply,
                    Ok(format!(
                        "Listo, estoy conectando {title}; en unos segundos tengo sus herramientas."
                    )),
                );
            }
        }
    }

    /// The browser login, on its own thread. `reply` is answered as soon as
    /// the browser opens (or the login fails before that).
    fn login(
        &self,
        name: &str,
        url: String,
        client_id: Option<String>,
        scopes: Option<Vec<String>>,
        title: String,
        reply: Option<Reply>,
    ) {
        let vault = Vault::new(self.services.store.clone(), name);
        let (sink, open, timeout) = (
            self.sink.clone(),
            self.services.open.clone(),
            self.services.login_timeout,
        );
        let plugin = name.to_owned();
        self.log.line(&format!("{plugin}: login en el navegador"));
        let spawned = std::thread::Builder::new()
            .name(format!("wawi-plugin-login-{name}"))
            .spawn(move || {
                let previous = [OAUTH_ACCOUNT, OAUTH_CLIENT_ACCOUNT]
                    .iter()
                    .find_map(|account| vault.get(account).ok().flatten())
                    .and_then(|text| Grant::from_json(&text));
                let mut reply = reply;
                let result = {
                    let mut opened = || {
                        answer(
                            &sink,
                            reply.take(),
                            Ok(format!(
                                "Te abrí {title} en el navegador para que me des permiso."
                            )),
                        );
                    };
                    oauth::login(oauth::Login {
                        mcp_url: &url,
                        client_id: client_id.as_deref(),
                        scopes: scopes.as_deref(),
                        previous: previous.as_ref(),
                        www_authenticate: None,
                        timeout,
                        open: &*open,
                        opened: &mut opened,
                    })
                };
                let result = result.and_then(|grant| {
                    vault.set(OAUTH_ACCOUNT, &grant.to_json())?;
                    // The client alone, to reuse it after a revocation.
                    let client = Grant {
                        access_token: String::new(),
                        refresh_token: None,
                        expires_at: None,
                        scope: None,
                        ..grant
                    };
                    vault.set(OAUTH_CLIENT_ACCOUNT, &client.to_json())
                });
                if let Err(error) = &result {
                    answer(
                        &sink,
                        reply.take(),
                        Err(format!("No pude conectar {title}: {error}")),
                    );
                }
                sink.plugin(PluginEvent::LoginDone { plugin, result });
            });
        if let Err(error) = spawned {
            self.log
                .line(&format!("no pude arrancar el login de {name}: {error}"));
        }
    }

    // -- the snapshot for screens ------------------------------------------------

    fn info(&self, name: &str) -> PluginInfo {
        let manifest = self.manifest(name);
        let kind = if BUILTIN.contains(&name) {
            PluginKind::Propio
        } else {
            PluginKind::Conector
        };
        let installed = manifest.is_some();
        let enabled = manifest.is_some_and(|m| m.enabled);
        let running = self.plugins.iter().find(|p| p.manifest.name == name);
        let reason = |map: &BTreeMap<String, String>| map.get(name).cloned();
        let (state, detail) = if !enabled {
            let detail = match (installed, kind) {
                (false, _) => Some("de Wawi; prendelo para usarlo".to_owned()),
                _ => None,
            };
            (PluginState::Apagado, detail)
        } else if let Some(what) = reason(&self.pending) {
            (PluginState::Conectando, Some(what))
        } else if let Some(how) = reason(&self.missing) {
            (PluginState::Error, Some(how))
        } else {
            match running {
                Some(r) if r.ready => (PluginState::Conectado, None),
                _ if self.waiting.contains_key(name) => {
                    (PluginState::Desconectado, reason(&self.waiting))
                }
                Some(r) if r.settled => (PluginState::Error, reason(&self.errors)),
                Some(_) => (PluginState::Conectando, None),
                None => (PluginState::Desconectado, None),
            }
        };
        PluginInfo {
            name: name.to_owned(),
            title: self.title(name),
            description: manifest.map(|m| m.description.clone()).unwrap_or_default(),
            kind,
            installed,
            enabled,
            state,
            auth: manifest.map(|m| m.auth.label()).unwrap_or("ninguna"),
            url: manifest.and_then(|m| match &m.server {
                Server::Http { url, .. } => Some(url.clone()),
                Server::Stdio { .. } => None,
            }),
            detail,
            tools: running
                .filter(|r| r.ready)
                .map(|r| r.registered.len())
                .unwrap_or(0),
        }
    }

    /// Recomputes the list for screens and tells the subscribers.
    fn publish(&self) {
        let mut names: Vec<String> = self.installed.iter().map(|m| m.name.clone()).collect();
        if let Some(catalog) = &self.services.catalog {
            for builtin in BUILTIN {
                let shipped = catalog.join(builtin).join("plugin.toml").is_file();
                if shipped && !names.iter().any(|n| n == builtin) {
                    names.push(builtin.to_string());
                }
            }
        }
        names.sort();
        let infos: Vec<PluginInfo> = names.iter().map(|name| self.info(name)).collect();
        self.shared.publish(infos);
    }

    fn watch_result(
        &mut self,
        plugin: &str,
        key: &str,
        result: Result<String, String>,
    ) -> Option<Event> {
        let watch = self
            .plugins
            .iter()
            .find(|p| p.manifest.name == plugin)?
            .manifest
            .watches
            .iter()
            .find(|w| format!("{plugin}/{}", w.name) == key)?
            .clone();
        let text = match result {
            Ok(text) => text,
            Err(_) => return None,
        };
        if mcp::says_nothing(&text, &watch.empty) {
            self.log.line(&format!("watch {key}: nada que decir"));
            return None;
        }
        self.log.line(&format!("watch {key}: candidato"));
        Some(Event::Instinct {
            candidate: InstinctCandidate {
                id: format!("watch:{key}:{}", self.clock.now()),
                key: key.to_owned(),
                say: watch.say.clone(),
                context: truncate(&text, self.options.max_context),
                priority: watch.priority,
                cooldown_ms: watch.cooldown.map(|d| d.as_millis() as u64),
                local_minute: self.local_minute(),
                urgent: false,
                expires_in_ms: None,
            },
        })
    }

    /// `{id, decir, urgencia, contexto}` from a plugin's notification.
    fn push(&mut self, plugin: &str, params: &Value) -> Option<Event> {
        let say = params
            .get("decir")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(say) = say else {
            self.log
                .line(&format!("{plugin} empujó un aviso sin «decir»; lo ignoro"));
            return None;
        };
        let id: String = params
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| inbox::valid_id(id))
            .unwrap_or("aviso")
            .to_owned();
        let urgent = params.get("urgencia").and_then(Value::as_str) == Some("alta");
        let priority = match params.get("urgencia").and_then(Value::as_str) {
            Some("baja") => Priority::Low,
            _ => Priority::High,
        };
        let context = match params.get("contexto") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
        };
        let mut data = format!("Aviso del plugin {plugin}: {say}");
        if !context.trim().is_empty() {
            data.push_str(&format!("\nContexto: {}", context.trim()));
        }
        self.next_push += 1;
        self.log.line(&format!("{plugin} empujó el aviso {id}"));
        Some(Event::Instinct {
            candidate: InstinctCandidate {
                id: format!("push:{plugin}:{}", self.next_push),
                key: format!("{plugin}/{id}"),
                say: PUSH_INSTRUCTION.into(),
                context: truncate(&data, self.options.max_context),
                priority,
                cooldown_ms: None,
                local_minute: self.local_minute(),
                urgent,
                expires_in_ms: None,
            },
        })
    }

    fn local_minute(&self) -> u16 {
        minute_of_day(&self.clock.local_now())
    }

    /// Call on every host tick. Starts due watches (their results come back
    /// as `PluginEvent::WatchResult`), and returns due routines and new
    /// inbox items as instinct candidates for the core.
    pub fn tick(&mut self) -> Vec<Event> {
        let now = self.clock.local_now();
        let epoch = self.clock.now();
        let ready: Vec<String> = self
            .plugins
            .iter()
            .filter(|p| p.ready)
            .map(|p| p.manifest.name.clone())
            .collect();
        let due = self
            .scheduler
            .due(&now, epoch, |plugin| ready.iter().any(|r| r == plugin));
        let mut events = Vec::new();
        for key in due {
            if let Some(event) = self.run_due(&key, &now) {
                events.push(event);
            }
        }
        let seen = self.questions.tick(epoch);
        if !seen.is_empty() {
            self.questions_seen(seen);
        }
        if self
            .last_inbox_scan
            .is_none_or(|last| epoch - last >= INBOX_EVERY_S || epoch < last)
        {
            self.last_inbox_scan = Some(epoch);
            events.extend(self.scan_inbox());
        }
        events
    }

    /// Reads the inbox now, whatever the last scan was.
    pub fn scan_inbox(&mut self) -> Vec<Event> {
        let Some(inbox) = self.inbox.as_mut() else {
            return Vec::new();
        };
        let mut notes = Vec::new();
        let found = inbox.scan(self.clock.as_ref(), &mut notes);
        for note in notes {
            self.log.line(&note);
        }
        found
            .into_iter()
            .map(|candidate| Event::Instinct { candidate })
            .collect()
    }

    fn run_due(&mut self, key: &str, now: &Civil) -> Option<Event> {
        let (plugin, name) = key.split_once('/')?;
        let running = self.plugins.iter().find(|p| p.manifest.name == plugin)?;
        if let Some(watch) = running.manifest.watches.iter().find(|w| w.name == name) {
            let job = Job::Call {
                origin: Origin::Watch {
                    key: key.to_owned(),
                },
                tool: watch.tool.clone(),
                args: expand(&watch.args, now),
            };
            let _ = running.jobs.send(job);
            return None;
        }
        let routine = running.manifest.routines.iter().find(|r| r.name == name)?;
        self.log.line(&format!("routine {key}: candidato"));
        Some(Event::Instinct {
            candidate: InstinctCandidate {
                id: format!("routine:{key}:{}", self.clock.now()),
                key: key.to_owned(),
                say: routine.prompt.clone(),
                context: String::new(),
                priority: Priority::High,
                // Once a day at most, unless it says otherwise.
                cooldown_ms: Some(
                    routine
                        .cooldown
                        .unwrap_or(Duration::from_secs(12 * 3600))
                        .as_millis() as u64,
                ),
                local_minute: minute_of_day(now),
                urgent: false,
                expires_in_ms: None,
            },
        })
    }

    /// The core decided about an instinct (`Effect::InstinctDecided`).
    pub fn instinct_decided(&mut self, id: &str, outcome: &InstinctOutcome) {
        match outcome {
            InstinctOutcome::Started(turn) => {
                self.log.line(&format!("instinto {id}: turno {}", turn.0));
                if let Some(inbox) = self.inbox.as_mut() {
                    inbox.started(id, *turn);
                }
            }
            InstinctOutcome::Postponed {
                reason,
                retry_in_ms,
            } => {
                self.log.line(&format!(
                    "instinto {id}: más tarde ({reason:?}, en {} s)",
                    retry_in_ms / 1000
                ));
                let retry_at = self.clock.now() + (*retry_in_ms / 1000) as i64;
                if let Some(inbox) = self.inbox.as_mut() {
                    inbox.postponed(id, *reason, retry_at);
                }
            }
            InstinctOutcome::Dropped(reason) => {
                self.log
                    .line(&format!("instinto {id}: descartado ({reason:?})"));
                let now = self.clock.local_now();
                if let Some(inbox) = self.inbox.as_mut() {
                    inbox.dropped(id, *reason, now);
                }
            }
        }
    }

    /// The owner's words, from a final transcript.
    pub fn heard(&mut self, text: &str) {
        let now = self.stamp();
        if let Some(item) = self.feed.heard(text, &now) {
            self.finished(item);
        }
    }

    /// Something Wawi said in `turn`.
    pub fn said(&mut self, turn: TurnId, text: &str, kind: &SpeechKind, initiative: bool) {
        let now = self.clock.local_now();
        if let Some(inbox) = self.inbox.as_mut()
            && let Some(id) = inbox.spoke(turn, now)
        {
            self.log.line(&format!("cola: {id} dicho"));
        }
        if matches!(
            kind,
            SpeechKind::Answer
                | SpeechKind::Question
                | SpeechKind::Route(_)
                | SpeechKind::Failure(_)
        ) {
            let stamp = self.stamp();
            self.feed.said(turn.0, text, initiative, &stamp);
        }
    }

    /// The owner talked over Wawi.
    pub fn interrupted(&mut self) {
        let now = self.stamp();
        if let Some(item) = self.feed.interrupted(&now) {
            self.finished(item);
        }
    }

    /// The dialogue is idle again: the turn is over.
    pub fn idle(&mut self) {
        let now = self.stamp();
        if let Some(item) = self.feed.finish(false, &now) {
            self.finished(item);
        }
        let civil = self.clock.local_now();
        if let Some(inbox) = self.inbox.as_mut() {
            for id in inbox.all_silent(civil) {
                self.log.line(&format!("cola: {id} sin palabras"));
            }
        }
    }

    fn stamp(&self) -> String {
        let now = self.clock.local_now();
        format!("{}:{:02}", now.iso_minute(), now.second)
    }

    /// A turn is over: questions she asked are followed, and observing
    /// plugins get it.
    fn finished(&mut self, item: FeedItem) {
        let turn = preguntas::Turn {
            owner: &item.owner,
            wawi: &item.wawi,
            interrupted: item.interrupted,
            initiative: item.initiative,
        };
        let seen = self.questions.turn(&turn, self.clock.now());
        self.questions_seen(seen);
        self.send_feed(item);
    }

    /// Unanswered questions become facts; a late answer takes one back.
    fn questions_seen(&mut self, seen: Vec<preguntas::Seen>) {
        for seen in seen {
            match seen {
                preguntas::Seen::Unanswered(question) => {
                    let config = self.questions.config();
                    let fact = avisos::Fact {
                        id: question.id.clone(),
                        origin: preguntas::ORIGIN.into(),
                        say: format!("Le preguntaste al dueño: «{}»", question.question),
                        context: Some(json!({ "pregunta": question.question })),
                        urgency: "baja",
                        not_before: Some(question.not_before(config)),
                        expires: Some(question.expires(config)),
                    };
                    match avisos::drop_fact(&self.home, &fact, self.clock.as_ref()) {
                        Ok(true) => self
                            .log
                            .line(&format!("{}: sin respuesta, para retomar", question.id)),
                        Ok(false) => {}
                        Err(error) => self
                            .log
                            .line(&format!("{}: no pude guardarla: {error}", question.id)),
                    }
                }
                preguntas::Seen::Answered { id } => {
                    let now = self.clock.local_now();
                    if let Some(inbox) = self.inbox.as_mut()
                        && inbox.answered(&id, now)
                    {
                        self.log
                            .line(&format!("{id}: respondida tarde, no se retoma"));
                    }
                }
            }
        }
    }

    /// Queues the item for every observing plugin and wakes its worker.
    /// Never waits on a plugin.
    fn send_feed(&self, item: FeedItem) {
        for running in self.plugins.iter().filter(|p| p.manifest.observe_chat) {
            let dropped = push_feed(&running.feed, item.clone(), self.options.feed_cap);
            if dropped > 0 {
                self.log.line(&format!(
                    "{}: el plugin va lento; descarté {dropped} turno(s) viejos del feed",
                    running.manifest.name
                ));
            }
            let _ = running.jobs.send(Job::Feed);
        }
    }

    /// Asks every worker to stop and waits up to two seconds for them; a
    /// worker stuck in a call is left behind (its server dies with Wawi).
    pub fn shutdown(&mut self) {
        for running in &self.plugins {
            let _ = running.jobs.send(Job::Shutdown);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        for running in &mut self.plugins {
            if let Some(thread) = running.thread.take() {
                while !thread.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                if thread.is_finished() {
                    let _ = thread.join();
                }
            }
        }
    }
}

/// Lowercase without accents, for matching what the owner said.
fn fold(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' => 'a',
            'é' => 'e',
            'í' => 'i',
            'ó' => 'o',
            'ú' | 'ü' => 'u',
            other => other,
        })
        .collect()
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}
