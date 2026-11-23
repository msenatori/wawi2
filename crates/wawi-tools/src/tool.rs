//! A tool that answers on the spot: it parses its arguments, does its work
//! and reports `ToolFinished` through its sink before `start` returns. Every
//! tool here is like that: a file read or write, nothing to wait for.

use std::rc::Rc;

use serde_json::{Map, Value};
use wawi_core::{Event, EventSink, PermissionLevel, Tool, ToolCall, ToolOutcome, ToolSpec, TurnId};

/// What a tool does with its arguments: the text for the brain, or why not.
pub type Action = Box<dyn FnMut(&Map<String, Value>) -> Result<String, String>>;

pub struct InstantTool {
    spec: ToolSpec,
    action: Action,
    sink: Rc<dyn EventSink>,
}

impl InstantTool {
    pub fn new(spec: ToolSpec, action: Action, sink: Rc<dyn EventSink>) -> Self {
        Self { spec, action, sink }
    }

    /// Runs the tool without a sink, for the simulator's commands and tests.
    pub fn run(&mut self, args_json: &str) -> Result<String, String> {
        let args = match serde_json::from_str::<Value>(args_json) {
            Ok(Value::Object(args)) => args,
            Ok(_) => return Err("los argumentos tienen que ser un objeto JSON".into()),
            Err(error) => return Err(format!("argumentos inválidos: {error}")),
        };
        (self.action)(&args)
    }
}

impl Tool for InstantTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn start(&mut self, turn: TurnId, call: &ToolCall) {
        let outcome = match self.run(&call.args_json) {
            Ok(text) => ToolOutcome::Success(text),
            Err(reason) => ToolOutcome::Failure(reason),
        };
        self.sink.send(Event::ToolFinished {
            turn,
            call_id: call.id.clone(),
            outcome,
        });
    }

    /// It already finished: there is nothing left to stop.
    fn cancel(&mut self, _turn: TurnId, _call_id: &str) {}
}

pub fn spec(name: &str, description: &str, schema: Value, permission: PermissionLevel) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        params_schema_json: schema.to_string(),
        permission,
        needs_confirmation: false,
        ack: None,
    }
}

/// A required, non-blank string argument, trimmed.
pub fn text_arg<'a>(args: &'a Map<String, Value>, name: &str) -> Result<&'a str, String> {
    match args.get(name) {
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(text.trim()),
        Some(Value::String(_)) => Err(format!("«{name}» está vacío")),
        Some(_) => Err(format!("«{name}» tiene que ser un texto")),
        None => Err(format!("falta «{name}»")),
    }
}

/// An optional string argument: absent, null or blank is `None`.
pub fn optional_text<'a>(
    args: &'a Map<String, Value>,
    name: &str,
) -> Result<Option<&'a str>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.trim())),
        Some(_) => Err(format!("«{name}» tiene que ser un texto")),
    }
}

/// An optional whole number. `30.0` counts; `30.5`, `"30"` and `true` do not.
pub fn optional_integer(args: &Map<String, Value>, name: &str) -> Result<Option<i64>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => {
            if let Some(n) = number.as_i64() {
                return Ok(Some(n));
            }
            match number.as_f64() {
                Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => Ok(Some(f as i64)),
                _ => Err(format!("«{name}» tiene que ser un número entero")),
            }
        }
        Some(_) => Err(format!("«{name}» tiene que ser un número entero")),
    }
}

/// Refuses arguments the tool does not know, so a misnamed one (say,
/// `en_minutos`) is an error the model sees, not something silently ignored.
pub fn known_args(args: &Map<String, Value>, known: &[&str]) -> Result<(), String> {
    match args.keys().find(|key| !known.contains(&key.as_str())) {
        Some(key) => Err(format!(
            "no conozco «{key}»; los argumentos son: {}",
            known.join(", ")
        )),
        None => Ok(()),
    }
}
