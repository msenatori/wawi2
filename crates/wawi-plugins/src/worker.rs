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

use crate::feed::FeedItem;
use crate::log::CallLog;
use crate::manifest::{FEED_TOOL, Manifest, Server, truncate};
use crate::mcp::{Client, HttpTransport, McpError, OnNotify, StdioSpec, StdioTransport, Transport};
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
    Shutdown,
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
    client: Option<Client>,
    generation: u64,
    failures: u32,
    next_try: Instant,
    last_error: String,
}

impl Worker {
    pub fn new(
        manifest: Manifest,
        secrets: Secrets,
        sink: Arc<dyn PluginSink>,
        log: CallLog,
        jobs: Sender<Job>,
        options: HostOptions,
        feed: FeedQueue,
    ) -> Self {
        Self {
            manifest,
            secrets,
            sink,
            log,
            jobs,
            options,
            feed,
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
            let job = if self.client.is_some() {
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
                Job::Relist { .. } | Job::Closed { .. } => {}
            }
        }
        self.log.line(&format!("{} cerrado", self.name()));
    }

    fn transport(&self, generation: u64) -> Result<Box<dyn Transport>, String> {
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
        let resolve = |text: &str| self.secrets.resolve(text);
        match &self.manifest.server {
            Server::Stdio { command, args, env } => {
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
                let headers = headers
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), resolve(v)?)))
                    .collect::<Result<Vec<_>, String>>()?;
                Ok(Box::new(HttpTransport::new(&url, headers, on_notify)))
            }
        }
    }

    fn connect(&mut self) {
        self.generation += 1;
        let generation = self.generation;
        let timeout = self.manifest.timeout;
        let connected = self
            .transport(generation)
            .and_then(|transport| Client::connect(transport, timeout).map_err(|e| e.to_string()))
            .and_then(|mut client| {
                let listed = client.list_tools(timeout).map_err(|e| e.to_string())?;
                Ok((client, listed))
            });
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
        if let McpError::Dead(why) = error {
            self.fail(why);
        }
    }

    fn call(&mut self, origin: Origin, tool: &str, args: Value) {
        if self.client.is_none() && Instant::now() >= self.next_try {
            self.connect();
        }
        let from = match &origin {
            Origin::Brain { turn, .. } => format!("cerebro turno {}", turn.0),
            Origin::Watch { key } => format!("watch {key}"),
        };
        let timeout = self.manifest.timeout;
        let Some(client) = self.client.as_mut() else {
            let wait = self.next_try.saturating_duration_since(Instant::now());
            let reason = format!(
                "el plugin {} no está disponible ({}); reintento en {} s",
                self.manifest.name,
                self.last_error,
                wait.as_secs_f32().ceil()
            );
            self.log.line(&format!(
                "{} {tool} desde {from}: no disponible",
                self.manifest.name
            ));
            return self.respond(origin, Err(reason));
        };
        let started = Instant::now();
        let result = client.call(tool, args, timeout);
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
