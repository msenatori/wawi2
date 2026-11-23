//! The thread that owns the `claude` process and everything about the turns
//! running in it. Brain calls, stdout lines and process exits all arrive as
//! `Input`s on one channel, so the state below has a single owner, like the
//! core's.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wawi_core::{
    BrainContext, Event, EventSink, SUGGEST_TOOL, ToolCall, ToolOutcome, ToolSpec, TurnId,
    TurnSource, suggest_spec,
};

use crate::ClaudeConfig;
use crate::protocol::{self, Incoming};
use crate::speech::SentenceBuffer;

/// Lines of the CLI's stderr kept to explain a crash.
const STDERR_TAIL: usize = 8;
/// How long a retired process may take to exit after its stdin closes.
const EXIT_GRACE: Duration = Duration::from_secs(2);

pub(crate) struct Start {
    pub turn: TurnId,
    pub text: String,
    pub context: BrainContext,
}

pub(crate) enum Input {
    Prewarm {
        memory: String,
        tools: Vec<ToolSpec>,
    },
    Start(Start),
    ToolResult {
        turn: TurnId,
        call_id: String,
        outcome: ToolOutcome,
    },
    Cancel {
        turn: TurnId,
    },
    Shutdown,
    Line {
        generation: u64,
        line: String,
    },
    Closed {
        generation: u64,
    },
}

struct Process {
    child: Child,
    stdin: ChildStdin,
    generation: u64,
    /// What it was started with; a turn that needs something else respawns it.
    prompt: String,
    tools: Vec<ToolSpec>,
    /// It said `system/init` at least once, so it came up fine.
    started: bool,
    resumed: bool,
    stderr: Arc<Mutex<VecDeque<String>>>,
    /// The id of the `initialize` control request; its answer means ready.
    init_request: String,
    ready: bool,
    /// `Driver::turns_done` when it was spawned. A standby that resumed the
    /// session before a later turn ended would not know that turn.
    turns_at_spawn: u64,
}

/// The turn whose user message is in the process now. Only one at a time:
/// the next one is written after this one's `result`.
struct Active {
    turn: TurnId,
    cancelled: bool,
    sentences: SentenceBuffer,
}

/// A tool call from claude waiting for the core's `ToolResultToBrain`. Its id
/// for the core is the control request id, unique per process.
struct PendingCall {
    turn: TurnId,
    rpc_id: Value,
}

pub(crate) struct Driver {
    config: ClaudeConfig,
    sink: Box<dyn EventSink + Send>,
    tx: Sender<Input>,
    process: Option<Process>,
    /// A replacement warming up in the background (new soul or new tools).
    /// Turns keep using `process` until it is ready; it is swapped in
    /// between turns, so no turn ever waits for a spawn.
    standby: Option<Process>,
    /// The prompt and tools turns should have, once nothing is running.
    wanted: Option<(String, Vec<ToolSpec>)>,
    /// Turns that ended in the current conversation.
    turns_done: u64,
    next_generation: u64,
    next_request: u64,
    session_id: Option<String>,
    /// Every tool any turn was offered, plus `suggest`. Exposing the union
    /// keeps owner and initiative turns on the same process; the core refuses
    /// whatever a turn may not use.
    exposed: Vec<ToolSpec>,
    active: Option<Active>,
    /// A start that waits for the cancelled turn's `result`.
    queued: Option<Start>,
    interrupt_deadline: Option<Instant>,
    calls: HashMap<String, PendingCall>,
    /// The memory block the conversation last saw in this process. A turn
    /// whose facts differ carries the new block; cleared on every spawn and
    /// compaction, so the facts are never lost from view.
    memory_sent: Option<String>,
    trace: Option<(File, Instant)>,
    swaps: Arc<AtomicU64>,
}

impl Driver {
    pub fn new(
        config: ClaudeConfig,
        sink: Box<dyn EventSink + Send>,
        tx: Sender<Input>,
        swaps: Arc<AtomicU64>,
    ) -> Self {
        Self {
            swaps,
            config,
            sink,
            tx,
            process: None,
            standby: None,
            wanted: None,
            turns_done: 0,
            next_generation: 0,
            next_request: 0,
            session_id: None,
            exposed: vec![suggest_spec()],
            active: None,
            queued: None,
            interrupt_deadline: None,
            calls: HashMap::new(),
            memory_sent: None,
            trace: None,
        }
    }

