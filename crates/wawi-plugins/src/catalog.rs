//! What can be installed, and the file work of installing it.
//!
//! Two kinds of plugins:
//!
//! - **Wawi's own** ([`BUILTIN`]): instinto, caracter, curiosidad and
//!   autoarreglo. They ship with Wawi in the catalog folder (`plugins/` in
//!   the source tree, `$WAWI2_ASSETS/plugins`, `Wawi.app/Contents/Resources/
//!   plugins`, or `<prefix>/plugins` next to `bin/wawi2`). Their programs
//!   (`wawi-instinto`, ...) are looked up next to the `wawi2` binary first
//!   (the app bundle's `MacOS` folder), then on the PATH.
//! - **Connectors**: remote MCP servers the owner adds from a URL. Wawi
//!   probes how the server wants to be authorized (OAuth or a key) and
//!   writes `<home>/plugins/<name>/plugin.toml` itself. A short curated
//!   list of [`Suggestion`]s (`assets/catalogo/sugeridos.toml`) makes the
//!   common ones one click; none is installed by default.
//!
//! Installing is a copy into `<home>/plugins`. The owner never copies a
//! folder, edits secrets.toml or runs chmod. Everything here is file work
//! (fast) except [`probe`], which talks to the server: run it off the host
//! thread.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::manifest::{Auth, valid_name};
use crate::oauth;

/// Wawi's own plugins, in the order they are listed.
pub const BUILTIN: &[&str] = &["instinto", "caracter", "curiosidad", "autoarreglo"];

/// The suggested connectors that ship with Wawi.
pub const SUGGESTIONS: &str = include_str!("../../../assets/catalogo/sugeridos.toml");

/// A connector Wawi suggests adding.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Suggestion {
    #[serde(rename = "nombre")]
    pub name: String,
    #[serde(rename = "titulo")]
    pub title: String,
    pub url: String,
    #[serde(rename = "descripcion", default)]
    pub description: String,
    #[serde(rename = "icono", default)]
    pub icon: String,
    /// A folder of the catalog to start from (schedules, notes, rules).
    #[serde(rename = "plantilla", default)]
    pub template: Option<String>,
}

#[derive(Deserialize)]
struct SuggestionFile {
    #[serde(default)]
    sugerido: Vec<Suggestion>,
}

/// Parses a suggestions file. Entries with a bad name or a non-https URL
/// are left out.
pub fn parse_suggestions(text: &str) -> Vec<Suggestion> {
    toml::from_str::<SuggestionFile>(text)
        .map(|file| file.sugerido)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| valid_name(&s.name) && s.url.starts_with("https://"))
        .collect()
}

pub fn suggestions() -> Vec<Suggestion> {
    parse_suggestions(SUGGESTIONS)
}

/// The folder of plugins that ship with Wawi, if there is one.
pub fn default_dir() -> Option<PathBuf> {
    if let Some(assets) = std::env::var_os("WAWI2_ASSETS") {
        let dir = PathBuf::from(assets).join("plugins");
        if dir.is_dir() {
            return Some(dir);
        }
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_owned));
    if let Some(dir) = exe_dir
        .as_ref()
        .filter(|dir| dir.ends_with("Contents/MacOS"))
        .map(|dir| dir.join("../Resources/plugins"))
        .filter(|dir| dir.is_dir())
    {
        return Some(dir.canonicalize().unwrap_or(dir));
    }
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    if source.is_dir() {
        return Some(source.canonicalize().unwrap_or(source));
    }
    exe_dir
        .map(|bin| bin.join("../plugins"))
        .filter(|dir| dir.is_dir())
}

/// Where a plugin's program is: a path with a `/` is taken as is (`./x` is
/// relative to the plugin folder); a bare name is looked up next to the
/// `wawi2` binary first (in the app, its `MacOS` folder), then on the PATH.
pub fn resolve_command(command: &str, plugin_dir: &Path) -> Option<PathBuf> {
    if command.starts_with("./") || command.starts_with("../") {
        let path = plugin_dir.join(command);
        return path.is_file().then_some(path);
    }
    if command.contains('/') {
        let path = PathBuf::from(command);
        return path.is_file().then_some(path);
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(command)))
        .filter(|path| path.is_file());
    if beside.is_some() {
        return beside;
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(command))
            .find(|candidate| candidate.is_file())
    })
}

