//! One thread per plugin: it owns the connection, runs calls one at a time
//! with a timeout, restarts a crashed or unreachable server with backoff,
//! and sends the turn feed. Nothing here can block the host thread or take
//! Wawi down: every failure becomes a tool failure, a log line, or both.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use wawi_core::{Event, ToolOutcome, TurnId};

use crate::catalog;
use crate::feed::FeedItem;
use crate::keychain::{OAUTH_ACCOUNT, Vault};
use crate::log::CallLog;
use crate::manifest::{Auth, FEED_TOOL, Manifest, Server, truncate};
use crate::mcp::{
    Bearer, Client, HttpTransport, McpError, OnNotify, StdioSpec, StdioTransport, Transport,
};
use crate::oauth::{self, Grant, RefreshError};
use crate::secrets::Secrets;
use crate::tool::specs_for;
use crate::{HostOptions, PUSH_METHOD, PluginEvent, PluginSink};

/// The longest tool result handed to the brain.
const MAX_RESULT: usize = 20_000;

pub(crate) enum Origin {
    Brain { turn: TurnId, call_id: String },
    Watch { key: String },
}

pub(crate) enum Job {
    Call {
        origin: Origin,
        tool: String,
        args: Value,
    },
    /// Turn feed items are waiting in the shared queue.
    Feed,
    /// The server said its tool list changed.
    Relist {
        generation: u64,
    },
    /// A stdio server's stdout closed.
    Closed {
        generation: u64,
    },
    /// The credential changed (a login, a key): drop the connection and
    /// connect again now.
    Reconnect,
    Shutdown,
}

/// Why a connection could not be made.
enum Fail {
    /// The server wants a (new) credential: wait for the owner.
    Auth(String),
    /// Anything else: retry with backoff.
    Other(String),
}

pub(crate) type FeedQueue = Arc<Mutex<VecDeque<FeedItem>>>;

pub(crate) struct Worker {
    pub manifest: Manifest,
    pub secrets: Secrets,
    pub sink: Arc<dyn PluginSink>,
    pub log: CallLog,
    pub jobs: Sender<Job>,
    pub options: HostOptions,
    pub feed: FeedQueue,
    vault: Vault,
    /// The OAuth grant in use (read from the Keychain on this thread).
    grant: Option<Grant>,
    /// What the HTTP transport sends as `Authorization: Bearer`.
    bearer: Bearer,
    /// Waiting for the owner to connect it: no retries until `Reconnect`.
    parked: bool,
    client: Option<Client>,
    generation: u64,
    failures: u32,
    next_try: Instant,
    last_error: String,
}

