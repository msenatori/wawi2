//! The follow-up question and its `vigia`: a declarative pipeline, pipes
//! and filters. `fuente` (a strategy, looked up in a registry) gives a
//! value, `extraer` narrows it, `cuando` (composable specifications) says
//! whether it came true, and the optional `juzgar` asks a cheap model a
//! yes/no question about new items only. Parsing here is pure; every
//! mistake comes back as a short Spanish reason.

use regex::Regex;
use serde_json::Value;
use wawi_plugins::inbox::valid_id;
use wawi_plugins::schedule::parse_duration;

use crate::jsonpath::Path;

/// The schema version this worker understands.
pub const VERSION: u64 = 1;
/// Ids leave room for the suffixes of repeated and broken facts.
pub const MAX_ID: usize = 48;
/// How often a source is read unless the spec says otherwise.
pub const DEFAULT_EVERY_S: u64 = 15 * 60;
pub const MIN_EVERY_S: u64 = 60;
/// Consecutive failures before a question is `rota`.
pub const DEFAULT_MAX_FAILURES: u32 = 3;
const MAX_TEXT: usize = 500;

#[derive(Clone, Debug)]
pub struct Pregunta {
    pub id: String,
    pub version: u64,
    pub origen: String,
    pub creado: String,
    pub vence: Option<String>,
    /// What is being watched, in a few words, for the owner. Optional;
    /// `al_cumplirse.decir` stands in.
    pub que: Option<String>,
    pub vigia: Vigia,
    pub al_cumplirse: AlCumplirse,
    /// The item as written, kept verbatim in the state file.
    pub raw: Value,
}

impl Pregunta {
    pub fn que(&self) -> &str {
        self.que.as_deref().unwrap_or(&self.al_cumplirse.decir)
    }
}

#[derive(Clone, Debug)]
pub struct AlCumplirse {
    pub decir: String,
    pub contexto: Value,
}

#[derive(Clone, Debug)]
pub struct Fuente {
    /// The registry key: `mcp`, `http`, `reloj`...
    pub tipo: String,
    pub config: Value,
}

#[derive(Clone, Debug)]
pub struct Vigia {
    pub fuente: Fuente,
    pub extraer: Vec<Extraer>,
    pub cuando: Option<Cuando>,
    pub juzgar: Option<Juzgar>,
    /// Seconds between looks; `None` lets the source decide (a `reloj`).
    pub cada: Option<u64>,
    pub hasta: Option<String>,
    pub repetir: bool,
    pub fallos_max: u32,
}

#[derive(Clone, Debug)]
pub enum Extraer {
    JsonPath(Path),
    Regex {
        re: Regex,
        todos: bool,
    },
    /// RSS or Atom items as `{id, titulo, link, fecha, resumen}`.
    Rss,
}

#[derive(Clone, Debug)]
pub enum Pred {
    Igual(Value),
    Distinto(Value),
    Menor(f64),
    Mayor(f64),
    Contiene(String),
    Coincide(Regex),
    Existe(bool),
    Cambio,
    /// Items whose id was never seen. `id` picks the id out of each item.
    Nuevos {
        id: Option<Path>,
    },
}

#[derive(Clone, Debug)]
pub enum Cuando {
    Hoja { en: Option<Path>, pred: Pred },
    Y(Vec<Cuando>),
    O(Vec<Cuando>),
    No(Box<Cuando>),
}

