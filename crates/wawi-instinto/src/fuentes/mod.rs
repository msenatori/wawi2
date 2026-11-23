//! `vigia.fuente`: where a value comes from. Each kind is a strategy in a
//! registry, keyed by the one key of the `fuente` object (`mcp`, `http`,
//! `reloj`). A new kind is one more [`TipoFuente`] registered with
//! [`Registro::con`].

pub mod http;
pub mod mcp;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use wawi_tools::WallClock;

use crate::spec::Fuente;

/// What a source may use: the home (plugins, secrets), the clock and how
/// long it may take.
pub struct Ctx<'a> {
    pub home: &'a Path,
    pub clock: &'a dyn WallClock,
    pub timeout: Duration,
}

pub trait TipoFuente: Send + Sync {
    /// Checks the config without reading anything remote.
    fn validar(&self, config: &Value, ctx: &Ctx<'_>) -> Result<(), String>;
    /// One read. Errors are short Spanish reasons, without secrets.
    fn leer(&self, config: &Value, ctx: &Ctx<'_>) -> Result<Value, String>;
    /// When the next look should be, if the source knows better than
    /// `cada` (a clock does).
    fn proxima(&self, _config: &Value, _ctx: &Ctx<'_>) -> Option<i64> {
        None
    }
    /// Whether it comes true again and again by nature (a clock with
    /// `cada`), so the question repeats.
    fn repite(&self, _config: &Value) -> bool {
        false
    }
}

#[derive(Clone, Default)]
pub struct Registro {
    tipos: BTreeMap<String, Arc<dyn TipoFuente>>,
}

impl Registro {
    /// `mcp`, `http` and `reloj`.
    pub fn base() -> Self {
        Self::default()
            .con("mcp", Arc::new(mcp::Mcp))
            .con("http", Arc::new(http::Http::default()))
            .con("reloj", Arc::new(Reloj))
    }

    pub fn con(mut self, nombre: &str, tipo: Arc<dyn TipoFuente>) -> Self {
        self.tipos.insert(nombre.to_owned(), tipo);
        self
    }

    pub fn nombres(&self) -> Vec<&str> {
        self.tipos.keys().map(String::as_str).collect()
    }

    fn tipo(&self, fuente: &Fuente) -> Result<&Arc<dyn TipoFuente>, String> {
        self.tipos.get(&fuente.tipo).ok_or_else(|| {
            format!(
                "no conozco la fuente «{}»: hay {}",
                fuente.tipo,
                self.nombres().join(", ")
            )
        })
    }

    pub fn validar(&self, fuente: &Fuente, ctx: &Ctx<'_>) -> Result<(), String> {
        self.tipo(fuente)?.validar(&fuente.config, ctx)
    }

    pub fn leer(&self, fuente: &Fuente, ctx: &Ctx<'_>) -> Result<Value, String> {
        self.tipo(fuente)?.leer(&fuente.config, ctx)
    }

    pub fn proxima(&self, fuente: &Fuente, ctx: &Ctx<'_>) -> Option<i64> {
        self.tipo(fuente).ok()?.proxima(&fuente.config, ctx)
    }

    pub fn repite(&self, fuente: &Fuente) -> bool {
        self.tipo(fuente).is_ok_and(|t| t.repite(&fuente.config))
    }
}

/// `{"at": "AAAA-MM-DDTHH:MM"}` is true from that time on (once).
/// `{"cada": "1d"}` is true every period, counted from when it was taken.
pub struct Reloj;

enum RelojSpec {
    At(i64),
    Cada(i64),
}

fn reloj(config: &Value, clock: &dyn WallClock) -> Result<RelojSpec, String> {
    let at = config.get("at").and_then(Value::as_str);
    let cada = config.get("cada").and_then(Value::as_str);
    match (at, cada) {
        (Some(at), None) => Ok(RelojSpec::At(crate::clock::parse_epoch(clock, at)?)),
        (None, Some(cada)) => {
            let every = wawi_plugins::schedule::parse_duration(cada)?.as_secs() as i64;
            if every < 60 {
                return Err("reloj.cada tiene que ser de 1m o más".into());
            }
            Ok(RelojSpec::Cada(every))
        }
        _ => Err("reloj necesita at (\"AAAA-MM-DDTHH:MM\") o cada (\"1d\"), uno solo".into()),
    }
}

impl TipoFuente for Reloj {
    fn validar(&self, config: &Value, ctx: &Ctx<'_>) -> Result<(), String> {
        reloj(config, ctx.clock).map(|_| ())
    }

    fn leer(&self, config: &Value, ctx: &Ctx<'_>) -> Result<Value, String> {
        Ok(Value::Bool(match reloj(config, ctx.clock)? {
            RelojSpec::At(at) => ctx.clock.now() >= at,
            RelojSpec::Cada(_) => true,
        }))
    }

    fn proxima(&self, config: &Value, ctx: &Ctx<'_>) -> Option<i64> {
        let now = ctx.clock.now();
        match reloj(config, ctx.clock).ok()? {
            RelojSpec::At(at) => (at > now).then_some(at),
            RelojSpec::Cada(every) => Some(now + every),
        }
    }

    fn repite(&self, config: &Value) -> bool {
        config.get("cada").is_some()
    }
}

/// `<home>/instinto/claude`, an empty folder the headless model runs in.
pub fn claude_dir(home: &Path) -> PathBuf {
    home.join(crate::DIR).join("claude")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wawi_tools::{Civil, FakeClock};

    #[test]
    fn the_clock_source() {
        let clock = FakeClock::at(Civil::date(2026, 11, 23).at(9, 0, 0), -3);
        let home = Path::new("/nowhere");
        let ctx = Ctx {
            home,
            clock: clock.as_ref(),
            timeout: Duration::from_secs(1),
        };
        let r = Registro::base();
        let at = Fuente {
            tipo: "reloj".into(),
            config: json!({ "at": "2026-11-23T10:00" }),
        };
        assert!(r.validar(&at, &ctx).is_ok());
        assert_eq!(r.leer(&at, &ctx).unwrap(), false);
        assert_eq!(r.proxima(&at, &ctx), Some(clock.now() + 3600));
        assert!(!r.repite(&at));
        clock.advance(3600);
        assert_eq!(r.leer(&at, &ctx).unwrap(), true);
        assert_eq!(r.proxima(&at, &ctx), None);
        let cada = Fuente {
            tipo: "reloj".into(),
            config: json!({ "cada": "1d" }),
        };
        assert!(r.repite(&cada));
        assert_eq!(r.proxima(&cada, &ctx), Some(clock.now() + 86_400));
        for bad in [
            json!({}),
            json!({ "at": "mañana" }),
            json!({ "cada": "10s" }),
        ] {
            let f = Fuente {
                tipo: "reloj".into(),
                config: bad,
            };
            assert!(r.validar(&f, &ctx).is_err());
        }
        let unknown = Fuente {
            tipo: "ftp".into(),
            config: json!({}),
        };
        assert!(
            r.validar(&unknown, &ctx)
                .unwrap_err()
                .contains("mcp, reloj")
        );
    }
}