    fn trace(&mut self, direction: &str, line: &str) {
        if let Some((file, since)) = self.trace.as_mut() {
            let ms = since.elapsed().as_millis();
            let _ = writeln!(file, "{ms:>8} {direction} {}", line.trim_end());
        }
    }

    pub fn run(mut self, rx: Receiver<Input>) {
        self.trace = self.config.trace.as_ref().and_then(|path| {
            let file = File::options().create(true).append(true).open(path).ok()?;
            Some((file, Instant::now()))
        });
        loop {
            let input = match self.interrupt_deadline {
                Some(deadline) => {
                    let wait = deadline.saturating_duration_since(Instant::now());
                    match rx.recv_timeout(wait) {
                        Ok(input) => input,
                        Err(RecvTimeoutError::Timeout) => {
                            self.interrupt_expired();
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                None => match rx.recv() {
                    Ok(input) => input,
                    Err(_) => break,
                },
            };
            match input {
                Input::Prewarm { memory, tools } => self.prewarm(&memory, &tools),
                Input::Start(start) => {
                    self.trace("--", &format!("start turn {}", start.turn.0));
                    self.on_start(start);
                }
                Input::ToolResult {
                    turn,
                    call_id,
                    outcome,
                } => self.on_tool_result(turn, &call_id, &outcome),
                Input::Cancel { turn } => self.on_cancel(turn),
                Input::Shutdown => break,
                Input::Line { generation, line } => {
                    if self.is_current(generation) {
                        self.trace("<<", &line);
                        self.on_line(&line);
                    } else if self.is_standby(generation) {
                        self.trace("<s", &line);
                        self.on_standby_line(&line);
                    }
                }
                Input::Closed { generation } => {
                    if self.is_current(generation) {
                        self.on_closed();
                    } else if self.is_standby(generation)
                        && let Some(standby) = self.standby.take()
                    {
                        // Tried again the next time something asks for it.
                        retire(standby, false);
                    }
                }
            }
        }
        if let Some(process) = self.process.take() {
            retire(process, true);
        }
        if let Some(standby) = self.standby.take() {
            retire(standby, true);
        }
    }

    fn is_standby(&self, generation: u64) -> bool {
        self.standby
            .as_ref()
            .is_some_and(|process| process.generation == generation)
    }

    fn is_current(&self, generation: u64) -> bool {
        self.process
            .as_ref()
            .is_some_and(|process| process.generation == generation)
    }

    fn emit(&mut self, event: Event) {
        if self.trace.is_some() {
            self.trace("->", &format!("{event:?}"));
        }
        self.sink.send(event);
    }

    /// Starts the process, or, if one runs with something else, warms a
    /// replacement in the background. Never makes a turn wait.
    fn prewarm(&mut self, memory: &str, tools: &[ToolSpec]) {
        let prompt = self.config.system_prompt(memory);
        let tools = merge(&self.exposed, tools);
        // A failure here shows up again, with a reason, on the next turn.
        if self.want(prompt, tools).is_ok() {
            self.warm_standby();
        }
    }

    /// Records what turns should run with. With no process it spawns one
    /// (nothing to keep using); otherwise the change waits for a standby.
    fn want(&mut self, prompt: String, tools: Vec<ToolSpec>) -> Result<(), String> {
        self.exposed = tools.clone();
        if self.process.is_none() {
            if let Some(standby) = self.standby.take() {
                retire(standby, false);
            }
            self.wanted = None;
            let process = self.spawn(prompt, tools)?;
            self.install(process);
            return Ok(());
        }
        let current = self
            .process
            .as_ref()
            .is_some_and(|p| p.prompt == prompt && p.tools == tools);
        if current {
            self.wanted = None;
            if let Some(standby) = self.standby.take() {
                retire(standby, false);
            }
        } else {
            self.wanted = Some((prompt, tools));
        }
        Ok(())
    }

    fn busy(&self) -> bool {
        self.active.is_some() || self.queued.is_some() || self.interrupt_deadline.is_some()
    }

    /// Spawns the standby for `wanted`, unless one with it already warms.
    /// Only between turns: a standby spawned during a turn would resume the
    /// session without that turn.
    fn warm_standby(&mut self) {
        if self.busy() {
            return;
        }
        let Some((prompt, tools)) = self.wanted.clone() else {
            return;
        };
        if let Some(standby) = &self.standby {
            let fresh = standby.turns_at_spawn == self.turns_done;
            if fresh && standby.prompt == prompt && standby.tools == tools {
                return self.try_swap();
            }
        }
        if let Some(old) = self.standby.take() {
            retire(old, false);
        }
        self.trace("--", "warming a standby");
        if let Ok(standby) = self.spawn(prompt, tools) {
            self.standby = Some(standby);
        }
    }

    /// Puts a ready standby in place of the current process, between turns.
    /// A stale one (a turn ended since it resumed the session) is replaced
    /// by a fresh standby instead.
    fn try_swap(&mut self) {
        if self.busy() {
            return;
        }
        let Some(standby) = &self.standby else {
            return;
        };
        if standby.turns_at_spawn != self.turns_done {
            return self.warm_standby_again();
        }
        if !standby.ready {
            return;
        }
        let Some(standby) = self.standby.take() else {
            return;
        };
        self.trace("--", &format!("swap in generation {}", standby.generation));
        if let Some(old) = self.process.take() {
            retire(old, false);
        }
        self.calls.clear();
        self.wanted = None;
        self.install(standby);
        self.swaps.fetch_add(1, Ordering::SeqCst);
    }

    fn warm_standby_again(&mut self) {
        if let Some(old) = self.standby.take() {
            retire(old, false);
        }
        self.warm_standby();
    }

    fn install(&mut self, process: Process) {
        // Cheap to repeat, and a fresh session would not have the facts.
        self.memory_sent = None;
        self.process = Some(process);
    }

    /// The standby's own lines: its MCP handshake, and the answer to its
    /// `initialize`, which makes it ready. It never runs a turn.
    fn on_standby_line(&mut self, line: &str) {
        let Some(incoming) = protocol::parse_line(line) else {
            return;
        };
        match incoming {
            Incoming::ControlResponse { request_id, .. } => {
                if let Some(standby) = self.standby.as_mut()
                    && standby.init_request == request_id
                {
                    standby.ready = true;
                    self.try_swap();
                }
            }
            Incoming::SessionStarted { .. } => {
                if let Some(standby) = self.standby.as_mut() {
                    standby.started = true;
                }
            }
            Incoming::ControlRequest {
                request_id,
                request,
            } => {
                let reply = match request.get("subtype").and_then(Value::as_str) {
                    Some("mcp_message") => {
                        let message = request.get("message").cloned().unwrap_or(Value::Null);
                        let tools = self
                            .standby
                            .as_ref()
                            .map(|p| p.tools.clone())
                            .unwrap_or_default();
                        protocol::mcp_reply(&request_id, mcp_answer(&message, &tools))
                    }
                    Some("can_use_tool") => {
                        let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
                        protocol::control_success(
                            &request_id,
                            json!({ "behavior": "allow", "updatedInput": input }),
                        )
                    }
                    other => protocol::control_error(
                        &request_id,
                        &format!("unsupported control request: {}", other.unwrap_or("?")),
                    ),
                };
                if let Some(standby) = self.standby.as_mut() {
                    write_to(standby, &reply);
                }
            }
            _ => {}
        }
    }

    fn on_start(&mut self, start: Start) {
        if let Some(active) = &self.active {
            // The core always cancels before starting again; this only guards
            // against a host that does not.
            if !active.cancelled {
                self.cancel_active();
            }
            self.queued = Some(start);
            return;
        }
        self.launch(start);
    }

    fn launch(&mut self, start: Start) {
        let prompt = self.config.system_prompt(&start.context.memory);
        let tools = merge(&self.exposed, &start.context.tools);
        // A ready standby goes in now; otherwise the turn runs on what is
        // there and the change waits in the background.
        self.try_swap();
        if let Err(reason) = self.want(prompt, tools) {
            self.emit(Event::BrainFailed {
                turn: start.turn,
                reason,
            });
            return;
        }
        let offered = self
            .process
            .as_ref()
            .map_or_else(|| self.exposed.clone(), |p| p.tools.clone());
        let anchor = self.config.anchor(&start.context.memory);
        let mut text = user_text(&start, &offered, anchor.as_deref());
        let block = ClaudeConfig::memory_block(&start.context.memory);
        if block.is_some() && block != self.memory_sent {
            if let Some(block) = &block {
                text = format!("{block}\n{text}");
            }
            self.memory_sent = block;
        }
        self.send(&protocol::user_message(&text));
        self.active = Some(Active {
            turn: start.turn,
            cancelled: false,
            sentences: SentenceBuffer::default(),
        });
    }

    fn on_cancel(&mut self, turn: TurnId) {
        if self.queued.as_ref().is_some_and(|start| start.turn == turn) {
            self.queued = None;
        }
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.turn == turn && !active.cancelled)
        {
            self.cancel_active();
        }
    }

    /// Interrupts the running turn. Its `result` still comes, and until then
    /// whatever it says is tagged with its old turn id, so the core drops it.
    fn cancel_active(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        active.cancelled = true;
        let turn = active.turn;
        // The CLI cancels its own MCP calls on interrupt; nothing to answer.
        self.calls.retain(|_, call| call.turn != turn);
        let request_id = self.request_id();
        self.send(&protocol::interrupt(&request_id));
        self.interrupt_deadline = Some(Instant::now() + self.config.interrupt_grace);
    }

    /// The interrupted turn never ended: the process is stuck, so it goes.
    fn interrupt_expired(&mut self) {
        self.interrupt_deadline = None;
        if let Some(process) = self.process.take() {
            retire(process, false);
        }
        if self.end_active().is_some() {
            self.turns_done += 1;
        }
        self.between_turns();
    }

    /// Nothing runs: the queued turn starts, or a pending change warms up.
    fn between_turns(&mut self) {
        if let Some(start) = self.queued.take() {
            self.launch(start);
        } else {
            self.warm_standby();
        }
    }

    fn on_tool_result(&mut self, turn: TurnId, call_id: &str, outcome: &ToolOutcome) {
        // A result for a call of an older turn has nobody waiting for it.
        if self.calls.get(call_id).is_none_or(|call| call.turn != turn) {
            return;
        }
        let Some(call) = self.calls.remove(call_id) else {
            return;
        };
        let (text, is_error) = match outcome {
            ToolOutcome::Success(text) => (text.as_str(), false),
            ToolOutcome::Failure(reason) => (reason.as_str(), true),
        };
        let result = protocol::tool_call_result(text, is_error);
        self.send(&protocol::mcp_reply(
            call_id,
            protocol::rpc_result(&call.rpc_id, result),
        ));
    }

    fn on_line(&mut self, line: &str) {
        let Some(incoming) = protocol::parse_line(line) else {
            return;
        };
        match incoming {
            Incoming::ControlRequest {
                request_id,
                request,
            } => self.on_control_request(&request_id, &request),
            Incoming::ControlCancel { request_id } => {
                self.calls.remove(&request_id);
            }
            Incoming::ControlResponse { .. } | Incoming::Other => {}
            // The summary may have dropped the facts: send them again.
            Incoming::Compacted => self.memory_sent = None,
            Incoming::SessionStarted { session_id } => {
                self.session_id = Some(session_id);
                if let Some(process) = self.process.as_mut() {
                    process.started = true;
                }
            }
            Incoming::TextDelta(delta) => {
                if let Some(active) = self.active.as_mut() {
                    let sentences = active.sentences.push(&delta);
                    let turn = active.turn;
                    for text in sentences {
                        self.emit(Event::BrainText { turn, text });
                    }
                }
            }
            Incoming::BlockStop => self.flush_text(),
            Incoming::Result { is_error, reason } => self.on_result(is_error, reason),
        }
    }

    fn flush_text(&mut self) {
        if let Some(active) = self.active.as_mut() {
            let turn = active.turn;
            for text in active.sentences.flush() {
                self.emit(Event::BrainText { turn, text });
            }
        }
    }

    fn on_result(&mut self, is_error: bool, reason: String) {
        self.flush_text();
        let Some(active) = self.end_active() else {
            return;
        };
        self.turns_done += 1;
        self.interrupt_deadline = None;
        if !active.cancelled {
            let turn = active.turn;
            self.emit(if is_error {
                Event::BrainFailed { turn, reason }
            } else {
                Event::BrainDone { turn }
            });
        }
        self.between_turns();
    }

    fn end_active(&mut self) -> Option<Active> {
        let active = self.active.take()?;
        self.calls.retain(|_, call| call.turn != active.turn);
        Some(active)
    }

    fn on_closed(&mut self) {
        let Some(process) = self.process.take() else {
            return;
        };
        if process.resumed && !process.started {
            // Resuming that session may be what kills it: start clean next time.
            self.session_id = None;
        }
        let tail = process
            .stderr
            .lock()
            .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join(" | "))
            .unwrap_or_default();
        retire(process, false);
        self.interrupt_deadline = None;
        self.calls.clear();
        let active = self.active.take();
        if active.is_some() {
            self.turns_done += 1;
        }
        // A fresh standby takes over at once.
        if self
            .standby
            .as_ref()
            .is_some_and(|standby| standby.turns_at_spawn == self.turns_done)
            && let Some(standby) = self.standby.take()
        {
            self.wanted = None;
            self.install(standby);
        }
        if let Some(active) = active
            && !active.cancelled
        {
            let reason = if tail.is_empty() {
                "claude se cerró".to_owned()
            } else {
                format!("claude se cerró: {tail}")
            };
            self.emit(Event::BrainFailed {
                turn: active.turn,
                reason,
            });
        }
        self.between_turns();
    }

    fn on_control_request(&mut self, request_id: &str, request: &Value) {
        match request.get("subtype").and_then(Value::as_str) {
            Some("mcp_message") => {
                let message = request.get("message").cloned().unwrap_or(Value::Null);
                self.on_mcp(request_id, &message);
            }
            // Never asked while the MCP server is pre-allowed. If it is, the
            // answer is yes: permissions are the core's job, not claude's.
            Some("can_use_tool") => {
                let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
                self.send(&protocol::control_success(
                    request_id,
                    json!({ "behavior": "allow", "updatedInput": input }),
                ));
            }
            other => {
                let error = format!("unsupported control request: {}", other.unwrap_or("?"));
                self.send(&protocol::control_error(request_id, &error));
            }
        }
    }

    fn on_mcp(&mut self, request_id: &str, message: &Value) {
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = message.get("id").cloned() else {
            if method == "notifications/cancelled"
                && let Some(cancelled) = params.get("requestId")
            {
                self.calls.retain(|_, call| &call.rpc_id != cancelled);
            }
            self.send(&protocol::mcp_reply(request_id, protocol::rpc_ack()));
            return;
        };
        let reply = match method {
            "initialize" => protocol::rpc_result(&id, protocol::mcp_initialize_result(&params)),
            "tools/list" => {
                let tools = self
                    .process
                    .as_ref()
                    .map_or(&self.exposed, |process| &process.tools);
                protocol::rpc_result(&id, protocol::tools_list_result(tools))
            }
            "tools/call" => return self.on_tool_call(request_id, id, &params),
            "ping" => protocol::rpc_result(&id, json!({})),
            _ => protocol::rpc_error(&id, -32601, &format!("method not found: {method}")),
        };
        self.send(&protocol::mcp_reply(request_id, reply));
    }

    /// Hands the call to the core. The MCP request stays open until the core
    /// answers with `ToolResultToBrain`, however long a confirmation takes.
    fn on_tool_call(&mut self, request_id: &str, rpc_id: Value, params: &Value) {
        let live_turn = self
            .active
            .as_ref()
            .filter(|active| !active.cancelled)
            .map(|active| active.turn);
        let Some(turn) = live_turn else {
            let result = protocol::tool_call_result("el turno ya terminó", true);
            self.send(&protocol::mcp_reply(
                request_id,
                protocol::rpc_result(&rpc_id, result),
            ));
            return;
        };
        // What was said before the call goes out before the call does.
        self.flush_text();
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let args_json = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}))
            .to_string();
        self.calls
            .insert(request_id.to_owned(), PendingCall { turn, rpc_id });
        self.emit(Event::BrainToolCall {
            turn,
            call: ToolCall {
                id: request_id.to_owned(),
                name,
                args_json,
            },
        });
    }

    fn request_id(&mut self) -> String {
        self.next_request += 1;
        format!("wawi_{}", self.next_request)
    }

    /// Write errors are ignored on purpose: a dead process is noticed, with
    /// its reason, when its stdout closes.
    fn send(&mut self, value: &Value) {
        let line = format!("{value}\n");
        self.trace(">>", &line);
        if let Some(process) = self.process.as_mut() {
            let _ = process
                .stdin
                .write_all(line.as_bytes())
                .and_then(|()| process.stdin.flush());
        }
    }

    /// Starts a `claude` and sends its `initialize`. The caller decides
    /// whether it is the current process or the standby.
    fn spawn(&mut self, prompt: String, tools: Vec<ToolSpec>) -> Result<Process, String> {
        std::fs::create_dir_all(&self.config.workdir).map_err(|error| {
            format!(
                "no pude crear {}: {error}",
                self.config.workdir.to_string_lossy()
            )
        })?;
        let resume = if self.config.resume {
            self.session_id.clone()
        } else {
            None
        };
        let mut command = Command::new(&self.config.program);
        command
            .args(self.config.args(&prompt, resume.as_deref()))
            .current_dir(&self.config.workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, _) in std::env::vars_os() {
            if is_inherited_claude_setting(&key.to_string_lossy()) {
                command.env_remove(&key);
            }
        }
        command.envs(&self.config.env);
        let mut child = command.spawn().map_err(|error| {
            format!(
                "no pude arrancar {}: {error}",
                self.config.program.to_string_lossy()
            )
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("claude arrancó sin sus tuberías".into());
        };
        self.next_generation += 1;
        let generation = self.next_generation;
        let tx = self.tx.clone();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(Input::Line { generation, line }).is_err() {
                    return;
                }
            }
            let _ = tx.send(Input::Closed { generation });
        });
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_tail = tail.clone();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if let Ok(mut lines) = stderr_tail.lock() {
                    if lines.len() == STDERR_TAIL {
                        lines.pop_front();
                    }
                    lines.push_back(line);
                }
            }
        });
        self.trace(
            "--",
            &format!("spawn generation {generation}, resume {resume:?}"),
        );
        let init_request = self.request_id();
        let mut process = Process {
            child,
            stdin,
            generation,
            prompt,
            tools,
            started: false,
            resumed: resume.is_some(),
            stderr: tail,
            init_request: init_request.clone(),
            ready: false,
            turns_at_spawn: self.turns_done,
        };
        let line = protocol::initialize(&init_request);
        self.trace(">>", &line.to_string());
        write_to(&mut process, &line);
        Ok(process)
    }
}