/// How to get a missing program, for the owner.
pub fn how_to_get(command: &str) -> String {
    let crate_name = command.trim_start_matches("./");
    format!(
        "Falta el programa {command}. Compilalo una vez desde el código de Wawi con «cargo build --release -p {crate_name}» (queda junto a wawi2), o instalalo con «cargo install --path crates/{crate_name} --locked»."
    )
}

/// Copies `catalog/<name>` to `<home>/plugins/<name>` unless it is there
/// already. Returns the installed folder.
pub fn install(catalog: &Path, home: &Path, name: &str) -> Result<PathBuf, String> {
    if !valid_name(name) {
        return Err(format!("«{name}» no es un nombre de plugin"));
    }
    let from = catalog.join(name);
    if !from.join("plugin.toml").is_file() {
        return Err(format!("no tengo el plugin {name} para instalar"));
    }
    let to = home.join("plugins").join(name);
    if to.join("plugin.toml").is_file() {
        return Ok(to);
    }
    copy_dir(&from, &to).map_err(|e| format!("no pude instalar {name}: {e}"))?;
    Ok(to)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Sets `enabled` in a plugin.toml, keeping every other line (and the
/// comments) as they are. Only a top-level `enabled` (before any table) is
/// touched; with none, one is added at the top.
pub fn set_enabled_text(text: &str, enabled: bool) -> String {
    let line = format!("enabled = {enabled}");
    let mut out = Vec::new();
    let mut done = false;
    let mut in_table = false;
    for original in text.lines() {
        let trimmed = original.trim_start();
        if trimmed.starts_with('[') {
            in_table = true;
        }
        let is_enabled = !in_table
            && trimmed
                .strip_prefix("enabled")
                .is_some_and(|rest| rest.trim_start().starts_with('='));
        if is_enabled && !done {
            out.push(line.clone());
            done = true;
        } else if !is_enabled {
            out.push(original.to_owned());
        }
    }
    if !done {
        // Before the first non-comment line, so it stays top-level.
        let at = out
            .iter()
            .position(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with('#')
            })
            .unwrap_or(out.len());
        out.insert(at, line);
    }
    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

pub fn set_enabled(dir: &Path, enabled: bool) -> Result<(), String> {
    let path = dir.join("plugin.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("no pude leer {e}"))?;
    write_atomic(&path, &set_enabled_text(&text, enabled))
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| format!("no pude escribir {}: {e}", path.display()))
}

/// Moves `<home>/plugins/<name>` to `<home>/plugins/.borrados/` (ignored
/// when plugins load; the owner can bring it back by hand).
pub fn remove(home: &Path, name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("«{name}» no es un nombre de plugin"));
    }
    let dir = home.join("plugins").join(name);
    if !dir.is_dir() {
        return Ok(());
    }
    let trash = home.join("plugins/.borrados");
    std::fs::create_dir_all(&trash).map_err(|e| e.to_string())?;
    let target = trash.join(format!("{name}-{}", oauth::now_s()));
    std::fs::rename(&dir, &target).map_err(|e| format!("no pude sacar {name}: {e}"))
}

/// A plugin name from a server URL: `https://mcp.linear.app/mcp` gives
/// `linear`. `taken` names get `-2`, `-3`...
pub fn name_for_url(url: &str, taken: &[String]) -> String {
    let host = oauth::Url::parse(url)
        .map(|u| u.host().to_owned())
        .unwrap_or_default();
    let local = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok();
    let labels: Vec<&str> = host
        .split('.')
        .filter(|l| !l.is_empty() && !["mcp", "api", "www", "server"].contains(l))
        .collect();
    let pick = match labels.len() {
        _ if local => "local",
        0 => "conector",
        1 => labels[0],
        // Drop the TLD: `linear.app` gives `linear`, `x.example.com` gives `example`.
        n => labels[n - 2],
    };
    let mut base: String = pick
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    base = base.trim_matches('-').chars().take(20).collect();
    if base.is_empty() || !valid_name(&base) {
        base = "conector".into();
    }
    let mut name = base.clone();
    let mut n = 2;
    while taken.contains(&name) {
        name = format!("{base}-{n}");
        n += 1;
    }
    name
}

