//! The stream-json protocol of `claude -p`, as far as Wawi needs it.
//!
//! Every line on stdin and stdout is one JSON object. Besides conversation
//! messages, both sides send `control_request`s and answer them with
//! `control_response`s; that is how the CLI reaches the in-process MCP server
//! (`mcp_message`) and how the host interrupts a turn (`interrupt`).

use serde_json::{Value, json};
use wawi_core::ToolSpec;

use crate::limit::{self, Limit};

/// Name of the in-process MCP server. Claude sees its tools as
/// `mcp__wawi__<tool>`.
pub const SERVER_NAME: &str = "wawi";

/// Value for `--mcp-config`: one SDK server, served over the control protocol
/// by this process instead of by a separate one.
pub fn mcp_config() -> String {
    json!({ "mcpServers": { SERVER_NAME: { "type": "sdk", "name": SERVER_NAME } } }).to_string()
}

pub fn user_message(text: &str) -> Value {
    json!({
        "type": "user",
        "message": { "role": "user", "content": text },
        "parent_tool_use_id": null,
        "session_id": "",
    })
}

pub fn control_request(request_id: &str, request: Value) -> Value {
    json!({ "type": "control_request", "request_id": request_id, "request": request })
}

pub fn initialize(request_id: &str) -> Value {
    control_request(
        request_id,
        json!({ "subtype": "initialize", "hooks": null }),
    )
}

pub fn interrupt(request_id: &str) -> Value {
    control_request(request_id, json!({ "subtype": "interrupt" }))
}

pub fn control_success(request_id: &str, response: Value) -> Value {
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": response },
    })
}

pub fn control_error(request_id: &str, error: &str) -> Value {
    json!({
        "type": "control_response",
        "response": { "subtype": "error", "request_id": request_id, "error": error },
    })
}

/// A control response carrying a JSON-RPC reply from the MCP server.
pub fn mcp_reply(request_id: &str, reply: Value) -> Value {
    control_success(request_id, json!({ "mcp_response": reply }))
}

pub fn rpc_result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A JSON-RPC notification needs no reply, but the control request that
/// carried it still expects an acknowledgement.
pub fn rpc_ack() -> Value {
    json!({ "jsonrpc": "2.0", "result": {} })
}

pub fn mcp_initialize_result(params: &Value) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("2025-06-18");
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
    })
}

