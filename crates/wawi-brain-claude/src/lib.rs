//! Wawi's real brain: Claude Code in headless mode (`claude -p`), on the
//! owner's subscription.
//!
//! One `claude` process stays alive across turns and speaks stream-json on
//! stdin/stdout. Wawi's tools reach it as an in-process MCP server carried by
//! the control protocol (`mcp_message`), so there is no extra process. A tool
//! call from claude becomes a `BrainToolCall`; the MCP request stays open
//! until the core answers with `ToolResultToBrain`. The core decides every
//! permission and confirmation; this adapter only relays.
//!
//! Threads: one driver thread owns the process and all turn state; one thread
//! per process reads stdout, another keeps the tail of stderr. No async.

pub mod cupo;
mod driver;
pub mod limit;
pub mod protocol;
pub mod speech;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use wawi_core::{Brain, BrainContext, EventSink, ToolOutcome, ToolSpec, TurnId};
use wawi_memory::{soul_anchor, split_context};

use driver::{Driver, Input, Start};

/// How Wawi talks. The system prompt is this followed by the soul; the facts
/// travel in the user messages (see [`ClaudeConfig::memory_block`]).
pub const SPOKEN_STYLE: &str = "\
Sos Wawi, un asistente de voz que vive en la Mac de su dueño. Todo lo que escribís se lee en voz alta.
Cómo hablás:
- En español rioplatense, corto: una a tres frases, salvo que te pidan más.
- Sin markdown, listas, viñetas, emojis, código ni direcciones web: es voz, no texto.
- Números, horas y siglas, escritos como se dicen.
- Si hace falta una herramienta, usala sin anunciarlo. Si falla o te la rechazan, decilo en una frase y no insistas.
- Si no sabés algo, decilo.
- Cada mensaje empieza con [Ahora: ...], la fecha y la hora local de ese momento. Usala para saber qué día es y para calcular horarios; no la repitas si no viene al caso.
- Después viene una línea corta entre corchetes que te recuerda quién sos. Seguila siempre y no la repitas.";

/// The anchor when the soul has none of its own (a `> ` line, see
/// `wawi_memory::soul_anchor`). It rides on every turn, after the time, so a
/// long chat does not wear the persona off.
pub const DEFAULT_ANCHOR: &str =
    "Sos Wawi: juguetona, curiosa y cálida. Hablás corto, de vos, y primero respondés.";

/// The model a voice turn uses unless told otherwise. Measured against Opus
/// 5.5 and Haiku 5.5 on short spoken turns (docs/LATENCIA.md): Sonnet 5.5
/// starts writing in about 0.6 s instead of 4 s, and still answers in good
/// Rioplatense Spanish.
pub const DEFAULT_MODEL: &str = "claude-sonnet-5-5";
/// The effort a voice turn uses unless told otherwise. On Sonnet 5.5, `low`
/// skips thinking: as fast as its default and a bit cheaper.
pub const DEFAULT_EFFORT: &str = "low";

#[derive(Clone, Debug)]
pub struct ClaudeConfig {
    /// The `claude` executable.
    pub program: PathBuf,
    /// Neutral working directory: no project, no CLAUDE.md. Created if missing.
    pub workdir: PathBuf,
    /// `--model`. `None` means [`DEFAULT_MODEL`]; an empty string leaves the
    /// flag out, so the CLI's own default applies.
    pub model: Option<String>,
    /// `--effort` (low, medium, high, xhigh, max). Lower answers sooner.
    /// `None` means [`DEFAULT_EFFORT`]; an empty string leaves the flag out.
    pub effort: Option<String>,
    /// How long an interrupted turn may take to end before its process is
    /// killed and replaced.
    pub interrupt_grace: Duration,
    /// Whether a replacement process resumes the conversation (`--resume`).
    pub resume: bool,
    /// Spoken-style rules; the system prompt is these plus the soul.
    pub style: String,
    /// Whether every turn carries the soul's one-line anchor after the
    /// time ([`ClaudeConfig::anchor`]). On by default.
    pub anchor: bool,
    /// Extra environment for the process, after the parent's Claude settings
    /// are removed.
    pub env: HashMap<String, String>,
    /// Appends every line to and from the process here, with milliseconds
    /// since the driver started. For measuring and debugging only.
    pub trace: Option<PathBuf>,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            program: default_program(),
            workdir: std::env::temp_dir().join("wawi2-brain"),
            model: None,
            effort: None,
            interrupt_grace: Duration::from_secs(3),
            resume: true,
            style: SPOKEN_STYLE.to_owned(),
            anchor: true,
            env: HashMap::new(),
            trace: std::env::var_os("WAWI2_CLAUDE_TRACE").map(PathBuf::from),
        }
    }
}