/// The default key name for a connector: `LINEAR_API_KEY`.
pub fn key_name(plugin: &str) -> String {
    format!("{}_API_KEY", plugin.to_ascii_uppercase().replace('-', "_"))
}

/// A plugin.toml for a remote connector.
pub fn connector_toml(url: &str, description: &str, auth: &Auth) -> String {
    let auth_line = match auth {
        Auth::None => "auth = \"ninguna\"".to_owned(),
        Auth::OAuth { .. } => "auth = \"oauth\"".to_owned(),
        Auth::Key { name, help } => match help {
            Some(help) => format!("auth = {{ clave = \"{name}\", ayuda = \"{help}\" }}"),
            None => format!("auth = {{ clave = \"{name}\" }}"),
        },
    };
    let description = description.replace(['"', '\\', '\n'], " ");
    let url = url.replace(['"', '\\', '\n'], "");
    format!(
        "# Conector agregado por Wawi desde {url}.\n# Podés editarlo: docs/PLUGINS.md explica cada campo.\n\ndescription = \"{description}\"\ntimeout = \"30s\"\n{auth_line}\n\n[server]\nurl = \"{url}\"\n"
    )
}

/// Writes `<home>/plugins/<name>/`: from the suggestion's template when the
/// catalog has it (its `auth` replaced by what the server wants), else a
/// fresh plugin.toml.
pub fn write_connector(
    home: &Path,
    catalog: Option<&Path>,
    name: &str,
    url: &str,
    description: &str,
    template: Option<&str>,
    auth: &Auth,
) -> Result<PathBuf, String> {
    let dir = home.join("plugins").join(name);
    if dir.join("plugin.toml").is_file() {
        return Err(format!("ya hay un plugin {name}"));
    }
    let template_dir = catalog
        .zip(template)
        .map(|(catalog, t)| catalog.join(t))
        .filter(|d| d.join("plugin.toml").is_file());
    match template_dir {
        Some(from) => {
            copy_dir(&from, &dir).map_err(|e| format!("no pude crear {name}: {e}"))?;
            // The template's own auth may not be what the server wants now.
            let path = dir.join("plugin.toml");
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let wanted = crate::manifest::parse(&text, &dir).map(|m| m.auth);
            if wanted.as_ref() != Ok(auth) {
                let fresh = connector_toml(url, description, auth);
                write_atomic(&path, &fresh)?;
            }
        }
        None => {
            std::fs::create_dir_all(&dir).map_err(|e| format!("no pude crear {name}: {e}"))?;
            write_atomic(
                &dir.join("plugin.toml"),
                &connector_toml(url, description, auth),
            )?;
        }
    }
    Ok(dir)
}

