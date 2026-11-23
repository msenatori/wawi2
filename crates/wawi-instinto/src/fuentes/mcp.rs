//! `{"mcp": {"plugin": "github", "tool": "get_pull_request", "args": {...}}}`:
//! calls a READ tool of any installed Wawi plugin. It reuses the plugin
//! host's own pieces: the manifest loader, the MCP client and the secrets.
//! A tool that is not `kind = read` (by plugin.toml or by the server's
//! `readOnlyHint`) is refused, before and during the call.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use wawi_plugins::manifest::{self, Expose, Kind, Manifest, Server};
use wawi_plugins::mcp::{Client, HttpTransport, OnNotify, StdioSpec, StdioTransport, Transport};
use wawi_plugins::secrets::Secrets;
use wawi_plugins::tool::kind_of;

use super::{Ctx, TipoFuente};
use crate::extraer::as_json;
use crate::texto::truncate;

/// This service's own plugin name: it never watches itself.
pub const SELF: &str = "instinto";

pub struct Mcp;

struct Spec<'a> {
    plugin: &'a str,
    tool: &'a str,
    args: Value,
}

fn spec(config: &Value) -> Result<Spec<'_>, String> {
    let plugin = config
        .get("plugin")
        .and_then(Value::as_str)
        .ok_or("mcp necesita plugin")?;
    let tool = config
        .get("tool")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .ok_or("mcp necesita tool")?;
    let args = match config.get("args") {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return Err("mcp.args es un objeto".into()),
    };
    if plugin == SELF {
        return Err("el instinto no se mira a sí mismo".into());
    }
    Ok(Spec { plugin, tool, args })
}

fn find(ctx: &Ctx<'_>, plugin: &str) -> Result<Manifest, String> {
    let (manifests, _) = manifest::discover(ctx.home);
    let manifest = manifests
        .into_iter()
        .find(|m| m.name == plugin)
        .ok_or_else(|| format!("no hay un plugin «{plugin}» instalado"))?;
    if !manifest.enabled {
        return Err(format!("el plugin «{plugin}» está apagado"));
    }
    Ok(manifest)
}

/// What plugin.toml alone says about the tool.
fn check_rules(manifest: &Manifest, tool: &str) -> Result<(), String> {
    let rule = manifest.tools.get(tool);
    if manifest.expose == Expose::Listed && rule.is_none() {
        return Err(format!("el plugin «{}» no expone {tool}", manifest.name));
    }
    if rule.and_then(|r| r.kind) == Some(Kind::Act) {
        return Err(format!(
            "{}__{tool} cambia cosas (kind = act): solo miro herramientas de lectura",
            manifest.name
        ));
    }
    Ok(())
}

impl TipoFuente for Mcp {
    fn validar(&self, config: &Value, ctx: &Ctx<'_>) -> Result<(), String> {
        let spec = spec(config)?;
        let manifest = find(ctx, spec.plugin)?;
        check_rules(&manifest, spec.tool)
    }

    fn leer(&self, config: &Value, ctx: &Ctx<'_>) -> Result<Value, String> {
        let spec = spec(config)?;
        let manifest = find(ctx, spec.plugin)?;
        check_rules(&manifest, spec.tool)?;
        let (secrets, _) = Secrets::load(ctx.home);
        let timeout = manifest.timeout.min(ctx.timeout);
        call(&manifest, &secrets, spec.tool, spec.args, timeout)
            .map_err(|e| truncate(&secrets.scrub(&e), 300))
    }
}

fn call(
    manifest: &Manifest,
    secrets: &Secrets,
    tool: &str,
    args: Value,
    timeout: Duration,
) -> Result<Value, String> {
    let transport = transport(manifest, secrets)?;
    let mut client = Client::connect(transport, timeout).map_err(|e| e.to_string())?;
    let listed = client.list_tools(timeout).map_err(|e| e.to_string())?;
    let remote = listed
        .iter()
        .find(|t| t.name == tool)
        .ok_or_else(|| format!("el plugin «{}» no tiene {tool}", manifest.name))?;
    if kind_of(manifest, remote) != Kind::Read {
        return Err(format!(
            "{}__{tool} no es de lectura: solo miro herramientas de lectura",
            manifest.name
        ));
    }
    let result = client
        .call(tool, args, timeout)
        .map_err(|e| e.to_string())?;
    if result.is_error {
        return Err(format!(
            "{}__{tool} dio error: {}",
            manifest.name,
            truncate(&result.text, 200)
        ));
    }
    Ok(as_json(&result.text))
}

/// The same connection the plugin host makes, without its notifications.
fn transport(manifest: &Manifest, secrets: &Secrets) -> Result<Box<dyn Transport>, String> {
    let on_notify: OnNotify = Arc::new(|_: &str, _: &Value| {});
    let resolve = |text: &str| secrets.resolve(text);
    match &manifest.server {
        Server::Stdio { command, args, env } => {
            let args = args
                .iter()
                .map(|a| resolve(a))
                .collect::<Result<Vec<_>, _>>()?;
            let env = env
                .iter()
                .map(|(k, v)| Ok((k.clone(), resolve(v)?)))
                .collect::<Result<Vec<_>, String>>()?;
            let transport = StdioTransport::spawn(
                StdioSpec {
                    command,
                    args: &args,
                    env: &env,
                    dir: &manifest.dir,
                },
                on_notify,
                Box::new(|_| {}),
                Box::new(|| {}),
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
