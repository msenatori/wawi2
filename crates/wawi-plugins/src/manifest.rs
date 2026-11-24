//! `<home>/plugins/<name>/plugin.toml` and `instrucciones.md`: what a plugin
//! is, how to reach it, what its tools may do and what it watches. The full
//! reference is docs/PLUGINS.md.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use wawi_core::Priority;

use crate::schedule::{At, When, parse_at, parse_duration};

/// The tool name a plugin exposes to receive the turn feed. Never offered to
/// the brain.
pub const FEED_TOOL: &str = "observar_charla";
/// How long a call may take unless the plugin says otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);
/// The longest `instrucciones.md` that reaches the brain.
pub const MAX_INSTRUCTIONS: usize = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Only reads: the initiative may use it.
    Read,
    /// Changes something: never in an initiative turn, and asks the owner
    /// first unless `confirm = false`.
    Act,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRule {
    pub kind: Option<Kind>,
    pub confirm: Option<bool>,
    /// Replaces the server's description. The confirmation question uses it,
    /// so a short Spanish sentence works best for `act` tools.
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Expose {
    /// Every tool the server lists.
    #[default]
    All,
    /// Only the tools named under `[tools.*]`.
    Listed,
}

/// How to reach the server. Values may hold `${SECRET}` references; they
/// are resolved only when connecting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Server {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

/// How Wawi gets the server's permission (`auth` in plugin.toml).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Auth {
    /// Nothing, or whatever `${SECRET}` references in the manifest say.
    #[default]
    None,
    /// OAuth 2.1 (MCP authorization): «Conectar» opens the browser. Only for
    /// `url` servers. `client_id` for servers without dynamic registration.
    OAuth {
        client_id: Option<String>,
        scopes: Option<Vec<String>>,
    },
    /// An API key the owner pastes once in a secure dialog. It is kept in
    /// the Keychain under the account `name`, and `${name}` in the manifest
    /// resolves to it. A `url` server with no header that uses it sends it
    /// as `Authorization: Bearer`.
    Key { name: String, help: Option<String> },
}