/// How a server wants to be authorized: OAuth if it publishes the MCP
/// authorization metadata, a key if it says 401 without it, nothing if it
/// answers an unauthenticated `initialize`. Talks to the server (blocking).
pub fn probe(url: &str, key: &str) -> Result<Auth, String> {
    oauth::check_secure(url)?;
    if oauth::discover(url, None).is_ok() {
        return Ok(Auth::OAuth {
            client_id: None,
            scopes: None,
        });
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .build()
        .into();
    let hello = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": crate::mcp::PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "wawi2", "version": env!("CARGO_PKG_VERSION") },
        },
    });
    let response = agent
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .send(hello.to_string())
        .map_err(|e| format!("no pude hablar con {url}: {e}"))?;
    let status = response.status().as_u16();
    match status {
        200..=299 => Ok(Auth::None),
        401 | 403 => {
            let header = response
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            if oauth::discover(url, Some(&header)).is_ok() {
                Ok(Auth::OAuth {
                    client_id: None,
                    scopes: None,
                })
            } else {
                Ok(Auth::Key {
                    name: key.to_owned(),
                    help: None,
                })
            }
        }
        other => Err(format!("{url} no parece un servidor MCP (HTTP {other})")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_suggestions_ship_linear_and_nothing_is_builtin_there() {
        let list = suggestions();
        let linear = list.iter().find(|s| s.name == "linear").unwrap();
        assert_eq!(linear.url, "https://mcp.linear.app/mcp");
        assert_eq!(linear.title, "Linear");
        assert_eq!(linear.template.as_deref(), Some("linear"));
        assert!(list.iter().all(|s| !BUILTIN.contains(&s.name.as_str())));
        assert!(
            parse_suggestions("[[sugerido]]\nnombre='x'\ntitulo='X'\nurl='http://x'\n").is_empty()
        );
    }

    #[test]
    fn the_source_tree_has_wawis_own_plugins() {
        let dir = default_dir().unwrap();
        for name in BUILTIN {
            assert!(dir.join(name).join("plugin.toml").is_file(), "{name}");
        }
        assert!(!BUILTIN.contains(&"linear"));
    }

    #[test]
    fn names_from_urls() {
        assert_eq!(name_for_url("https://mcp.linear.app/mcp", &[]), "linear");
        assert_eq!(name_for_url("https://api.example.com/x", &[]), "example");
        assert_eq!(name_for_url("http://127.0.0.1:8080/mcp", &[]), "local");
        assert_eq!(
            name_for_url("https://mcp.linear.app/mcp", &["linear".into()]),
            "linear-2"
        );
        assert_eq!(name_for_url("https://localhost/mcp", &[]), "local");
        assert_eq!(key_name("mi-cosa"), "MI_COSA_API_KEY");
    }

    #[test]
    fn enabled_is_edited_in_place() {
        let text = "# hola\ndescription = \"x\"\nenabled = true\n\n[server]\ncommand = \"a\"\n[tools.x]\nenabled = 3\n";
        let off = set_enabled_text(text, false);
        assert!(off.contains("# hola\ndescription = \"x\"\nenabled = false\n"));
        assert!(off.contains("[tools.x]\nenabled = 3"));
        let fresh = set_enabled_text("# c\n\ndescription = \"x\"\n[server]\n", false);
        assert!(
            fresh.starts_with("# c\n\nenabled = false\ndescription"),
            "{fresh}"
        );
        let m = crate::manifest::parse(
            &fresh.replace("[server]", "[server]\ncommand='a'"),
            Path::new("/x/a"),
        )
        .unwrap();
        assert!(!m.enabled);
    }

    #[test]
    fn connector_files_parse_back() {
        let home = std::env::temp_dir().join(format!("wawi-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        for (name, auth) in [
            (
                "uno",
                Auth::OAuth {
                    client_id: None,
                    scopes: None,
                },
            ),
            (
                "dos",
                Auth::Key {
                    name: "DOS_API_KEY".into(),
                    help: None,
                },
            ),
            ("tres", Auth::None),
        ] {
            let dir = write_connector(
                &home,
                None,
                name,
                "https://x.com/mcp",
                "Algo \"raro\"",
                None,
                &auth,
            )
            .unwrap();
            let m = crate::manifest::load(&dir).unwrap();
            assert_eq!(m.auth, auth);
            assert_eq!(m.name, name);
        }
        // From the Linear template, with the auth the server asked for.
        let catalog = default_dir().unwrap();
        let key = Auth::Key {
            name: "LINEAR_API_KEY".into(),
            help: None,
        };
        let dir = write_connector(
            &home,
            Some(&catalog),
            "linear",
            "https://mcp.linear.app/mcp",
            "x",
            Some("linear"),
            &Auth::OAuth {
                client_id: None,
                scopes: None,
            },
        )
        .unwrap();
        let m = crate::manifest::load(&dir).unwrap();
        assert!(!m.watches.is_empty(), "keeps the template's schedule");
        assert!(write_connector(&home, Some(&catalog), "linear", "u", "x", None, &key).is_err());
        remove(&home, "linear").unwrap();
        assert!(!home.join("plugins/linear").exists());
        assert!(
            crate::manifest::discover(&home)
                .0
                .iter()
                .all(|m| m.name != "linear")
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn programs_are_found_beside_wawi_then_on_the_path() {
        let dir = Path::new("/nowhere");
        assert!(resolve_command("sh", dir).is_some());
        assert!(resolve_command("wawi-no-existe-jamas", dir).is_none());
        assert!(how_to_get("wawi-instinto").contains("cargo build --release -p wawi-instinto"));
    }
}