pub fn tools_list_result(tools: &[ToolSpec]) -> Value {
    let tools: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": input_schema(&tool.params_schema_json),
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// MCP requires an object schema. A spec whose schema is not one (the core
/// does not validate it) is offered as "any object" instead of breaking the
/// whole tool list.
fn input_schema(schema_json: &str) -> Value {
    match serde_json::from_str::<Value>(schema_json) {
        Ok(schema @ Value::Object(_)) => schema,
        _ => json!({ "type": "object" }),
    }
}

pub fn tool_call_result(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// What one stdout line means to the adapter. Anything else is ignored.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    /// The CLI asks something of the host.
    ControlRequest {
        request_id: String,
        request: Value,
    },
    /// The CLI gave up on a control request it sent earlier.
    ControlCancel {
        request_id: String,
    },
    /// An answer to a request the host sent (initialize, interrupt).
    ControlResponse {
        request_id: String,
        error: Option<String>,
    },
    SessionStarted {
        session_id: String,
    },
    /// The CLI compacted the conversation: older messages became a summary.
    Compacted,
    TextDelta(String),
    BlockStop,
    /// The turn is over, by success, error or interrupt. `limit` when it
    /// ended on the subscription's usage limit.
    Result {
        is_error: bool,
        reason: String,
        limit: Option<Limit>,
    },
    /// An assistant message that wraps the usage-limit refusal.
    Limit(Limit),
    /// A `rate_limit_event` saying requests are refused until this Unix
    /// second.
    RefusedUntil(u64),
    Other,
}

pub fn parse_line(line: &str) -> Option<Incoming> {
    let value: Value = serde_json::from_str(line).ok()?;
    let kind = value.get("type").and_then(Value::as_str)?;
    // Output of subagents (none are offered, but just in case) is not Wawi's.
    if value
        .get("parent_tool_use_id")
        .is_some_and(|parent| !parent.is_null())
    {
        return Some(Incoming::Other);
    }
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(match kind {
        "control_request" => Incoming::ControlRequest {
            request_id: text(&value, "request_id")?,
            request: value.get("request")?.clone(),
        },
        "control_cancel_request" => Incoming::ControlCancel {
            request_id: text(&value, "request_id")?,
        },
        "control_response" => {
            let response = value.get("response")?;
            Incoming::ControlResponse {
                request_id: text(response, "request_id").unwrap_or_default(),
                error: (text(response, "subtype").as_deref() == Some("error"))
                    .then(|| text(response, "error").unwrap_or_default()),
            }
        }
        "system" if text(&value, "subtype").as_deref() == Some("init") => {
            Incoming::SessionStarted {
                session_id: text(&value, "session_id")?,
            }
        }
        "system" if text(&value, "subtype").as_deref() == Some("compact_boundary") => {
            Incoming::Compacted
        }
        "stream_event" => {
            let event = value.get("event")?;
            match event.get("type").and_then(Value::as_str) {
                Some("content_block_delta") => {
                    let delta = event.get("delta")?;
                    match delta.get("type").and_then(Value::as_str) {
                        Some("text_delta") => Incoming::TextDelta(text(delta, "text")?),
                        _ => Incoming::Other,
                    }
                }
                Some("content_block_stop") => Incoming::BlockStop,
                _ => Incoming::Other,
            }
        }
        "assistant" => limit::from_value(&value).map_or(Incoming::Other, Incoming::Limit),
        "rate_limit_event" => {
            limit::refused_until(&value).map_or(Incoming::Other, Incoming::RefusedUntil)
        }
        "result" => {
            let is_error = value
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let reason = value
                .get("errors")
                .and_then(Value::as_array)
                .and_then(|errors| errors.first())
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| text(&value, "result"))
                .or_else(|| text(&value, "subtype"))
                .unwrap_or_default();
            Incoming::Result {
                is_error,
                reason,
                limit: limit::from_value(&value),
            }
        }
        _ => Incoming::Other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_lines_that_matter() {
        assert_eq!(
            parse_line(
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Ho"}},"parent_tool_use_id":null}"#
            ),
            Some(Incoming::TextDelta("Ho".into()))
        );
        assert_eq!(
            parse_line(r#"{"type":"system","subtype":"init","session_id":"abc"}"#),
            Some(Incoming::SessionStarted {
                session_id: "abc".into()
            })
        );
        assert_eq!(
            parse_line(
                r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["boom"]}"#
            ),
            Some(Incoming::Result {
                is_error: true,
                reason: "boom".into(),
                limit: None,
            })
        );
        assert_eq!(
            parse_line(
                r#"{"type":"result","subtype":"success","is_error":true,"api_error":"usage_limit_reached","result":"You've hit your limit · resets 6pm"}"#
            ),
            Some(Incoming::Result {
                is_error: true,
                reason: "You've hit your limit · resets 6pm".into(),
                limit: Some(Limit { resets_at: None }),
            })
        );
        assert_eq!(
            parse_line(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"You've hit your limit"}]},"error":"rate_limit","api_error":"usage_limit_reached","api_error_params":{"rate_limit_info":{"status":"rejected","resetsAt":1764007200}},"parent_tool_use_id":null}"#
            ),
            Some(Incoming::Limit(Limit {
                resets_at: Some(1_764_007_200)
            }))
        );
        assert_eq!(
            parse_line(
                r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1764007200}}"#
            ),
            Some(Incoming::RefusedUntil(1_764_007_200))
        );
        assert_eq!(
            parse_line(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hola"}]}}"#
            ),
            Some(Incoming::Other)
        );
        assert_eq!(
            parse_line(
                r#"{"type":"control_response","response":{"subtype":"error","request_id":"r1","error":"no"}}"#
            ),
            Some(Incoming::ControlResponse {
                request_id: "r1".into(),
                error: Some("no".into())
            })
        );
        assert_eq!(
            parse_line(
                r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"x"}},"parent_tool_use_id":"toolu_1"}"#
            ),
            Some(Incoming::Other)
        );
        assert_eq!(
            parse_line(r#"{"type":"system","subtype":"compact_boundary","compact_metadata":{}}"#),
            Some(Incoming::Compacted)
        );
        assert_eq!(parse_line("not json"), None);
    }

    #[test]
    fn a_broken_schema_becomes_any_object() {
        assert_eq!(input_schema("[1]"), json!({ "type": "object" }));
        assert_eq!(
            input_schema(r#"{"type":"object","properties":{}}"#),
            json!({ "type": "object", "properties": {} })
        );
    }
}
