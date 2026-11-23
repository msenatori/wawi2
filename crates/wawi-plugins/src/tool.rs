//! A plugin's tool as the core sees it: an ordinary `wawi_core::Tool` in the
//! registry, the same trait the built-in tools implement. Starting it hands
//! the call to the plugin's worker thread and returns at once; the result
//! comes back as `ToolFinished` through the sink.

use std::sync::Arc;
use std::sync::mpsc::Sender;

use serde_json::Value;
use wawi_core::{Event, PermissionLevel, Tool, ToolCall, ToolOutcome, ToolSpec, TurnId};

use crate::PluginSink;
use crate::manifest::{Expose, FEED_TOOL, Kind, Manifest, truncate};
use crate::mcp::RemoteTool;
use crate::worker::{Job, Origin};

/// The longest tool description the brain sees.
const MAX_DESCRIPTION: usize = 1500;

/// A tool spec for the core and the name the server knows it by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginToolSpec {
    pub spec: ToolSpec,
    pub remote: String,
}

/// `<plugin>__<tool>`, with anything MCP would refuse turned into `_`, at
/// most 64 characters.
pub fn exposed_name(plugin: &str, tool: &str) -> String {
    let name: String = format!("{plugin}__{tool}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    name.chars().take(64).collect()
}

/// The kind of a tool: what plugin.toml says, else the server's
/// `readOnlyHint`, else `act`, the careful default.
pub fn kind_of(manifest: &Manifest, tool: &RemoteTool) -> Kind {
    manifest
        .tools
        .get(&tool.name)
        .and_then(|rule| rule.kind)
        .unwrap_or(if tool.read_only_hint == Some(true) {
            Kind::Read
        } else {
            Kind::Act
        })
}

/// The tools of `manifest` the brain may be offered, from what the server
/// listed. The feed tool is never offered.
pub fn specs_for(manifest: &Manifest, listed: &[RemoteTool]) -> Vec<PluginToolSpec> {
    listed
        .iter()
        .filter(|tool| tool.name != FEED_TOOL)
        .filter(|tool| manifest.expose == Expose::All || manifest.tools.contains_key(&tool.name))
        .map(|tool| {
            let rule = manifest.tools.get(&tool.name).cloned().unwrap_or_default();
            let kind = kind_of(manifest, tool);
            let description = rule
                .description
                .clone()
                .unwrap_or_else(|| tool.description.clone());
            PluginToolSpec {
                spec: ToolSpec {
                    name: exposed_name(&manifest.name, &tool.name),
                    description: truncate(description.trim(), MAX_DESCRIPTION),
                    params_schema_json: tool.input_schema.to_string(),
                    permission: match kind {
                        Kind::Read => PermissionLevel::Read,
                        Kind::Act => PermissionLevel::Write,
                    },
                    needs_confirmation: rule.confirm.unwrap_or(kind == Kind::Act),
                    ack: None,
                },
                remote: tool.name.clone(),
            }
        })
        .collect()
}

pub struct PluginTool {
    spec: ToolSpec,
    remote: String,
    jobs: Sender<Job>,
    sink: Arc<dyn PluginSink>,
}

impl PluginTool {
    pub(crate) fn new(spec: PluginToolSpec, jobs: Sender<Job>, sink: Arc<dyn PluginSink>) -> Self {
        Self {
            spec: spec.spec,
            remote: spec.remote,
            jobs,
            sink,
        }
    }
}

impl Tool for PluginTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn start(&mut self, turn: TurnId, call: &ToolCall) {
        // The core already checked it is a JSON object.
        let args = serde_json::from_str::<Value>(&call.args_json)
            .unwrap_or_else(|_| Value::Object(Default::default()));
        let job = Job::Call {
            origin: Origin::Brain {
                turn,
                call_id: call.id.clone(),
            },
            tool: self.remote.clone(),
            args,
        };
        if self.jobs.send(job).is_err() {
            self.sink.core(Event::ToolFinished {
                turn,
                call_id: call.id.clone(),
                outcome: ToolOutcome::Failure("el plugin está cerrado".into()),
            });
        }
    }

    /// The call keeps running in the plugin; its late result names a turn
    /// that is over, so the core drops it.
    fn cancel(&mut self, _turn: TurnId, _call_id: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::parse;
    use serde_json::json;

    fn remote(name: &str, read_only: Option<bool>) -> RemoteTool {
        RemoteTool {
            name: name.into(),
            description: format!("{name} desc"),
            input_schema: json!({"type": "object"}),
            read_only_hint: read_only,
        }
    }

    #[test]
    fn kinds_come_from_the_manifest_then_the_hint_then_act() {
        let manifest = parse(
            "[server]\ncommand='x'\n[tools.listado]\nkind='act'\n[tools.leer_seguro]\nkind='read'\nconfirm=true\n[tools.crear]\nkind='act'\nconfirm=false\ndescription='crea algo'\n",
            std::path::Path::new("/x/demo"),
        )
        .unwrap();
        let specs = specs_for(
            &manifest,
            &[
                remote("listado", Some(true)),
                remote("hint", Some(true)),
                remote("nada", None),
                remote("leer_seguro", None),
                remote("crear", None),
                remote(FEED_TOOL, Some(true)),
            ],
        );
        let by = |name: &str| {
            specs
                .iter()
                .find(|s| s.remote == name)
                .unwrap()
                .spec
                .clone()
        };
        assert_eq!(by("listado").permission, PermissionLevel::Write);
        assert!(by("listado").needs_confirmation);
        assert_eq!(by("hint").permission, PermissionLevel::Read);
        assert!(!by("hint").needs_confirmation);
        assert_eq!(by("nada").permission, PermissionLevel::Write);
        assert!(by("nada").needs_confirmation);
        assert!(by("leer_seguro").needs_confirmation);
        assert!(!by("crear").needs_confirmation);
        assert_eq!(by("crear").description, "crea algo");
        assert_eq!(by("crear").name, "demo__crear");
        assert!(specs.iter().all(|s| s.remote != FEED_TOOL));
    }

    #[test]
    fn listed_exposes_only_the_named_tools() {
        let manifest = parse(
            "expose='listed'\n[server]\ncommand='x'\n[tools.a]\nkind='read'\n",
            std::path::Path::new("/x/demo"),
        )
        .unwrap();
        let specs = specs_for(&manifest, &[remote("a", None), remote("b", Some(true))]);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].spec.name, "demo__a");
    }

    #[test]
    fn exposed_names_are_safe_for_mcp() {
        assert_eq!(exposed_name("linear", "list_issues"), "linear__list_issues");
        assert_eq!(exposed_name("x", "a.b c"), "x__a_b_c");
        assert_eq!(exposed_name("x", &"y".repeat(100)).len(), 64);
    }
}
