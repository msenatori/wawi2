//! `<home>/instinto/config.toml`, all optional:
//!
//! ```toml
//! silencio = "23:00-08:00"   # no facts are written inside it ("" = never quiet)
//! [juzgar]
//! modelo = "claude-haiku-5-5"
//! max_dia = 40               # judge calls per day, all questions together
//! [observador]
//! modelo = "claude-sonnet-5-5"
//! esfuerzo = "low"
//! confianza = 0.8
//! max_dia = 5                # new follow-ups per day
//! ventana = 6                # turns of context
//! ```

use std::path::Path;

use serde::Deserialize;

pub const FILE: &str = "instinto/config.toml";

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub silencio: String,
    pub juzgar: Juzgar,
    pub observador: Observador,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Juzgar {
    pub modelo: String,
    pub max_dia: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Observador {
    pub modelo: String,
    pub esfuerzo: String,
    pub confianza: f64,
    pub max_dia: u32,
    pub ventana: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            silencio: "23:00-08:00".into(),
            juzgar: Juzgar::default(),
            observador: Observador::default(),
        }
    }
}

impl Default for Juzgar {
    fn default() -> Self {
        Self {
            modelo: "claude-haiku-5-5".into(),
            max_dia: 40,
        }
    }
}

impl Default for Observador {
    fn default() -> Self {
        Self {
            modelo: "claude-sonnet-5-5".into(),
            esfuerzo: "low".into(),
            confianza: 0.8,
            max_dia: 5,
            ventana: 6,
        }
    }
}

impl Config {
    /// The file, or the defaults (with the reason when the file is wrong).
    pub fn load(home: &Path) -> (Self, Option<String>) {
        let Ok(text) = std::fs::read_to_string(home.join(FILE)) else {
            return (Self::default(), None);
        };
        match toml::from_str::<Config>(&text) {
            Ok(config) => match config.quiet() {
                Ok(_) => (config, None),
                Err(e) => (
                    Self::default(),
                    Some(format!("{FILE}: {e}; uso los valores por defecto")),
                ),
            },
            Err(e) => (
                Self::default(),
                Some(format!(
                    "{FILE} no se entiende ({}); uso los valores por defecto",
                    e.message()
                )),
            ),
        }
    }

    /// The quiet hours as minutes since midnight, `None` for never.
    pub fn quiet(&self) -> Result<Option<(u16, u16)>, String> {
        let text = self.silencio.trim();
        if text.is_empty() {
            return Ok(None);
        }
        let minute = |t: &str| -> Result<u16, String> {
            let (h, m) = t
                .trim()
                .split_once(':')
                .ok_or("silencio es \"HH:MM-HH:MM\"")?;
            let (h, m): (u16, u16) = (
                h.parse().map_err(|_| "silencio es \"HH:MM-HH:MM\"")?,
                m.parse().map_err(|_| "silencio es \"HH:MM-HH:MM\"")?,
            );
            if h > 23 || m > 59 {
                return Err("silencio es \"HH:MM-HH:MM\"".into());
            }
            Ok(h * 60 + m)
        };
        let (from, until) = text.split_once('-').ok_or("silencio es \"HH:MM-HH:MM\"")?;
        Ok(Some((minute(from)?, minute(until)?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let c = Config::default();
        assert_eq!(c.quiet().unwrap(), Some((1380, 480)));
        assert_eq!(c.observador.confianza, 0.8);
        let c: Config = toml::from_str("silencio = \"\"\n[observador]\nmax_dia = 2\n").unwrap();
        assert_eq!(c.quiet().unwrap(), None);
        assert_eq!(c.observador.max_dia, 2);
        assert_eq!(c.observador.modelo, "claude-sonnet-5-5");
        assert!(toml::from_str::<Config>("otra = 1").is_err());
    }
}
