//! Secrets never live in plugin.toml. It says `${LINEAR_API_KEY}` and the
//! value comes from `<home>/secrets.toml` (a flat table of strings, only if
//! its permissions are 0600 or tighter) or, failing that, the environment.
//! Every resolved value is remembered so logs can blank it out.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub const FILE: &str = "secrets.toml";
const BLANK: &str = "«secreto»";

#[derive(Clone, Debug, Default)]
pub struct Secrets {
    values: BTreeMap<String, String>,
    /// Values handed out so far, to blank them out of logs.
    used: Arc<Mutex<Vec<String>>>,
}

impl Secrets {
    /// Reads `<home>/secrets.toml` if there is one. Refuses a file anyone
    /// but the owner may read; the environment still works then.
    pub fn load(home: &Path) -> (Self, Option<String>) {
        let path = home.join(FILE);
        let Ok(metadata) = std::fs::metadata(&path) else {
            return (Self::default(), None);
        };
        if let Some(mode) = loose_mode(&metadata) {
            let problem = format!(
                "{} tiene permisos {mode:o}; no lo leo hasta que sean 600 (chmod 600 {})",
                FILE,
                path.display()
            );
            return (Self::default(), Some(problem));
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                return (
                    Self::default(),
                    Some(format!("no pude leer {FILE}: {error}")),
                );
            }
        };
        match toml::from_str::<BTreeMap<String, String>>(&text) {
            Ok(values) => (
                Self {
                    values,
                    used: Arc::default(),
                },
                None,
            ),
            // The parser's message could quote a value: say only where.
            Err(_) => (
                Self::default(),
                Some(format!(
                    "{FILE} no es una tabla de textos (NOMBRE = \"valor\"); no lo uso"
                )),
            ),
        }
    }

    pub fn from_values(values: BTreeMap<String, String>) -> Self {
        Self {
            values,
            used: Arc::default(),
        }
    }

    /// A copy where `${name}` gives `value` (from the Keychain), ahead of
    /// secrets.toml and the environment. The log blanking stays shared.
    pub fn with_value(&self, name: &str, value: &str) -> Self {
        let mut values = self.values.clone();
        values.insert(name.to_owned(), value.to_owned());
        Self {
            values,
            used: self.used.clone(),
        }
    }

    /// Blanks `value` out of logs from now on (an OAuth token, say).
    pub fn remember(&self, value: &str) {
        if let Ok(mut used) = self.used.lock()
            && !used.iter().any(|v| v == value)
        {
            used.push(value.to_owned());
        }
    }

    /// Replaces every `${NAME}` in `template`. A missing name is an error
    /// that names it, never a value.
    pub fn resolve(&self, template: &str) -> Result<String, String> {
        let mut out = String::new();
        let mut rest = template;
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let end = after
                .find('}')
                .ok_or_else(|| format!("falta cerrar «${{» en «{template}»"))?;
            let name = &after[..end];
            let value = self
                .values
                .get(name)
                .cloned()
                .or_else(|| std::env::var(name).ok())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    format!("falta el secreto {name}: ponelo en {FILE} o en el entorno")
                })?;
            if let Ok(mut used) = self.used.lock()
                && !used.contains(&value)
            {
                used.push(value.clone());
            }
            out.push_str(&value);
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    /// `text` with every secret handed out so far blanked, and anything that
    /// looks like a bearer token too.
    pub fn scrub(&self, text: &str) -> String {
        let mut out = text.to_owned();
        if let Ok(used) = self.used.lock() {
            for value in used.iter().filter(|v| v.len() >= 4) {
                out = out.replace(value.as_str(), BLANK);
            }
        }
        scrub_bearer(&out)
    }
}

fn scrub_bearer(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(index) = rest.to_ascii_lowercase().find("bearer ") {
        let (before, after) = rest.split_at(index + "bearer ".len());
        out.push_str(before);
        let token_len = after
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(after.len());
        if token_len > 0 {
            out.push_str(BLANK);
        }
        rest = &after[token_len..];
    }
    out.push_str(rest);
    out
}

#[cfg(unix)]
fn loose_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    let mode = metadata.permissions().mode() & 0o777;
    (mode & 0o077 != 0).then_some(mode)
}

#[cfg(not(unix))]
fn loose_mode(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wawi-secrets-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolves_from_the_file_then_the_environment() {
        let secrets = Secrets::from_values(BTreeMap::from([("A_KEY".into(), "abc123".into())]));
        assert_eq!(secrets.resolve("Bearer ${A_KEY}").unwrap(), "Bearer abc123");
        assert_eq!(secrets.resolve("sin nada").unwrap(), "sin nada");
        let missing = secrets.resolve("${WAWI_TEST_NO_EXISTE_JAMAS}").unwrap_err();
        assert!(missing.contains("WAWI_TEST_NO_EXISTE_JAMAS"));
        assert!(secrets.resolve("${ROTO").is_err());
        // PATH is always there.
        assert!(!secrets.resolve("${PATH}").unwrap().is_empty());
    }

    #[test]
    fn logs_never_carry_a_value() {
        let secrets = Secrets::from_values(BTreeMap::from([("K".into(), "s3cr3t-value".into())]));
        let _ = secrets.resolve("${K}");
        assert_eq!(
            secrets.scrub("falló con s3cr3t-value y Authorization: Bearer otro.token"),
            "falló con «secreto» y Authorization: Bearer «secreto»"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_others_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp("perm");
        let path = dir.join(FILE);
        std::fs::write(&path, "K = \"v1234\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let (secrets, problem) = Secrets::load(&dir);
        assert!(problem.unwrap().contains("644"));
        assert!(secrets.resolve("${K}").is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (secrets, problem) = Secrets::load(&dir);
        assert_eq!(problem, None);
        assert_eq!(secrets.resolve("${K}").unwrap(), "v1234");
        std::fs::write(&path, "K = 3\n").unwrap();
        let (_, problem) = Secrets::load(&dir);
        assert!(!problem.unwrap().contains('3'));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
