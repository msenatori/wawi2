//! A stand-in for `claude -p` in stream-json mode, for tests only.
//!
//! It does the same handshake as the real CLI (initialize, then the MCP
//! `initialize` and `tools/list` over `mcp_message`) and acts on the last line
//! of each user message:
//!
//! - `say TEXT`: streams TEXT in small deltas and ends the turn.
//! - `tool NAME ARGS`: calls the MCP tool and says what it returned.
//! - `slow`: says one sentence and waits for an interrupt, then says a late one.
//! - `hang`: says one sentence and ignores the interrupt.
//! - `die`: starts a sentence and exits with an error.
//! - `fail`: ends the turn with an error result.
//! - `prompt`: says the last line of its system prompt.
//! - `compact`: reports a compaction of the conversation, then says "Listo.".
//!
//! With `FAKE_CLAUDE_SLOW_START` naming a file that exists when it starts,
//! it waits that file's number of milliseconds before reading anything, like
//! a slow cold start.
//!
//! With `FAKE_CLAUDE_LOG` set, it appends what it saw to that file: `spawn`
//! lines with its arguments, `tools`, `user`, `overlap` (a user message
//! that arrived while a turn was running) and `exit` when stdin closes.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

struct Fake {
    lines: io::Lines<io::StdinLock<'static>>,
    backlog: VecDeque<Value>,
    session: String,
    prompt: String,
    next_id: u64,
    in_turn: bool,
}

fn log(entry: &str) {
    if let Some(path) = std::env::var_os("FAKE_CLAUDE_LOG")
        && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(file, "{entry}");
    }
}

fn out(value: Value) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{value}");
    let _ = stdout.flush();
}

fn stream(event: Value) {
    out(json!({ "type": "stream_event", "event": event, "parent_tool_use_id": null }));
}

fn say(text: &str) {
    stream(
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
    );
    let chars: Vec<char> = text.chars().collect();
    for chunk in chars.chunks(3) {
        let delta: String = chunk.iter().collect();
        stream(
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": delta } }),
        );
    }
    stream(json!({ "type": "content_block_stop", "index": 0 }));
}

fn result(is_error: bool, errors: &[&str]) {
    out(json!({
        "type": "result",
        "subtype": if is_error { "error_during_execution" } else { "success" },
        "is_error": is_error,
        "errors": errors,
        "result": "",
    }));
}

fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let index = args.iter().position(|arg| arg == flag)?;
    args.get(index + 1).map(String::as_str)
}

impl Fake {
    fn read(&mut self) -> Option<Value> {
        loop {
            let line = self.lines.next()?.ok()?;
            if let Ok(value) = serde_json::from_str(&line) {
                return Some(value);
            }
        }
    }

    /// A user message that arrives early waits for its turn, as in the CLI.
    fn hold(&mut self, message: Value) {
        if self.in_turn {
            log("overlap");
        }
        self.backlog.push_back(message);
    }

    fn next_request_id(&mut self) -> String {
        self.next_id += 1;
        format!("cli_{}", self.next_id)
    }