impl Worker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        manifest: Manifest,
        secrets: Secrets,
        sink: Arc<dyn PluginSink>,
        log: CallLog,
        jobs: Sender<Job>,
        options: HostOptions,
        feed: FeedQueue,
        vault: Vault,
    ) -> Self {
        Self {
            manifest,
            secrets,
            sink,
            log,
            jobs,
            options,
            feed,
            vault,
            grant: None,
            bearer: Arc::default(),
            parked: false,
            client: None,
            generation: 0,
            failures: 0,
            next_try: Instant::now(),
            last_error: String::new(),
        }
    }

    fn name(&self) -> &str {
        &self.manifest.name
    }

    pub fn run(mut self, rx: Receiver<Job>) {
        self.connect();
        loop {
            let job = if self.client.is_some() || self.parked {
                match rx.recv() {
                    Ok(job) => job,
                    Err(_) => break,
                }
            } else {
                let wait = self.next_try.saturating_duration_since(Instant::now());
                match rx.recv_timeout(wait) {
                    Ok(job) => job,
                    Err(RecvTimeoutError::Timeout) => {
                        self.connect();
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            };
            match job {
                Job::Shutdown => break,
                Job::Call { origin, tool, args } => self.call(origin, &tool, args),
                Job::Feed => self.drain_feed(),
                Job::Relist { generation } if generation == self.generation => self.relist(),
                Job::Closed { generation } if generation == self.generation => {
                    if self.client.is_some() {
                        self.fail("el proceso del plugin terminó".into());
                    }
                }
                Job::Reconnect => {
                    self.client = None;
                    self.parked = false;
                    self.failures = 0;
                    self.grant = None;
                    self.connect();
                }
                Job::Relist { .. } | Job::Closed { .. } => {}
            }
        }
        self.log.line(&format!("{} cerrado", self.name()));
    }

    /// The secrets this connection resolves `${NAME}` with: a Keychain key
    /// first (only this plugin's own item), then secrets.toml and the
    /// environment.
    fn credentials(&mut self) -> Result<Secrets, Fail> {
        match self.manifest.auth.clone() {
            Auth::None => Ok(self.secrets.clone()),
            Auth::Key { name, .. } => {
                let stored = self
                    .vault
                    .get(&name)
                    .map_err(|e| Fail::Other(e.to_string()))?;
                match stored {
                    Some(value) => {
                        self.secrets.remember(&value);
                        Ok(self.secrets.with_value(&name, &value))
                    }
                    // Legacy: secrets.toml or the environment still work.
                    None if self.secrets.resolve(&format!("${{{name}}}")).is_ok() => {
                        Ok(self.secrets.clone())
                    }
                    None => Err(Fail::Auth("falta la clave".into())),
                }
            }
            Auth::OAuth { .. } => {
                if self.grant.is_none() {
                    let stored = self
                        .vault
                        .get(OAUTH_ACCOUNT)
                        .map_err(|e| Fail::Other(e.to_string()))?;
                    self.grant = stored.as_deref().and_then(Grant::from_json);
                }
                if self.grant.is_none() {
                    return Err(Fail::Auth("todavía no le diste permiso".into()));
                }
                if self.grant.as_ref().is_some_and(|g| g.stale(oauth::now_s())) {
                    self.refresh()?;
                }
                self.arm_bearer();
                Ok(self.secrets.clone())
            }
        }
    }

    fn arm_bearer(&self) {
        let token = self.grant.as_ref().map(|g| g.access_token.clone());
        if let Some(token) = &token {
            self.secrets.remember(token);
        }
        if let Ok(mut slot) = self.bearer.lock() {
            *slot = token;
        }
    }

    /// Renews the OAuth token (on this thread) and keeps the new grant,
    /// rotated refresh token included. Revoked: the grant is forgotten.
    fn refresh(&mut self) -> Result<(), Fail> {
        let Some(grant) = self.grant.clone() else {
            return Err(Fail::Auth("todavía no le diste permiso".into()));
        };
        match oauth::refresh(&grant) {
            Ok(fresh) => {
                if let Some(token) = &fresh.refresh_token {
                    self.secrets.remember(token);
                }
                if let Err(error) = self.vault.set(OAUTH_ACCOUNT, &fresh.to_json()) {
                    self.log.line(&format!(
                        "{}: no pude guardar el permiso renovado: {error}",
                        self.name()
                    ));
                }
                self.log.line(&format!("{} permiso renovado", self.name()));
                self.grant = Some(fresh);
                self.arm_bearer();
                Ok(())
            }
            Err(RefreshError::Revoked(why)) => {
                self.grant = None;
                let _ = self.vault.delete(OAUTH_ACCOUNT);
                Err(Fail::Auth(why))
            }
            Err(RefreshError::Transient(why)) => Err(Fail::Other(why)),
        }
    }

    fn transport(&self, generation: u64, secrets: &Secrets) -> Result<Box<dyn Transport>, String> {
        let plugin = self.name().to_owned();
        let sink = self.sink.clone();
        let jobs = self.jobs.clone();
        let log = self.log.clone();
        let on_notify: OnNotify = Arc::new(move |method: &str, params: &Value| {
            match method {
                PUSH_METHOD => sink.plugin(PluginEvent::Push {
                    plugin: plugin.clone(),
                    params: params.clone(),
                }),
                // A structured log message carrying the same payload counts too.
                "notifications/message" if params.pointer("/data/decir").is_some() => {
                    sink.plugin(PluginEvent::Push {
                        plugin: plugin.clone(),
                        params: params["data"].clone(),
                    })
                }
                "notifications/tools/list_changed" => {
                    let _ = jobs.send(Job::Relist { generation });
                }
                // The hook for subscribed resources: logged, not spoken yet.
                other => log.line(&format!("{plugin} aviso {other} (ignorado)")),
            }
        });
        let resolve = |text: &str| secrets.resolve(text);
        match &self.manifest.server {
            Server::Stdio { command, args, env } => {
                // Wawi's own programs: beside wawi2 first, then the PATH.
                let command = catalog::resolve_command(command, &self.manifest.dir)
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|| command.clone());
                let command = &command;
                let args = args
                    .iter()
                    .map(|a| resolve(a))
                    .collect::<Result<Vec<_>, _>>()?;
                let env = env
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), resolve(v)?)))
                    .collect::<Result<Vec<_>, String>>()?;
                let jobs = self.jobs.clone();
                let (log, secrets, plugin) = (
                    self.log.clone(),
                    self.secrets.clone(),
                    self.name().to_owned(),
                );
                let transport = StdioTransport::spawn(
                    StdioSpec {
                        command,
                        args: &args,
                        env: &env,
                        dir: &self.manifest.dir,
                    },
                    on_notify,
                    Box::new(move |line| {
                        log.line(&format!(
                            "{plugin} stderr: {}",
                            truncate(&secrets.scrub(line), 300)
                        ));
                    }),
                    Box::new(move || {
                        let _ = jobs.send(Job::Closed { generation });
                    }),
                )
                .map_err(|e| e.to_string())?;
                Ok(Box::new(transport))
            }
            Server::Http { url, headers } => {
                let url = resolve(url)?;
                let mut resolved = headers
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), resolve(v)?)))
                    .collect::<Result<Vec<_>, String>>()?;
                let transport = match &self.manifest.auth {
                    Auth::OAuth { .. } => {
                        resolved.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
                        HttpTransport::new(&url, resolved, on_notify)
                            .with_bearer(self.bearer.clone())
                    }
                    Auth::Key { name, .. } => {
                        let reference = format!("${{{name}}}");
                        if !headers.values().any(|v| v.contains(&reference)) {
                            resolved.push((
                                "Authorization".into(),
                                format!("Bearer {}", resolve(&reference)?),
                            ));
                        }
                        HttpTransport::new(&url, resolved, on_notify)
                    }
                    Auth::None => HttpTransport::new(&url, resolved, on_notify),
                };
                Ok(Box::new(transport))
            }
        }
    }

    fn try_connect(&mut self) -> Result<(Client, Vec<crate::mcp::RemoteTool>), Fail> {
        let timeout = self.manifest.timeout;
        // One more try after renewing an OAuth token the server refused.
        for attempt in 0..2 {
            let secrets = self.credentials()?;
            self.generation += 1;
            let generation = self.generation;
            let transport = self.transport(generation, &secrets).map_err(Fail::Other)?;
            let connected = Client::connect(transport, timeout).and_then(|mut client| {
                let listed = client.list_tools(timeout)?;
                Ok((client, listed))
            });
            match connected {
                Ok(done) => return Ok(done),
                Err(McpError::Unauthorized { .. })
                    if attempt == 0 && matches!(self.manifest.auth, Auth::OAuth { .. }) =>
                {
                    self.refresh()?;
                }
                Err(McpError::Unauthorized { status, .. }) => {
                    return Err(Fail::Auth(match self.manifest.auth {
                        Auth::Key { .. } => format!("el servidor rechazó la clave (HTTP {status})"),
                        _ => format!("el servidor pide permiso (HTTP {status})"),
                    }));
                }
                Err(other) => return Err(Fail::Other(other.to_string())),
            }
        }
        Err(Fail::Auth("el servidor rechazó el permiso".into()))
    }

    fn connect(&mut self) {
        let connected = self.try_connect();
        let connected = match connected {
            Ok(done) => Ok(done),
            Err(Fail::Auth(reason)) => return self.park(reason),
            Err(Fail::Other(reason)) => Err(reason),
        };
        match connected {
            Ok((client, listed)) => {
                let tools = specs_for(&self.manifest, &listed);
                self.log.line(&format!(
                    "{} conectado ({} herramientas)",
                    self.name(),
                    tools.len()
                ));
                self.client = Some(client);
                self.sink.plugin(PluginEvent::Ready {
                    plugin: self.name().to_owned(),
                    tools,
                });
                // Feed items that waited while it was down.
                self.drain_feed();
            }
            Err(reason) => self.fail(reason),
        }
    }

    fn relist(&mut self) {
        let timeout = self.manifest.timeout;
        let Some(client) = self.client.as_mut() else {
            return;
        };
        match client.list_tools(timeout) {
            Ok(listed) => {
                let tools = specs_for(&self.manifest, &listed);
                self.sink.plugin(PluginEvent::Ready {
                    plugin: self.manifest.name.clone(),
                    tools,
                });
            }
            Err(error) => self.handle_error(error),
        }
    }

    /// Waits for the owner: no retries until a `Reconnect`. The host shows
    /// it as «Desconectado».
    fn park(&mut self, reason: String) {
        self.client = None;
        self.parked = true;
        let reason = self.secrets.scrub(&reason);
        self.log
            .line(&format!("{} desconectado: {reason}", self.name()));
        self.last_error = reason.clone();
        self.sink.plugin(PluginEvent::NeedsAuth {
            plugin: self.name().to_owned(),
            reason,
        });
    }

    /// Drops the connection and waits longer each time before retrying.
    fn fail(&mut self, reason: String) {
        self.client = None;
        self.failures += 1;
        let delay = backoff(&self.options, self.failures);
        self.next_try = Instant::now() + delay;
        let reason = self.secrets.scrub(&reason);
        self.log.line(&format!(
            "{} caído: {reason}; reintento en {} s",
            self.name(),
            delay.as_secs_f32().ceil()
        ));
        self.last_error = reason.clone();
        self.sink.plugin(PluginEvent::Down {
            plugin: self.name().to_owned(),
            reason,
        });
    }

    fn handle_error(&mut self, error: McpError) {
        match error {
            McpError::Dead(why) => self.fail(why),
            McpError::Unauthorized { .. } if self.parked => {}
            McpError::Unauthorized { status, .. } => {
                self.park(format!("el servidor rechazó la credencial (HTTP {status})"))
            }
            McpError::Timeout | McpError::Rpc(_) => {}
        }
    }

    /// Before a call: renew a token about to expire. False if the plugin
    /// had to be parked or dropped.
    fn fresh_enough(&mut self) -> bool {
        if !self.grant.as_ref().is_some_and(|g| g.stale(oauth::now_s())) {
            return true;
        }
        match self.refresh() {
            Ok(()) => true,
            Err(Fail::Auth(why)) => {
                self.park(why);
                false
            }
            Err(Fail::Other(why)) => {
                self.fail(why);
                false
            }
        }
    }

    fn call(&mut self, origin: Origin, tool: &str, args: Value) {
        if self.client.is_none() && !self.parked && Instant::now() >= self.next_try {
            self.connect();
        }
        if self.client.is_some() {
            self.fresh_enough();
        }
        let from = match &origin {
            Origin::Brain { turn, .. } => format!("cerebro turno {}", turn.0),
            Origin::Watch { key } => format!("watch {key}"),
        };
        let timeout = self.manifest.timeout;
        let Some(client) = self.client.as_mut() else {
            let wait = self.next_try.saturating_duration_since(Instant::now());
            let reason = if self.parked {
                format!(
                    "el plugin {} está desconectado ({}). El dueño lo conecta diciendo «conectá {}» o desde Ajustes",
                    self.manifest.name, self.last_error, self.manifest.name
                )
            } else {
                format!(
                    "el plugin {} no está disponible ({}); reintento en {} s",
                    self.manifest.name,
                    self.last_error,
                    wait.as_secs_f32().ceil()
                )
            };
            self.log.line(&format!(
                "{} {tool} desde {from}: no disponible",
                self.manifest.name
            ));
            return self.respond(origin, Err(reason));
        };
        let started = Instant::now();
        let mut result = client.call(tool, args.clone(), timeout);
        if matches!(result, Err(McpError::Unauthorized { .. }))
            && matches!(self.manifest.auth, Auth::OAuth { .. })
        {
            // The token was refused before its time: renew once and retry.
            result = match self.refresh() {
                Ok(()) => match self.client.as_mut() {
                    Some(client) => client.call(tool, args, timeout),
                    None => result,
                },
                Err(Fail::Auth(why)) => {
                    self.park(why);
                    result
                }
                Err(Fail::Other(_)) => result,
            };
        }
        let ms = started.elapsed().as_millis();
        let outcome = match result {
            Ok(result) => {
                self.failures = 0;
                let status = if result.is_error { "error" } else { "ok" };
                self.log.line(&format!(
                    "{} {tool} desde {from}: {status} en {ms} ms, {} letras",
                    self.manifest.name,
                    result.text.chars().count()
                ));
                let text = truncate(&result.text, MAX_RESULT);
                if result.is_error { Err(text) } else { Ok(text) }
            }
            Err(error) => {
                let reason = match &error {
                    McpError::Timeout => format!(
                        "el plugin {} tardó demasiado (más de {} s)",
                        self.manifest.name,
                        timeout.as_secs_f32()
                    ),
                    other => format!("el plugin {} falló: {other}", self.manifest.name),
                };
                let reason = self.secrets.scrub(&reason);
                self.log.line(&format!(
                    "{} {tool} desde {from}: falló en {ms} ms: {reason}",
                    self.manifest.name
                ));
                self.handle_error(error);
                Err(reason)
            }
        };
        self.respond(origin, outcome);
    }

    fn respond(&self, origin: Origin, outcome: Result<String, String>) {
        match origin {
            Origin::Brain { turn, call_id } => self.sink.core(Event::ToolFinished {
                turn,
                call_id,
                outcome: match outcome {
                    Ok(text) => ToolOutcome::Success(text),
                    Err(reason) => ToolOutcome::Failure(reason),
                },
            }),
            Origin::Watch { key } => self.sink.plugin(PluginEvent::WatchResult {
                plugin: self.manifest.name.clone(),
                key,
                result: outcome,
            }),
        }
    }

    /// Sends every waiting feed item, oldest first. Only logged.
    fn drain_feed(&mut self) {
        if !self.manifest.observe_chat {
            return;
        }
        let timeout = self.manifest.timeout;
        loop {
            if self.client.is_none() {
                return;
            }
            let Some(item) = self
                .feed
                .lock()
                .ok()
                .and_then(|mut queue| queue.pop_front())
            else {
                return;
            };
            let Some(client) = self.client.as_mut() else {
                return;
            };
            let started = Instant::now();
            let result = client.call(FEED_TOOL, item.to_args(), timeout);
            let ms = started.elapsed().as_millis();
            let status = match &result {
                Ok(result) if !result.is_error => "ok".to_owned(),
                Ok(_) => "error del plugin".to_owned(),
                Err(error) => self.secrets.scrub(&error.to_string()),
            };
            self.log.line(&format!(
                "{} {FEED_TOOL} turno {}: {status} en {ms} ms",
                self.manifest.name, item.turn
            ));
            if let Err(error) = result {
                self.handle_error(error);
            }
        }
    }
}

/// Adds a feed item, dropping the oldest beyond `cap`. Returns how many
/// were dropped. Never blocks for longer than the lock.
pub(crate) fn push_feed(queue: &FeedQueue, item: FeedItem, cap: usize) -> usize {
    let Ok(mut queue) = queue.lock() else {
        return 1;
    };
    queue.push_back(item);
    let mut dropped = 0;
    while queue.len() > cap.max(1) {
        queue.pop_front();
        dropped += 1;
    }
    dropped
}

/// How long the worker waits after `failures` failures in a row.
pub fn backoff(options: &HostOptions, failures: u32) -> Duration {
    let exponent = failures.saturating_sub(1).min(16);
    options
        .backoff_initial
        .saturating_mul(1 << exponent)
        .min(options.backoff_max)
}
