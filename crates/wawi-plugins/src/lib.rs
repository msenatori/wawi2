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
//! Everything here runs on the host's thread except the workers; results
//! come back through a [`PluginSink`].

pub mod feed;
pub mod inbox;
pub mod log;
pub mod manifest;
pub mod mcp;
pub mod schedule;
pub mod secrets;
pub mod tool;
mod worker;

use std::collections::VecDeque;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;
use wawi_core::{
    Event, InstinctCandidate, InstinctOutcome, Priority, Registry, SpeechKind, TurnId,
};
use wawi_tools::{Civil, WallClock};

use crate::feed::{FeedItem, TurnFeed};
use crate::inbox::Inbox;
use crate::log::CallLog;
use crate::manifest::{Manifest, truncate};
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
    plugins: Vec<Running>,
    disabled: Vec<String>,
    scheduler: Scheduler,
    problems: Vec<String>,
    feed: TurnFeed,
    inbox: Option<Inbox>,
    last_inbox_scan: Option<i64>,
    next_push: u64,
}

impl PluginHost {
    /// Finds the plugins under `<home>/plugins`, starts a worker for each
    /// enabled one (they connect in the background) and opens the inbox.
    /// Problems (a bad plugin.toml, a secrets file with loose permissions)
    /// are kept for [`Self::problems`]; nothing here fails.
    pub fn start(
        home: &Path,
        logs_dir: Option<&Path>,
        clock: Rc<dyn WallClock>,
        sink: Arc<dyn PluginSink>,
        options: HostOptions,
    ) -> Self {
        let log = CallLog::open(logs_dir);
        let (manifests, mut problems) = manifest::discover(home);
        let (secrets, secrets_problem) = Secrets::load(home);
        problems.extend(secrets_problem);
        let mut plugins = Vec::new();
        let mut disabled = Vec::new();
        let mut entries = Vec::new();
        for manifest in manifests {
            if !manifest.enabled {
                disabled.push(manifest.name.clone());
                continue;
            }
            for watch in &manifest.watches {
                entries.push(Entry {
                    key: format!("{}/{}", manifest.name, watch.name),
                    plugin: manifest.name.clone(),
                    when: watch.when,
                });
            }
            for routine in &manifest.routines {
                entries.push(Entry {
                    key: format!("{}/{}", manifest.name, routine.name),
                    plugin: manifest.name.clone(),
                    when: When::At(routine.at),
                });
            }
            let (jobs, rx) = mpsc::channel();
            let feed: FeedQueue = Arc::new(Mutex::new(VecDeque::new()));
            let worker = Worker::new(
                manifest.clone(),
                secrets.clone(),
                sink.clone(),
                log.clone(),
                jobs.clone(),
                options.clone(),
                feed.clone(),
            );
            let thread = std::thread::Builder::new()
                .name(format!("wawi-plugin-{}", manifest.name))
                .spawn(move || worker.run(rx));
            match thread {
                Ok(thread) => plugins.push(Running {
                    manifest,
                    jobs,
                    feed,
                    ready: false,
                    settled: false,
                    registered: Vec::new(),
                    thread: Some(thread),
                }),
                Err(error) => problems.push(format!(
                    "{}: no pude arrancar su hilo: {error}",
                    manifest.name
                )),
            }
        }
        for problem in &problems {
            log.line(&format!("problema: {problem}"));
        }
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
        Self {
            scheduler: Scheduler::new(entries, Some(home.join(SCHEDULE_STATE))),
            clock,
            sink,
            log,
            options,
            plugins,
            disabled,
            problems,
            feed: TurnFeed::default(),
            inbox,
            last_inbox_scan: None,
            next_push: 0,
        }
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

    /// Plugins with `enabled = false`.
    pub fn disabled(&self) -> &[String] {
        &self.disabled
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
                Vec::new()
            }
            PluginEvent::Down { plugin, .. } => {
                // Its tools stay registered: a call fails fast with the reason.
                if let Some(running) = self.plugins.iter_mut().find(|p| p.manifest.name == plugin) {
                    running.ready = false;
                    running.settled = true;
                }
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
            self.send_feed(item);
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
            self.send_feed(item);
        }
    }

    /// The dialogue is idle again: the turn is over.
    pub fn idle(&mut self) {
        let now = self.stamp();
        if let Some(item) = self.feed.finish(false, &now) {
            self.send_feed(item);
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

impl Drop for PluginHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}