impl Cuando {
    /// Whether it keeps state between looks (`cambio`, `nuevos`).
    pub fn has_nuevos(&self) -> bool {
        match self {
            Cuando::Hoja {
                pred: Pred::Nuevos { .. },
                ..
            } => true,
            Cuando::Hoja { .. } => false,
            Cuando::Y(list) | Cuando::O(list) => list.iter().any(Cuando::has_nuevos),
            Cuando::No(inner) => inner.has_nuevos(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Juzgar {
    pub pregunta: String,
    pub modelo: Option<String>,
}

fn text_field(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Reads a question from the queue (or from `vigilar`). Sources are checked
/// here only for shape; their own config is checked by the registry.
pub fn parse_pregunta(value: &Value) -> Result<Pregunta, String> {
    if !value.is_object() {
        return Err("la pregunta tiene que ser un objeto JSON".into());
    }
    let id = text_field(value, "id").ok_or("falta id")?;
    if !valid_id(&id) || id.len() > MAX_ID {
        return Err(format!(
            "id «{id}» inválido: letras, números, . _ -, hasta {MAX_ID}"
        ));
    }
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .unwrap_or(VERSION);
    if version != VERSION {
        return Err(format!(
            "version {version}: este worker entiende la {VERSION}"
        ));
    }
    let origen = text_field(value, "origen").unwrap_or_else(|| "externo".into());
    let creado = text_field(value, "creado").unwrap_or_default();
    let vence = text_field(value, "vence");
    if let Some(vence) = &vence {
        check_local(vence, "vence")?;
    }
    let que = text_field(value, "que").map(|q| crate::texto::truncate(&q, 120));
    let vigia = parse_vigia(value.get("vigia").ok_or("falta vigia")?)?;
    let al = value.get("al_cumplirse").ok_or("falta al_cumplirse")?;
    let decir = text_field(al, "decir").ok_or("al_cumplirse necesita decir")?;
    if decir.chars().count() > MAX_TEXT {
        return Err(format!(
            "al_cumplirse.decir es muy largo (hasta {MAX_TEXT})"
        ));
    }
    Ok(Pregunta {
        id,
        version,
        origen,
        creado,
        vence,
        que,
        vigia,
        al_cumplirse: AlCumplirse {
            decir,
            contexto: al.get("contexto").cloned().unwrap_or(Value::Null),
        },
        raw: value.clone(),
    })
}

fn check_local(text: &str, field: &str) -> Result<(), String> {
    wawi_tools::clock::parse_local(text)
        .map(|_| ())
        .ok_or_else(|| format!("{field} «{text}» no es una hora local AAAA-MM-DDTHH:MM"))
}

pub fn parse_vigia(value: &Value) -> Result<Vigia, String> {
    let map = value.as_object().ok_or("vigia tiene que ser un objeto")?;
    for key in map.keys() {
        if ![
            "fuente",
            "extraer",
            "cuando",
            "juzgar",
            "cada",
            "hasta",
            "repetir",
            "fallos_max",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("vigia no conoce «{key}»"));
        }
    }
    let fuente = parse_fuente(value.get("fuente").ok_or("vigia necesita fuente")?)?;
    let extraer = match value.get("extraer") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(list)) => list.iter().map(parse_extraer).collect::<Result<_, _>>()?,
        Some(one) => vec![parse_extraer(one)?],
    };
    let cuando = match value.get("cuando") {
        None | Some(Value::Null) => None,
        Some(c) => Some(parse_cuando(c)?),
    };
    let juzgar = match value.get("juzgar") {
        None | Some(Value::Null) => None,
        Some(j) => Some(Juzgar {
            pregunta: text_field(j, "pregunta")
                .ok_or("juzgar necesita pregunta (una pregunta de sí o no)")?,
            modelo: text_field(j, "modelo"),
        }),
    };
    if juzgar.is_some() && !cuando.as_ref().is_some_and(Cuando::has_nuevos) {
        return Err("juzgar va solo junto a nuevos: el modelo mira nada más lo nuevo".into());
    }
    let cada = match value.get("cada") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => {
            let every = parse_duration(text)?.as_secs();
            if every < MIN_EVERY_S {
                return Err("cada tiene que ser de 1m o más".into());
            }
            Some(every)
        }
        Some(_) => return Err("cada es un texto como \"10m\" o \"1h\"".into()),
    };
    let hasta = text_field(value, "hasta");
    if let Some(hasta) = &hasta {
        check_local(hasta, "hasta")?;
    }
    let repetir = match value.get("repetir") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("repetir es true o false".into()),
    };
    let fallos_max = match value.get("fallos_max") {
        None | Some(Value::Null) => DEFAULT_MAX_FAILURES,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=20).contains(n))
            .ok_or("fallos_max es un número de 1 a 20")? as u32,
    };
    Ok(Vigia {
        fuente,
        extraer,
        cuando,
        juzgar,
        cada,
        hasta,
        repetir,
        fallos_max,
    })
}

fn parse_fuente(value: &Value) -> Result<Fuente, String> {
    let map = value
        .as_object()
        .filter(|m| m.len() == 1)
        .ok_or("fuente es un objeto con una sola clave: mcp, http o reloj")?;
    let (tipo, config) = map.iter().next().ok_or("fuente vacía")?;
    Ok(Fuente {
        tipo: tipo.clone(),
        config: config.clone(),
    })
}