/// Writes one message to a process. Errors are ignored, as in `send`.
fn write_to(process: &mut Process, value: &Value) {
    let line = format!("{value}\n");
    let _ = process
        .stdin
        .write_all(line.as_bytes())
        .and_then(|()| process.stdin.flush());
}

/// The answer to an MCP message that is not a `tools/call`, from a process
/// offering `tools`. Notifications get the protocol's empty ack.
fn mcp_answer(message: &Value, tools: &[ToolSpec]) -> Value {
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = message.get("id").cloned() else {
        return protocol::rpc_ack();
    };
    match method {
        "initialize" => protocol::rpc_result(&id, protocol::mcp_initialize_result(&params)),
        "tools/list" => protocol::rpc_result(&id, protocol::tools_list_result(tools)),
        "ping" => protocol::rpc_result(&id, json!({})),
        "tools/call" => protocol::rpc_result(
            &id,
            protocol::tool_call_result("este proceso todavía no atiende turnos", true),
        ),
        _ => protocol::rpc_error(&id, -32601, &format!("method not found: {method}")),
    }
}

/// Settings of a parent Claude Code session (when Wawi itself runs inside
/// one) or an API key: neither may leak into the brain, which must use the
/// owner's subscription and nothing else.
fn is_inherited_claude_setting(key: &str) -> bool {
    key == "CLAUDECODE"
        || key == "CLAUDE_PID"
        || key == "CLAUDE_EFFORT"
        || key.starts_with("CLAUDE_CODE_")
        || key.starts_with("ANTHROPIC_")
}