impl Auth {
    pub fn label(&self) -> &'static str {
        match self {
            Auth::None => "ninguna",
            Auth::OAuth { .. } => "oauth",
            Auth::Key { .. } => "clave",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Watch {
    pub name: String,
    pub when: When,
    pub tool: String,
    pub args: Value,
    pub say: String,
    pub cooldown: Option<Duration>,
    pub priority: Priority,
    /// Extra texts that mean "nothing" for this watch.
    pub empty: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Routine {
    pub name: String,
    pub at: At,
    pub prompt: String,
    pub cooldown: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub dir: PathBuf,
    pub description: String,
    pub enabled: bool,
    pub instructions: Option<String>,
    pub timeout: Duration,
    pub expose: Expose,
    /// Gets every finished turn through `observar_charla`. Off by default.
    pub observe_chat: bool,
    pub server: Server,
    pub auth: Auth,
    pub tools: BTreeMap<String, ToolRule>,
    pub watches: Vec<Watch>,
    pub routines: Vec<Routine>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServer {
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWatch {
    name: Option<String>,
    every: Option<String>,
    at: Option<String>,
    tool: String,
    #[serde(default)]
    args: Option<Value>,
    say: String,
    cooldown: Option<String>,
    priority: Option<String>,
    #[serde(default)]
    empty: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoutine {
    name: Option<String>,
    at: String,
    prompt: String,
    cooldown: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default = "yes")]
    enabled: bool,
    timeout: Option<String>,
    #[serde(default)]
    expose: Expose,
    #[serde(default)]
    observa_charla: bool,
    server: RawServer,
    auth: Option<toml::Value>,
    #[serde(default)]
    tools: BTreeMap<String, ToolRule>,
    #[serde(default)]
    watch: Vec<RawWatch>,
    #[serde(default)]
    routine: Vec<RawRoutine>,
}

fn yes() -> bool {
    true
}

/// Lowercase letters, digits, `-` and `_`, starting with a letter or digit,
/// up to 24, and no `__` (it separates plugin and tool in the names the
/// brain sees).
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    first_ok
        && name.len() <= 24
        && !name.contains("__")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Reads `dir/plugin.toml` (and `dir/instrucciones.md`, if there is one).
pub fn load(dir: &Path) -> Result<Manifest, String> {
    let folder = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path = dir.join("plugin.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("{folder}: no pude leer plugin.toml: {e}"))?;
    parse(&text, dir).map_err(|e| format!("{folder}: {e}"))
}

pub fn parse(text: &str, dir: &Path) -> Result<Manifest, String> {
    let raw: Raw = toml::from_str(text).map_err(|e| format!("plugin.toml inválido: {e}"))?;
    let folder = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = raw.name.unwrap_or(folder);
    if !valid_name(&name) {
        return Err(format!(
            "nombre «{name}» inválido: minúsculas, números, - y _, hasta 24, sin «__»"
        ));
    }
    let server = match (raw.server.command, raw.server.url) {
        (Some(command), None) => {
            if !raw.server.headers.is_empty() {
                return Err("[server] headers es solo para url".into());
            }
            Server::Stdio {
                command,
                args: raw.server.args,
                env: raw.server.env,
            }
        }
        (None, Some(url)) => {
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err(format!(
                    "[server] url tiene que empezar con https://: {url}"
                ));
            }
            if !raw.server.args.is_empty() || !raw.server.env.is_empty() {
                return Err("[server] args y env son solo para command".into());
            }
            Server::Http {
                url,
                headers: raw.server.headers,
            }
        }
        _ => return Err("[server] necesita command (stdio) o url (http), uno solo".into()),
    };
    let auth = parse_auth(raw.auth.as_ref())?;
    if matches!(auth, Auth::OAuth { .. }) && !matches!(server, Server::Http { .. }) {
        return Err("auth = \"oauth\" es solo para servidores con url".into());
    }
    let timeout = match raw.timeout {
        Some(text) => parse_duration(&text)?,
        None => DEFAULT_TIMEOUT,
    };
    let optional_duration = |text: Option<String>| text.map(|t| parse_duration(&t)).transpose();
    let mut watches = Vec::new();
    for (index, watch) in raw.watch.into_iter().enumerate() {
        let name = watch.name.unwrap_or_else(|| format!("watch{}", index + 1));
        let when = match (watch.every, watch.at) {
            (Some(every), None) => {
                let every = parse_duration(&every)?;
                if every < Duration::from_secs(60) {
                    return Err(format!("watch {name}: every tiene que ser de 1m o más"));
                }
                When::Every(every)
            }
            (None, Some(at)) => When::At(parse_at(&at)?),
            _ => return Err(format!("watch {name}: necesita every o at, uno solo")),
        };
        let priority = match watch.priority.as_deref().map(str::to_lowercase).as_deref() {
            None | Some("alta") | Some("high") => Priority::High,
            Some("baja") | Some("low") => Priority::Low,
            Some(other) => return Err(format!("watch {name}: priority «{other}» (alta o baja)")),
        };
        let args = match watch.args {
            None => Value::Object(Default::default()),
            Some(value @ Value::Object(_)) => value,
            Some(_) => return Err(format!("watch {name}: args tiene que ser una tabla")),
        };
        if watch.say.trim().is_empty() {
            return Err(format!("watch {name}: say está vacío"));
        }
        watches.push(Watch {
            name,
            when,
            tool: watch.tool,
            args,
            say: watch.say,
            cooldown: optional_duration(watch.cooldown)?,
            priority,
            empty: watch.empty,
        });
    }
    let mut routines = Vec::new();
    for (index, routine) in raw.routine.into_iter().enumerate() {
        let name = routine
            .name
            .unwrap_or_else(|| format!("routine{}", index + 1));
        if routine.prompt.trim().is_empty() {
            return Err(format!("routine {name}: prompt está vacío"));
        }
        routines.push(Routine {
            at: parse_at(&routine.at)?,
            prompt: routine.prompt,
            cooldown: optional_duration(routine.cooldown)?,
            name,
        });
    }
    let mut seen = Vec::new();
    for name in watches
        .iter()
        .map(|w| &w.name)
        .chain(routines.iter().map(|r| &r.name))
    {
        if seen.contains(&name) {
            return Err(format!("hay dos watch o routine llamados «{name}»"));
        }
        seen.push(name);
    }
    let instructions = std::fs::read_to_string(dir.join("instrucciones.md"))
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .map(|text| truncate(&text, MAX_INSTRUCTIONS));
    Ok(Manifest {
        name,
        dir: dir.to_path_buf(),
        description: raw.description,
        enabled: raw.enabled,
        instructions,
        timeout,
        expose: raw.expose,
        observe_chat: raw.observa_charla,
        server,
        auth,
        tools: raw.tools,
        watches,
        routines,
    })
}

/// `auth = "oauth"`, `auth = "ninguna"`, `auth = { oauth = true,
/// client_id = "...", scopes = [...] }` or `auth = { clave = "NOMBRE",
/// ayuda = "https://..." }`.
fn parse_auth(value: Option<&toml::Value>) -> Result<Auth, String> {
    use toml::Value as T;
    let bad = || {
        "auth tiene que ser \"oauth\", \"ninguna\" o una tabla con clave (y ayuda) u oauth = true"
            .to_owned()
    };
    match value {
        None => Ok(Auth::None),
        Some(T::String(text)) => match text.as_str() {
            "oauth" => Ok(Auth::OAuth {
                client_id: None,
                scopes: None,
            }),
            "ninguna" | "none" => Ok(Auth::None),
            _ => Err(bad()),
        },
        Some(T::Table(table)) => {
            for key in table.keys() {
                if !["clave", "ayuda", "oauth", "client_id", "scopes"].contains(&key.as_str()) {
                    return Err(format!("auth: no conozco «{key}»"));
                }
            }
            let text = |key: &str| table.get(key).and_then(T::as_str).map(str::to_owned);
            if let Some(name) = text("clave") {
                if table.contains_key("oauth") {
                    return Err("auth: clave u oauth, uno solo".into());
                }
                let valid = !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
                if !valid {
                    return Err(format!(
                        "auth: la clave «{name}» va en MAYÚSCULAS, números y _"
                    ));
                }
                if let Some(help) = text("ayuda")
                    && !help.starts_with("https://")
                {
                    return Err("auth: ayuda tiene que ser una dirección https://".into());
                }
                return Ok(Auth::Key {
                    name,
                    help: text("ayuda"),
                });
            }
            if table.get("oauth").and_then(T::as_bool) == Some(true) {
                let scopes = match table.get("scopes") {
                    None => None,
                    Some(T::Array(items)) => Some(
                        items
                            .iter()
                            .map(|i| i.as_str().map(str::to_owned).ok_or_else(bad))
                            .collect::<Result<Vec<_>, _>>()?,
                    ),
                    Some(_) => return Err(bad()),
                };
                return Ok(Auth::OAuth {
                    client_id: text("client_id"),
                    scopes,
                });
            }
            Err(bad())
        }
        Some(_) => Err(bad()),
    }
}

/// Every plugin folder under `<home>/plugins`, sorted by name. Folders whose
/// name starts with `.` or `_` are skipped. Problems are reported, never
/// fatal: a broken plugin is left out and the rest load.
pub fn discover(home: &Path) -> (Vec<Manifest>, Vec<String>) {
    let mut manifests: Vec<Manifest> = Vec::new();
    let mut problems = Vec::new();
    let Ok(entries) = std::fs::read_dir(home.join("plugins")) else {
        return (manifests, problems);
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("plugin.toml").is_file())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| !n.starts_with('.') && !n.starts_with('_'))
        })
        .collect();
    dirs.sort();
    for dir in dirs {
        match load(&dir) {
            Ok(manifest) if manifests.iter().any(|m| m.name == manifest.name) => {
                problems.push(format!(
                    "{}: ya hay otro plugin con ese nombre",
                    manifest.name
                ));
            }
            Ok(manifest) => manifests.push(manifest),
            Err(problem) => problems.push(problem),
        }
    }
    (manifests, problems)
}

pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max_chars).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        PathBuf::from("/nowhere/demo")
    }

    #[test]
    fn a_full_stdio_manifest() {
        let m = parse(
            r#"
            description = "demo"
            timeout = "5s"
            expose = "listed"
            observa_charla = true
            [server]
            command = "./server"
            args = ["--x"]
            env = { TOKEN = "${DEMO_TOKEN}" }
            [tools.leer]
            kind = "read"
            [tools.crear]
            kind = "act"
            description = "crea una tarea"
            [[watch]]
            name = "hoy"
            at = "lun-vie 09:00"
            tool = "leer"
            args = { estado = "{dia_o_finde}", n = 3 }
            say = "Contale."
            cooldown = "20h"
            empty = ["sin tareas"]
            [[routine]]
            at = "vie 18:30"
            prompt = "Cierre de semana."
            "#,
            &dir(),
        )
        .unwrap();
        assert_eq!(m.name, "demo");
        assert!(m.enabled && m.observe_chat);
        assert_eq!(m.timeout, Duration::from_secs(5));
        assert_eq!(m.expose, Expose::Listed);
        assert_eq!(m.tools["crear"].kind, Some(Kind::Act));
        assert_eq!(m.tools["crear"].confirm, None);
        assert_eq!(m.watches[0].args["n"], 3);
        assert_eq!(m.watches[0].cooldown, Some(Duration::from_secs(72_000)));
        assert_eq!(m.routines[0].name, "routine1");
        assert!(
            matches!(m.server, Server::Stdio { ref env, .. } if env["TOKEN"] == "${DEMO_TOKEN}")
        );
    }

    #[test]
    fn a_remote_manifest_with_defaults() {
        let m = parse(
            r#"
            name = "linear"
            enabled = false
            [server]
            url = "https://mcp.linear.app/mcp"
            headers = { Authorization = "Bearer ${LINEAR_API_KEY}" }
            "#,
            &dir(),
        )
        .unwrap();
        assert_eq!(m.name, "linear");
        assert!(!m.enabled && !m.observe_chat);
        assert_eq!(m.timeout, DEFAULT_TIMEOUT);
        assert_eq!(m.expose, Expose::All);
    }

    #[test]
    fn mistakes_are_explained() {
        for (toml, needle) in [
            ("[server]\n", "command"),
            ("[server]\ncommand='a'\nurl='https://x'\n", "uno solo"),
            ("name='Mal Nombre'\n[server]\ncommand='a'\n", "inválido"),
            ("name='a__b'\n[server]\ncommand='a'\n", "inválido"),
            (
                "[server]\ncommand='a'\n[[watch]]\ntool='t'\nsay='s'\n",
                "every o at",
            ),
            (
                "[server]\ncommand='a'\n[[watch]]\nevery='10s'\ntool='t'\nsay='s'\n",
                "1m",
            ),
            (
                "[server]\ncommand='a'\n[tools.x]\nkind='write'\n",
                "inválido",
            ),
            ("[server]\ncommand='a'\nsecret='x'\n", "inválido"),
            ("[server]\nurl='ftp://x'\n", "https"),
        ] {
            let error = parse(toml, &dir()).unwrap_err();
            assert!(error.contains(needle), "{toml}: {error}");
        }
    }

    #[test]
    fn the_linear_example_in_the_repo_loads() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/linear");
        let m = load(&dir).unwrap();
        assert_eq!(m.name, "linear");
        assert!(matches!(&m.server, Server::Http { url, headers }
            if url == "https://mcp.linear.app/mcp" && headers.is_empty()));
        assert!(matches!(m.auth, Auth::OAuth { .. }));
        assert_eq!(m.tools["list_issues"].kind, Some(Kind::Read));
        assert_eq!(m.tools["save_issue"].kind, Some(Kind::Act));
        assert_eq!(m.watches[0].args["state"], "{dia_o_finde}");
        assert_eq!(m.routines[0].name, "cierre");
        assert!(m.instructions.unwrap().contains("Life"));
    }

    #[test]
    fn auth_kinds() {
        let auth = |extra: &str| {
            parse(&format!("{extra}\n[server]\nurl='https://x/mcp'\n"), &dir()).map(|m| m.auth)
        };
        assert_eq!(auth("").unwrap(), Auth::None);
        assert_eq!(
            auth("auth = 'oauth'").unwrap(),
            Auth::OAuth {
                client_id: None,
                scopes: None
            }
        );
        assert_eq!(
            auth("auth = { oauth = true, client_id = 'abc', scopes = ['read'] }").unwrap(),
            Auth::OAuth {
                client_id: Some("abc".into()),
                scopes: Some(vec!["read".into()])
            }
        );
        assert_eq!(
            auth("auth = { clave = 'LINEAR_API_KEY', ayuda = 'https://linear.app/settings' }")
                .unwrap(),
            Auth::Key {
                name: "LINEAR_API_KEY".into(),
                help: Some("https://linear.app/settings".into())
            }
        );
        for bad in [
            "auth = 'magia'",
            "auth = { clave = 'minusculas' }",
            "auth = { clave = 'K', ayuda = 'http://x' }",
            "auth = { otra = 1 }",
            "auth = 3",
        ] {
            assert!(auth(bad).is_err(), "{bad}");
        }
        let stdio = parse("auth = 'oauth'\n[server]\ncommand='x'\n", &dir()).unwrap_err();
        assert!(stdio.contains("url"), "{stdio}");
    }

    #[test]
    fn names() {
        for good in ["linear", "a1", "mi-plugin", "x_y"] {
            assert!(valid_name(good), "{good}");
        }
        for bad in [
            "",
            "Linear",
            "-a",
            "a__b",
            "a b",
            "ñ",
            "abcdefghijklmnopqrstuvwxy",
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }
}