fn parse_extraer(value: &Value) -> Result<Extraer, String> {
    if value.as_str() == Some("rss") {
        return Ok(Extraer::Rss);
    }
    let map = value
        .as_object()
        .ok_or("extraer es {\"jsonpath\": ...}, {\"regex\": ...} o \"rss\"")?;
    if let Some(path) = map.get("jsonpath") {
        let path = path.as_str().ok_or("jsonpath es un texto como \"$.a.b\"")?;
        return Ok(Extraer::JsonPath(Path::parse(path)?));
    }
    if let Some(pattern) = map.get("regex") {
        let pattern = pattern.as_str().ok_or("regex es un texto")?;
        let todos = map.get("todos").and_then(Value::as_bool).unwrap_or(false);
        return Ok(Extraer::Regex {
            re: compile(pattern)?,
            todos,
        });
    }
    if map.contains_key("rss") {
        return Ok(Extraer::Rss);
    }
    if map.contains_key("css") {
        return Err("extraer css no está: usá regex o rss".into());
    }
    Err("extraer conoce jsonpath, regex y rss".into())
}

fn compile(pattern: &str) -> Result<Regex, String> {
    regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| {
            format!(
                "la regex «{pattern}» no compila: {}",
                first_line(&e.to_string())
            )
        })
}

fn first_line(text: &str) -> String {
    text.lines().last().unwrap_or(text).trim().to_owned()
}

const LEAVES: &[&str] = &[
    "igual", "distinto", "menor", "mayor", "contiene", "coincide", "existe", "cambio", "nuevos",
];

pub fn parse_cuando(value: &Value) -> Result<Cuando, String> {
    parse_cuando_at(value, 0)
}