/// Closes stdin, which ends the CLI, and makes sure it is gone. `wait`
/// blocks until then; otherwise a thread does it.
fn retire(process: Process, wait: bool) {
    let Process {
        mut child, stdin, ..
    } = process;
    let reap = move || {
        drop(stdin);
        let deadline = Instant::now() + EXIT_GRACE;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    };
    if wait {
        reap();
    } else {
        thread::spawn(reap);
    }
}

/// `base` with every tool of `more` added or replaced by name.
fn merge(base: &[ToolSpec], more: &[ToolSpec]) -> Vec<ToolSpec> {
    let mut tools = base.to_vec();
    for tool in more {
        match tools.iter_mut().find(|known| known.name == tool.name) {
            Some(known) => *known = tool.clone(),
            None => tools.push(tool.clone()),
        }
    }
    tools
}

/// The user message for a turn: the local date and time when the host gave
/// one, the soul's one-line anchor, then the turn itself. Both ride here and
/// not in the system prompt, so the prompt stays the same and its cache
/// keeps working; the anchor is a few dozen tokens a turn.
fn user_text(start: &Start, exposed: &[ToolSpec], anchor: Option<&str>) -> String {
    let mut text = String::new();
    let now = start.context.now.trim();
    if !now.is_empty() {
        text.push_str(&format!("[Ahora: {now}.]\n"));
    }
    if let Some(anchor) = anchor.map(str::trim).filter(|a| !a.is_empty()) {
        text.push_str(&format!("[{anchor}]\n"));
    }
    text.push_str(&turn_text(start, exposed));
    text
}

