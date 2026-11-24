//! How the `claude` CLI says that the owner's subscription refused a request
//! for its usage limit, and when it resets. Checked against Claude Code
//! 2.1.331 (its bundled SDK schema and its own handling of the error):
//!
//! - **The refused request** becomes an assistant message that wraps the API
//!   error: `"error": "rate_limit"`, `"is_api_error_message": true`,
//!   `"api_error": "usage_limit_reached"` and
//!   `"api_error_params": {"rate_limit_info": {"status": "rejected",
//!   "resetsAt": <Unix seconds>, "rateLimitType": "five_hour" | "seven_day"
//!   | …}}`. Its text is the line the terminal shows, like «You've hit your
//!   limit · resets 6pm (America/Argentina/Buenos_Aires)». `usage_limit_reached`
//!   is documented as «the request was refused by the account's own usage
//!   limit … api_error_params.rate_limit_info says which and when it
//!   resets»; other refusals that share `error: "rate_limit"` (a server
//!   throttle, an org policy) never carry it.
//! - **The turn's `result`** repeats it: `"is_error": true` and
//!   `"api_error": "usage_limit_reached"` (with `subtype: "success"`), and
//!   `result` is the same line.
//! - **`rate_limit_event`** lines (`{"type": "rate_limit_event",
//!   "rate_limit_info": {...}}`) come when the limit state changes, for
//!   subscribers only. A top-level `status: "rejected"` means requests are
//!   refused, unless `overageStatus` is `allowed` or `allowed_warning`
//!   (paid extra usage covers it). Its `resetsAt` is a fallback for the
//!   reset time; it never starts the limit on its own.
//! - **Older CLIs** said it only in the text: «Claude AI usage limit
//!   reached|<Unix seconds>» or «5-hour limit reached ∙ resets 3pm». The
//!   text check below is the fallback for those.

use serde_json::Value;

/// A request refused for the usage limit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limit {
    /// When it resets, in Unix seconds, if the CLI said.
    pub resets_at: Option<u64>,
}

/// The usage limit in an assistant message or a `result`, if that is what
/// it says. Anything else (a server throttle, an outage) is `None`.
pub fn from_value(value: &Value) -> Option<Limit> {
    let kind = value.get("type").and_then(Value::as_str)?;
    if !matches!(kind, "assistant" | "result") {
        return None;
    }
    let api_error = value.get("api_error").and_then(Value::as_str);
    if api_error == Some("usage_limit_reached") {
        let resets_at = value
            .get("api_error_params")
            .and_then(|p| p.get("rate_limit_info"))
            .and_then(resets_at);
        return Some(Limit { resets_at });
    }
    // The fallback: only an error, and only its words.
    let text = match kind {
        "result" if value.get("is_error").and_then(Value::as_bool) == Some(true) => {
            value.get("result").and_then(Value::as_str)
        }
        "assistant" if value.get("error").is_some_and(|e| !e.is_null()) => first_text(value),
        _ => None,
    }?;
    text_says_limit(text).then(|| Limit {
        resets_at: legacy_reset(text),
    })
}

/// From a `rate_limit_event`: the reset time when requests are refused now.
pub fn refused_until(value: &Value) -> Option<u64> {
    if value.get("type").and_then(Value::as_str) != Some("rate_limit_event") {
        return None;
    }
    let info = value.get("rate_limit_info")?;
    let status = info.get("status").and_then(Value::as_str);
    let overage = info.get("overageStatus").and_then(Value::as_str);
    let covered = matches!(overage, Some("allowed" | "allowed_warning"));
    (status == Some("rejected") && !covered)
        .then(|| resets_at(info))
        .flatten()
}

fn resets_at(info: &Value) -> Option<u64> {
    info.get("resetsAt")
        .and_then(Value::as_u64)
        .filter(|at| *at > 0)
}

fn first_text(value: &Value) -> Option<&str> {
    value
        .get("message")?
        .get("content")?
        .as_array()?
        .iter()
        .find_map(|block| block.get("text").and_then(Value::as_str))
}

/// Whether an error text is the usage limit, not a passing throttle.
pub fn text_says_limit(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "usage limit",
        "hit your limit",
        "hit your session limit",
        "hit your weekly limit",
        "limit reached",
        "out of usage",
        "out of extra usage",
    ]
    .iter()
    .any(|words| text.contains(words))
}

/// «Claude AI usage limit reached|1764000000»: the Unix second after the bar.
fn legacy_reset(text: &str) -> Option<u64> {
    let (_, after) = text.rsplit_once('|')?;
    let digits: String = after
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|at| *at > 1_000_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_typed_error_carries_the_reset() {
        let assistant = json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": "You've hit your limit · resets 6pm" }] },
            "error": "rate_limit",
            "is_api_error_message": true,
            "api_error": "usage_limit_reached",
            "api_error_params": { "rate_limit_info": {
                "status": "rejected", "resetsAt": 1_764_007_200u64, "rateLimitType": "five_hour"
            } },
        });
        assert_eq!(
            from_value(&assistant),
            Some(Limit {
                resets_at: Some(1_764_007_200)
            })
        );
        let result = json!({
            "type": "result", "subtype": "success", "is_error": true,
            "api_error": "usage_limit_reached", "result": "You've hit your limit · resets 6pm",
        });
        assert_eq!(from_value(&result), Some(Limit { resets_at: None }));
    }

    #[test]
    fn a_passing_throttle_or_an_outage_is_not_the_limit() {
        let throttle = json!({
            "type": "assistant", "error": "rate_limit",
            "message": { "content": [{ "type": "text", "text": "API Error: rate limited, try again" }] },
        });
        assert_eq!(from_value(&throttle), None);
        let outage =
            json!({ "type": "result", "is_error": true, "result": "API Error: 529 overloaded" });
        assert_eq!(from_value(&outage), None);
        let ok = json!({ "type": "result", "is_error": false, "result": "llegaste al límite de velocidad, usage limit" });
        assert_eq!(from_value(&ok), None, "a normal answer that mentions it");
        let words = json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "usage limit" }] } });
        assert_eq!(from_value(&words), None, "no error, no limit");
    }

    #[test]
    fn older_clis_said_it_in_words() {
        let legacy = json!({
            "type": "result", "is_error": true,
            "result": "Claude AI usage limit reached|1764007200",
        });
        assert_eq!(
            from_value(&legacy),
            Some(Limit {
                resets_at: Some(1_764_007_200)
            })
        );
        let words = json!({ "type": "result", "is_error": true, "result": "5-hour limit reached ∙ resets 3pm" });
        assert_eq!(from_value(&words), Some(Limit { resets_at: None }));
    }

    #[test]
    fn a_rejected_rate_limit_event_gives_the_reset_unless_overage_covers_it() {
        let event = |status: &str, overage: Option<&str>| {
            let mut info = json!({ "status": status, "resetsAt": 1_764_007_200u64 });
            if let Some(overage) = overage {
                info["overageStatus"] = json!(overage);
            }
            json!({ "type": "rate_limit_event", "rate_limit_info": info, "uuid": "u", "session_id": "s" })
        };
        assert_eq!(refused_until(&event("rejected", None)), Some(1_764_007_200));
        assert_eq!(
            refused_until(&event("rejected", Some("rejected"))),
            Some(1_764_007_200)
        );
        assert_eq!(refused_until(&event("rejected", Some("allowed"))), None);
        assert_eq!(refused_until(&event("allowed_warning", None)), None);
    }
}
