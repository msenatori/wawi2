//! What a turn may do, decided in code from where the turn came from. Nothing
//! here asks the model: a tool call the policy refuses never becomes a
//! `RunTool`, and the refusal goes back to the brain as a tool failure.

use crate::json::{self, Json};
use crate::registry::{PermissionLevel, ToolSpec};

/// Who started a turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TurnSource {
    /// The owner, by voice or typing, or by accepting a suggestion.
    #[default]
    Owner,
    /// Wawi on its own: a scheduled or triggered proactive run.
    Initiative,
}

/// Sent to the brain when an initiative turn asks for more than reading.
pub const INITIATIVE_READ_ONLY: &str = "la iniciativa solo puede leer; proponelo como sugerencia";

/// Sent to the brain when an initiative turn asks for a tool that needs the
/// owner's yes: the owner is not in that conversation to give it.
pub const INITIATIVE_NO_CONFIRMATION: &str =
    "la iniciativa no puede pedir confirmación; proponelo como sugerencia";

/// Whether a turn from `source` may use the tool at all. Owner turns may use
/// any tool; `needs_confirmation` is still enforced by the dialogue for them.
pub fn check_tool(source: TurnSource, spec: &ToolSpec) -> Result<(), &'static str> {
    match source {
        TurnSource::Owner => Ok(()),
        TurnSource::Initiative if spec.permission != PermissionLevel::Read => {
            Err(INITIATIVE_READ_ONLY)
        }
        TurnSource::Initiative if spec.needs_confirmation => Err(INITIATIVE_NO_CONFIRMATION),
        TurnSource::Initiative => Ok(()),
    }
}

/// Tool arguments must be a JSON object. The schema itself is not checked.
pub fn parse_args(args_json: &str) -> Result<Json, String> {
    match json::parse(args_json) {
        Ok(value @ Json::Object(_)) => Ok(value),
        Ok(_) => Err("invalid arguments: not a JSON object".into()),
        Err(error) => Err(format!("invalid arguments: {error}")),
    }
}

/// Whether a tool may appear in the context of a turn from `source`, so the
/// brain is not offered what the policy would refuse anyway.
pub fn offered(source: TurnSource, spec: &ToolSpec) -> bool {
    check_tool(source, spec).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(permission: PermissionLevel, needs_confirmation: bool) -> ToolSpec {
        ToolSpec {
            name: "t".into(),
            description: String::new(),
            params_schema_json: "{}".into(),
            permission,
            needs_confirmation,
            ack: None,
        }
    }

    #[test]
    fn the_owner_may_use_any_level() {
        for level in [
            PermissionLevel::Read,
            PermissionLevel::Write,
            PermissionLevel::System,
        ] {
            assert_eq!(check_tool(TurnSource::Owner, &spec(level, true)), Ok(()));
        }
    }

    #[test]
    fn initiative_may_only_read_without_confirmation() {
        let initiative = TurnSource::Initiative;
        assert_eq!(
            check_tool(initiative, &spec(PermissionLevel::Read, false)),
            Ok(())
        );
        assert_eq!(
            check_tool(initiative, &spec(PermissionLevel::Write, false)),
            Err(INITIATIVE_READ_ONLY)
        );
        assert_eq!(
            check_tool(initiative, &spec(PermissionLevel::System, false)),
            Err(INITIATIVE_READ_ONLY)
        );
        assert_eq!(
            check_tool(initiative, &spec(PermissionLevel::Read, true)),
            Err(INITIATIVE_NO_CONFIRMATION)
        );
    }

    #[test]
    fn arguments_must_be_a_json_object() {
        assert!(parse_args(r#"{"a": 1}"#).is_ok());
        assert!(parse_args(" {} ").is_ok());
        assert!(parse_args("[]").unwrap_err().contains("not a JSON object"));
        assert!(parse_args("{bad").is_err());
        assert!(parse_args("").is_err());
    }
}