impl ClaudeConfig {
    /// The style rules and the soul from a `BrainContext::memory` text (see
    /// `wawi_memory::split_context`). The facts are left out on purpose: they
    /// change often, and a different system prompt means a new process and a
    /// prompt cache that starts cold.
    pub fn system_prompt(&self, context: &str) -> String {
        let (soul, _) = split_context(context);
        if soul.is_empty() {
            self.style.clone()
        } else {
            format!("{}\n\n{soul}", self.style)
        }
    }

    /// The facts from a `BrainContext::memory` text, as a block that opens a
    /// user message. It is sent when the facts changed since the conversation
    /// last saw them. `None` when there are no facts at all.
    pub fn memory_block(context: &str) -> Option<String> {
        let (_, facts) = split_context(context);
        (!facts.is_empty()).then(|| {
            format!(
                "[Memoria de Wawi, al día. Reemplaza a cualquier versión anterior. \
                 No la menciones si no viene al caso.]\n{facts}\n[Fin de la memoria.]"
            )
        })
    }

    /// The one line that reminds the brain who it is, on every turn: the
    /// soul's own (`> ` line) or [`DEFAULT_ANCHOR`]. `None` when turned off.
    /// At most `wawi_memory::MAX_ANCHOR_CHARS` characters.
    pub fn anchor(&self, context: &str) -> Option<String> {
        if !self.anchor {
            return None;
        }
        let (soul, _) = split_context(context);
        Some(soul_anchor(soul).unwrap_or_else(|| DEFAULT_ANCHOR.to_owned()))
    }

    /// The `--model` passed to the CLI, if any.
    pub fn effective_model(&self) -> Option<&str> {
        Some(self.model.as_deref().unwrap_or(DEFAULT_MODEL)).filter(|model| !model.is_empty())
    }

    /// The `--effort` passed to the CLI, if any.
    pub fn effective_effort(&self) -> Option<&str> {
        Some(self.effort.as_deref().unwrap_or(DEFAULT_EFFORT)).filter(|effort| !effort.is_empty())
    }

    /// The command line, without the program. See README for why each flag.
    pub fn args(&self, system_prompt: &str, resume: Option<&str>) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            // No built-in tools: only Wawi's, through MCP.
            "--tools",
            "",
            // No MCP servers but Wawi's own.
            "--strict-mcp-config",
            "--mcp-config",
            &protocol::mcp_config(),
            // Wawi's tools run without asking claude's permission system: the
            // core already decided. Anything else is denied, never prompted.
            "--allowedTools",
            &format!("mcp__{}", protocol::SERVER_NAME),
            "--permission-mode",
            "dontAsk",
            // No user, project or local settings: no owner CLAUDE.md, hooks,
            // plugins or permissions.
            "--setting-sources=",
            "--disable-slash-commands",
            "--no-chrome",
            "--system-prompt",
            system_prompt,
            // Render the prompt fresh, so a resumed session picks up new memory.
            "--system-prompt-snapshot",
            "off",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        if let Some(model) = self.effective_model() {
            args.extend(["--model".to_owned(), model.to_owned()]);
        }
        if let Some(effort) = self.effective_effort() {
            args.extend(["--effort".to_owned(), effort.to_owned()]);
        }
        if let Some(session) = resume {
            args.extend(["--resume".to_owned(), session.to_owned()]);
        }
        args
    }
}

/// `$WAWI2_CLAUDE_BIN`, else `~/.local/bin/claude` if it exists, else
/// `claude` from the PATH.
pub fn default_program() -> PathBuf {
    if let Some(program) = std::env::var_os("WAWI2_CLAUDE_BIN") {
        return program.into();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let local = PathBuf::from(home).join(".local/bin/claude");
        if local.is_file() {
            return local;
        }
    }
    PathBuf::from("claude")
}

/// The `Brain` backed by `claude -p`. Calls never block: they hand the work to
/// the driver thread, and results come back through the sink, from that
/// thread.
pub struct ClaudeBrain {
    tx: Sender<Input>,
    driver: Option<JoinHandle<()>>,
    swaps: Arc<AtomicU64>,
}

impl ClaudeBrain {
    /// Starts the driver; the process itself starts on `prewarm` or on the
    /// first turn.
    pub fn new(config: ClaudeConfig, sink: Box<dyn EventSink + Send>) -> Self {
        let (tx, rx) = mpsc::channel();
        let swaps = Arc::new(AtomicU64::new(0));
        let driver = Driver::new(config, sink, tx.clone(), swaps.clone());
        let driver = thread::Builder::new()
            .name("wawi-brain-claude".into())
            .spawn(move || driver.run(rx))
            .expect("spawn the claude driver thread");
        Self {
            tx,
            driver: Some(driver),
            swaps,
        }
    }