/// The process exposes more tools than a turn may use; when that matters,
/// the turn says which ones it has.
fn turn_text(start: &Start, exposed: &[ToolSpec]) -> String {
    let allowed: Vec<&str> = start
        .context
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    let list = if allowed.is_empty() {
        "ninguna".to_owned()
    } else {
        allowed.join(", ")
    };
    if start.context.source == TurnSource::Initiative {
        return format!(
            "[Iniciativa de Wawi: esto no lo dijo el dueño, es una tarea tuya. \
             Herramientas permitidas: {list}. Lo que no puedas hacer con ellas, \
             proponelo con {SUGGEST_TOOL}.]\n{}",
            start.text
        );
    }
    let all_offered = exposed
        .iter()
        .filter(|tool| tool.name != SUGGEST_TOOL)
        .all(|tool| allowed.contains(&tool.name.as_str()));
    if all_offered {
        start.text.clone()
    } else {
        format!(
            "[Herramientas disponibles en este turno: {list}.]\n{}",
            start.text
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wawi_core::PermissionLevel;

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: format!("{name} tool"),
            params_schema_json: r#"{"type":"object"}"#.into(),
            permission: PermissionLevel::Read,
            needs_confirmation: false,
            ack: None,
        }
    }

    fn start(source: TurnSource, tools: Vec<ToolSpec>) -> Start {
        Start {
            turn: TurnId(1),
            text: "hola".into(),
            context: BrainContext {
                tools,
                source,
                memory: String::new(),
                now: String::new(),
            },
        }
    }

    #[test]
    fn merge_adds_and_replaces_by_name() {
        let mut changed = tool("a");
        changed.description = "new".into();
        let merged = merge(&[tool("a"), tool("b")], &[changed.clone(), tool("c")]);
        assert_eq!(merged, vec![changed, tool("b"), tool("c")]);
    }

    #[test]
    fn owner_turns_with_every_tool_send_the_text_alone() {
        let exposed = vec![suggest_spec(), tool("a")];
        let owner = start(TurnSource::Owner, vec![tool("a")]);
        assert_eq!(user_text(&owner, &exposed, None), "hola");
        let fewer = start(TurnSource::Owner, vec![]);
        assert!(user_text(&fewer, &exposed, None).starts_with("[Herramientas disponibles"));
    }

    #[test]
    fn the_local_time_opens_the_message_when_the_host_gives_it() {
        let exposed = vec![suggest_spec(), tool("a")];
        let mut owner = start(TurnSource::Owner, vec![tool("a")]);
        owner.context.now = "viernes 20 de noviembre de 2026, 16:23".into();
        assert_eq!(
            user_text(&owner, &exposed, None),
            "[Ahora: viernes 20 de noviembre de 2026, 16:23.]\nhola"
        );
        assert_eq!(
            user_text(&owner, &exposed, Some("Sos Wawi.")),
            "[Ahora: viernes 20 de noviembre de 2026, 16:23.]\n[Sos Wawi.]\nhola"
        );
    }

    #[test]
    fn initiative_turns_are_marked() {
        let exposed = vec![suggest_spec(), tool("a")];
        let initiative = start(TurnSource::Initiative, vec![suggest_spec()]);
        let text = user_text(&initiative, &exposed, None);
        assert!(text.contains("Iniciativa") && text.contains("permitidas: suggest"));
        assert!(text.ends_with("\nhola"));
    }

    #[test]
    fn parent_session_settings_and_api_keys_are_not_inherited() {
        for key in [
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "ANTHROPIC_API_KEY",
            "CLAUDE_PID",
        ] {
            assert!(is_inherited_claude_setting(key), "{key}");
        }
        assert!(!is_inherited_claude_setting("CLAUDE_CONFIG_DIR"));
        assert!(!is_inherited_claude_setting("HOME"));
    }
}