    /// Sends an MCP request to the host and waits for its reply. Gives back
    /// `Err(interrupt_id)` if the host interrupts first.
    fn mcp(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let request_id = self.next_request_id();
        let rpc_id = self.next_id;
        out(json!({
            "type": "control_request",
            "request_id": request_id,
            "request": {
                "subtype": "mcp_message",
                "server_name": "wawi",
                "message": { "jsonrpc": "2.0", "id": rpc_id, "method": method, "params": params },
            },
        }));
        loop {
            let message = self.read().ok_or_else(String::new)?;
            match message["type"].as_str() {
                Some("control_response") if message["response"]["request_id"] == request_id => {
                    return Ok(message["response"]["response"]["mcp_response"].clone());
                }
                Some("control_request") if message["request"]["subtype"] == "interrupt" => {
                    let interrupt = message["request_id"].as_str().unwrap_or("").to_owned();
                    out(json!({
                        "type": "control_request",
                        "request_id": self.next_request_id(),
                        "request": {
                            "subtype": "mcp_message",
                            "server_name": "wawi",
                            "message": { "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": rpc_id } },
                        },
                    }));
                    return Err(interrupt);
                }
                Some("user") => self.hold(message),
                _ => {}
            }
        }
    }

    fn ack_interrupt(request_id: &str) {
        out(json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": request_id, "response": { "still_queued": [] } },
        }));
    }

    /// Waits for an interrupt; user messages that come meanwhile are kept.
    fn wait_for_interrupt(&mut self) -> Option<String> {
        loop {
            let message = self.read()?;
            match message["type"].as_str() {
                Some("control_request") if message["request"]["subtype"] == "interrupt" => {
                    return message["request_id"].as_str().map(str::to_owned);
                }
                Some("user") => self.hold(message),
                _ => {}
            }
        }
    }

    fn initialize(&mut self, request_id: &str) {
        let _ = self.mcp("initialize", json!({ "protocolVersion": "2025-11-25" }));
        out(json!({
            "type": "control_request",
            "request_id": self.next_request_id(),
            "request": { "subtype": "mcp_message", "server_name": "wawi", "message": { "jsonrpc": "2.0", "method": "notifications/initialized" } },
        }));
        if let Ok(reply) = self.mcp("tools/list", json!({})) {
            let names: Vec<&str> = reply["result"]["tools"]
                .as_array()
                .map(|tools| tools.iter().filter_map(|t| t["name"].as_str()).collect())
                .unwrap_or_default();
            log(&format!("tools {}", names.join(",")));
        }
        out(json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": request_id, "response": { "commands": [] } },
        }));
    }

    fn turn(&mut self, text: &str) {
        log(&format!("user {}", text.replace('\n', "\\n")));
        out(json!({ "type": "system", "subtype": "init", "session_id": self.session }));
        let command = text.lines().last().unwrap_or("");
        let (verb, rest) = command.split_once(' ').unwrap_or((command, ""));
        match verb {
            "say" => {
                say(rest);
                result(false, &[]);
            }
            "tool" => {
                let (name, args) = rest.split_once(' ').unwrap_or((rest, "{}"));
                let arguments: Value = serde_json::from_str(args).unwrap_or(json!({}));
                stream(
                    json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "tool_use", "id": "toolu_1", "name": format!("mcp__wawi__{name}"), "input": {} } }),
                );
                stream(json!({ "type": "content_block_stop", "index": 0 }));
                match self.mcp(
                    "tools/call",
                    json!({ "name": name, "arguments": arguments }),
                ) {
                    Ok(reply) => {
                        let content = reply["result"]["content"][0]["text"].as_str().unwrap_or("");
                        let failed = reply["result"]["isError"].as_bool().unwrap_or(false);
                        say(&format!(
                            "{}: {content}.",
                            if failed { "Error" } else { "Resultado" }
                        ));
                        result(false, &[]);
                    }
                    Err(interrupt) => {
                        Self::ack_interrupt(&interrupt);
                        result(true, &["interrupted"]);
                    }
                }
            }
            "slow" | "hang" => {
                say("Uno. ");
                let Some(interrupt) = self.wait_for_interrupt() else {
                    return;
                };
                if verb == "hang" {
                    // Never ends the turn; only a kill gets rid of it.
                    while self.read().is_some() {}
                    return;
                }
                Self::ack_interrupt(&interrupt);
                say("Tarde. ");
                result(true, &["interrupted"]);
            }
            "die" => {
                stream(
                    json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Me " } }),
                );
                eprintln!("fake crash");
                std::process::exit(3);
            }
            "fail" => result(true, &["fallo simulado"]),
            "compact" => {
                out(
                    json!({ "type": "system", "subtype": "compact_boundary", "session_id": self.session, "compact_metadata": { "trigger": "auto" } }),
                );
                say("Listo.");
                result(false, &[]);
            }
            "prompt" => {
                let last = self.prompt.lines().last().unwrap_or("").to_owned();
                say(&last);
                result(false, &[]);
            }
            _ => {
                say("No entendí.");
                result(false, &[]);
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    log(&format!(
        "spawn {}",
        serde_json::to_string(&args).unwrap_or_default()
    ));
    if let Some(path) = std::env::var_os("FAKE_CLAUDE_SLOW_START")
        && let Ok(ms) = std::fs::read_to_string(path)
        && let Ok(ms) = ms.trim().parse::<u64>()
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    let session = arg_after(&args, "--resume")
        .unwrap_or("fake-session")
        .to_owned();
    let prompt = arg_after(&args, "--system-prompt").unwrap_or("").to_owned();
    let mut fake = Fake {
        lines: io::stdin().lock().lines(),
        backlog: VecDeque::new(),
        session,
        prompt,
        next_id: 0,
        in_turn: false,
    };
    loop {
        let message = match fake.backlog.pop_front() {
            Some(message) => message,
            None => match fake.read() {
                Some(message) => message,
                None => break,
            },
        };
        match message["type"].as_str() {
            Some("control_request") => {
                let request_id = message["request_id"].as_str().unwrap_or("").to_owned();
                match message["request"]["subtype"].as_str() {
                    Some("initialize") => fake.initialize(&request_id),
                    // An interrupt with no turn running is just acknowledged.
                    Some("interrupt") => Fake::ack_interrupt(&request_id),
                    _ => {}
                }
            }
            Some("user") => {
                let text = message["message"]["content"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                fake.in_turn = true;
                fake.turn(&text);
                fake.in_turn = false;
            }
            _ => {}
        }
    }
    log("exit");
}