    /// How many times a warmed-up replacement took over between turns.
    pub fn swaps(&self) -> u64 {
        self.swaps.load(Ordering::SeqCst)
    }

    /// Starts the process ahead of the first turn, so its cold start is not
    /// paid while the owner waits. Pass what the turns will carry. When a
    /// process already runs with another soul or other tools, a replacement
    /// warms up in the background and takes over between turns, once ready;
    /// turns never wait for it. Call it whenever the tools change.
    pub fn prewarm(&self, memory: &str, tools: &[ToolSpec]) {
        let _ = self.tx.send(Input::Prewarm {
            memory: memory.to_owned(),
            tools: tools.to_vec(),
        });
    }
}

impl Brain for ClaudeBrain {
    fn start(&mut self, turn: TurnId, text: &str, context: &BrainContext) {
        let _ = self.tx.send(Input::Start(Start {
            turn,
            text: text.to_owned(),
            context: context.clone(),
        }));
    }

    fn tool_result(&mut self, turn: TurnId, call_id: &str, outcome: &ToolOutcome) {
        let _ = self.tx.send(Input::ToolResult {
            turn,
            call_id: call_id.to_owned(),
            outcome: outcome.clone(),
        });
    }

    fn cancel(&mut self, turn: TurnId) {
        let _ = self.tx.send(Input::Cancel { turn });
    }

    fn prewarm(&mut self, memory: &str, tools: &[ToolSpec]) {
        ClaudeBrain::prewarm(self, memory, tools);
    }
}

impl Drop for ClaudeBrain {
    /// Ends the process before returning, so no `claude` outlives Wawi.
    fn drop(&mut self) {
        let _ = self.tx.send(Input::Shutdown);
        if let Some(driver) = self.driver.take() {
            let _ = driver.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_isolates_claude_and_resumes_when_asked() {
        let config = ClaudeConfig {
            effort: Some("low".into()),
            ..ClaudeConfig::default()
        };
        let args = config.args("PROMPT", Some("abc"));
        let joined = args.join(" ");
        for flag in [
            "--strict-mcp-config",
            "--setting-sources=",
            "--permission-mode dontAsk",
            "--allowedTools mcp__wawi",
            "--system-prompt PROMPT",
            "--effort low",
            "--resume abc",
        ] {
            assert!(joined.contains(flag), "{flag} in {joined}");
        }
        let tools = args.iter().position(|arg| arg == "--tools").unwrap();
        assert_eq!(args[tools + 1], "");
        assert!(!config.args("P", None).contains(&"--resume".to_owned()));
    }

    #[test]
    fn the_voice_defaults_apply_unless_overridden() {
        let joined = ClaudeConfig::default().args("P", None).join(" ");
        assert!(joined.contains("--model claude-sonnet-5-5"), "{joined}");
        assert!(joined.contains("--effort low"), "{joined}");
        let opus = ClaudeConfig {
            model: Some("claude-opus-5-5".into()),
            effort: Some(String::new()),
            ..ClaudeConfig::default()
        };
        let joined = opus.args("P", None).join(" ");
        assert!(joined.contains("--model claude-opus-5-5"), "{joined}");
        assert!(!joined.contains("--effort"), "{joined}");
    }

    #[test]
    fn the_system_prompt_is_style_then_soul_and_the_facts_go_apart() {
        let config = ClaudeConfig::default();
        let context = "SOUL\n\n# Lo que sabés\n\n- 2026-11-20: Toma mate.";
        assert_eq!(config.system_prompt("  "), SPOKEN_STYLE);
        assert_eq!(
            config.system_prompt(context),
            format!("{SPOKEN_STYLE}\n\nSOUL")
        );
        let block = ClaudeConfig::memory_block(context).unwrap();
        assert!(block.contains("- 2026-11-20: Toma mate."), "{block}");
        assert!(!block.contains("SOUL"), "{block}");
        // The anchor: the soul's own, else the default; off when asked.
        let with_anchor = "# Wawi\n\n> Sos Wawi y punto.\n\n# Lo que sabés\n\n- x";
        assert_eq!(
            config.anchor(with_anchor).as_deref(),
            Some("Sos Wawi y punto.")
        );
        assert_eq!(config.anchor(context).as_deref(), Some(DEFAULT_ANCHOR));
        assert!(DEFAULT_ANCHOR.chars().count() <= wawi_memory::MAX_ANCHOR_CHARS);
        let off = ClaudeConfig {
            anchor: false,
            ..ClaudeConfig::default()
        };
        assert_eq!(off.anchor(with_anchor), None);
        // Without the heading, everything is facts, never system prompt.
        assert_eq!(config.system_prompt("dato suelto"), SPOKEN_STYLE);
        assert!(ClaudeConfig::memory_block("dato suelto").is_some());
        assert_eq!(ClaudeConfig::memory_block(" "), None);
    }
}