fn parse_cuando_at(value: &Value, depth: usize) -> Result<Cuando, String> {
    if depth > 8 {
        return Err("cuando anida demasiado (hasta 8)".into());
    }
    let map = value
        .as_object()
        .ok_or("cuando es un objeto, como {\"igual\": \"APPROVED\"}")?;
    let list = |v: &Value, name: &str| -> Result<Vec<Cuando>, String> {
        let items = v
            .as_array()
            .filter(|l| !l.is_empty())
            .ok_or_else(|| format!("{name} es una lista de condiciones"))?;
        items
            .iter()
            .map(|i| parse_cuando_at(i, depth + 1))
            .collect()
    };
    if map.len() == 1 {
        if let Some(v) = map.get("y") {
            return Ok(Cuando::Y(list(v, "y")?));
        }
        if let Some(v) = map.get("o") {
            return Ok(Cuando::O(list(v, "o")?));
        }
        if let Some(v) = map.get("no") {
            return Ok(Cuando::No(Box::new(parse_cuando_at(v, depth + 1)?)));
        }
    }
    let en = match map.get("en") {
        None => None,
        Some(Value::String(p)) => Some(Path::parse(p)?),
        Some(_) => return Err("en es un camino como \"$.estado\"".into()),
    };
    let leaves: Vec<&String> = map.keys().filter(|k| k.as_str() != "en").collect();
    let [key] = leaves.as_slice() else {
        return Err(format!(
            "cada condición tiene una sola de: {}, y, o, no (más en, opcional)",
            LEAVES.join(", ")
        ));
    };
    let arg = &map[key.as_str()];
    let number = |v: &Value| -> Result<f64, String> {
        v.as_f64()
            .or_else(|| v.as_str().and_then(crate::texto::parse_number))
            .ok_or_else(|| format!("{key} necesita un número"))
    };
    let pred = match key.as_str() {
        "igual" => Pred::Igual(arg.clone()),
        "distinto" => Pred::Distinto(arg.clone()),
        "menor" => Pred::Menor(number(arg)?),
        "mayor" => Pred::Mayor(number(arg)?),
        "contiene" => Pred::Contiene(
            arg.as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or("contiene necesita un texto")?
                .to_owned(),
        ),
        "coincide" => Pred::Coincide(compile(arg.as_str().ok_or("coincide necesita una regex")?)?),
        "existe" => Pred::Existe(arg.as_bool().unwrap_or(true)),
        "cambio" => {
            if arg == &Value::Bool(false) {
                return Err("cambio: false no tiene sentido; sacalo".into());
            }
            Pred::Cambio
        }
        "nuevos" => {
            let id = match arg {
                Value::Object(o) => match o.get("id") {
                    Some(Value::String(p)) => Some(Path::parse(p)?),
                    None => None,
                    Some(_) => return Err("nuevos.id es un camino como \"$.link\"".into()),
                },
                Value::Bool(true) => None,
                _ => return Err("nuevos es true o {\"id\": \"$.link\"}".into()),
            };
            Pred::Nuevos { id }
        }
        other => {
            return Err(format!(
                "cuando no conoce «{other}»: usá {}, y, o, no",
                LEAVES.join(", ")
            ));
        }
    };
    Ok(Cuando::Hoja { en, pred })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pr() -> Value {
        json!({
            "id": "pr-7", "version": 1, "origen": "wawi", "creado": "2026-11-23T09:05",
            "vence": "2026-11-30T09:00",
            "vigia": {
                "fuente": { "mcp": { "plugin": "github", "tool": "get_pull_request", "args": { "number": 7 } } },
                "extraer": { "jsonpath": "$.reviewDecision" },
                "cuando": { "igual": "APPROVED" },
                "cada": "10m"
            },
            "al_cumplirse": { "decir": "Aprobaron el PR 7.", "contexto": "repo wawi2" }
        })
    }

    #[test]
    fn a_full_question() {
        let p = parse_pregunta(&pr()).unwrap();
        assert_eq!(p.id, "pr-7");
        assert_eq!(p.vigia.fuente.tipo, "mcp");
        assert_eq!(p.vigia.cada, Some(600));
        assert_eq!(p.vigia.fallos_max, 3);
        assert!(matches!(p.vigia.extraer[0], Extraer::JsonPath(_)));
        assert_eq!(p.que(), "Aprobaron el PR 7.");
    }

    #[test]
    fn composable_predicates_and_a_judge() {
        let v = parse_vigia(&json!({
            "fuente": { "http": { "url": "https://ejemplo.com/rss" } },
            "extraer": "rss",
            "cuando": { "y": [ { "nuevos": { "id": "$.link" } }, { "coincide": "(?i)dólar", "en": "$.titulo" }, { "no": { "contiene": "opinión", "en": "$.titulo" } } ] },
            "juzgar": { "pregunta": "¿Habla de una suba del dólar?" }
        }))
        .unwrap();
        assert!(v.cuando.unwrap().has_nuevos());
        assert!(v.juzgar.is_some());
    }

    #[test]
    fn mistakes_are_explained_in_spanish() {
        let cases: Vec<(Value, &str)> = vec![
            (json!({ "id": "a b" }), "inválido"),
            (json!({ "id": "a" }), "falta vigia"),
            (json!({ "fuente": {} }), "una sola clave"),
            (json!({ "fuente": { "reloj": {} }, "cada": "10s" }), "1m"),
            (
                json!({ "fuente": { "reloj": {} }, "cuando": { "parecido": 1 } }),
                "no conoce",
            ),
            (
                json!({ "fuente": { "reloj": {} }, "cuando": { "igual": 1, "mayor": 2 } }),
                "una sola",
            ),
            (
                json!({ "fuente": { "reloj": {} }, "cuando": { "coincide": "(" } }),
                "no compila",
            ),
            (
                json!({ "fuente": { "reloj": {} }, "extraer": { "jsonpath": "a.b" } }),
                "empezar con $",
            ),
            (
                json!({ "fuente": { "reloj": {} }, "extraer": { "css": "h1" } }),
                "css",
            ),
            (
                json!({ "fuente": { "reloj": {} }, "juzgar": { "pregunta": "¿sí?" } }),
                "nuevos",
            ),
            (
                json!({ "fuente": { "reloj": {} }, "hasta": "mañana" }),
                "hora local",
            ),
            (
                json!({ "fuente": { "reloj": {} }, "extra": 1 }),
                "no conoce",
            ),
            (
                json!({ "fuente": { "reloj": {} }, "cuando": { "menor": "mil" } }),
                "número",
            ),
        ];
        for (value, needle) in cases {
            let error = if value.get("fuente").is_some() {
                parse_vigia(&value).unwrap_err()
            } else {
                parse_pregunta(&value).unwrap_err()
            };
            assert!(error.contains(needle), "{value}: {error}");
        }
        let mut long = pr();
        long["al_cumplirse"]["decir"] = json!("x".repeat(600));
        assert!(parse_pregunta(&long).unwrap_err().contains("largo"));
        let mut v2 = pr();
        v2["version"] = json!(2);
        assert!(parse_pregunta(&v2).unwrap_err().contains("version"));
    }
}
