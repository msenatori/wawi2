//! `vigia.cuando`: composable specifications over the extracted value.
//! Pure: what was seen before comes in as a [`Memoria`] and the new memory
//! comes out; nothing here reads a clock or a file.
//!
//! - Leaves: `igual`, `distinto`, `menor`, `mayor`, `contiene`,
//!   `coincide`, `existe`, plus the stateful `cambio` (against the last
//!   value) and `nuevos` (ids never seen). Each leaf may say `en`, a path
//!   into the value (or into each item, after `nuevos`).
//! - Combinators: `y`, `o`, `no`.
//! - After `nuevos` inside a `y`, the rest of the `y` filters the new items
//!   one by one; it holds when some item is left.
//! - The first look only learns: `cambio` and `nuevos` are false then.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::extraer::as_text;
use crate::jsonpath::Path;
use crate::spec::{Cuando, Pred};
use crate::texto::{normalize, parse_number};

/// Seen ids kept per question; the oldest go first.
pub const MAX_SEEN: usize = 2000;
/// A last value bigger than this (as JSON) is kept as a fingerprint.
const MAX_LAST: usize = 16 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Memoria {
    /// The last extracted value. `None` until the first good look.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ultimo: Option<Value>,
    /// Ids already seen by `nuevos`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vistos: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Resultado {
    pub cumple: bool,
    /// The items that made it true, when `nuevos` was involved.
    pub items: Option<Vec<Value>>,
}

struct Ctx<'a> {
    antes: &'a Memoria,
    first: bool,
    ids: Vec<String>,
}

/// Evaluates `cuando` (or, without one, "the value is truthy") against the
/// value, and gives the memory to keep for the next look.
pub fn evaluar(
    cuando: Option<&Cuando>,
    valor: &Value,
    antes: &Memoria,
) -> Result<(Resultado, Memoria), String> {
    let mut ctx = Ctx {
        antes,
        first: antes.ultimo.is_none(),
        ids: Vec::new(),
    };
    let resultado = match cuando {
        None => Resultado {
            cumple: truthy(valor),
            items: None,
        },
        Some(c) => {
            let (cumple, items) = eval(c, valor, None, &mut ctx)?;
            Resultado { cumple, items }
        }
    };
    let mut vistos = antes.vistos.clone();
    for id in ctx.ids {
        if !vistos.contains(&id) {
            vistos.push(id);
        }
    }
    if vistos.len() > MAX_SEEN {
        let extra = vistos.len() - MAX_SEEN;
        vistos.drain(..extra);
    }
    Ok((
        resultado,
        Memoria {
            ultimo: Some(keepable(valor)),
            vistos,
        },
    ))
}

fn keepable(valor: &Value) -> Value {
    let text = valor.to_string();
    if text.len() <= MAX_LAST {
        return valor.clone();
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    Value::String(format!("huella:{:016x}", hasher.finish()))
}

fn at(en: &Option<Path>, value: &Value) -> Value {
    match en {
        Some(path) => path.get(value),
        None => value.clone(),
    }
}

type Outcome = (bool, Option<Vec<Value>>);

fn eval(
    c: &Cuando,
    valor: &Value,
    focus: Option<Vec<Value>>,
    ctx: &mut Ctx<'_>,
) -> Result<Outcome, String> {
    if let Some(items) = focus {
        let mut kept = Vec::new();
        for item in items {
            if keeps(c, &item)? {
                kept.push(item);
            }
        }
        return Ok((!kept.is_empty(), Some(kept)));
    }
    match c {
        Cuando::Hoja {
            en,
            pred: Pred::Nuevos { id },
        } => {
            let subject = at(en, valor);
            let list = subject.as_array().ok_or_else(|| {
                format!("nuevos necesita una lista y encontró {}", kind(&subject))
            })?;
            let mut fresh = Vec::new();
            for item in list {
                let key = item_id(id, item);
                if !ctx.first && !ctx.antes.vistos.contains(&key) && !ctx.ids.contains(&key) {
                    fresh.push(item.clone());
                }
                ctx.ids.push(key);
            }
            Ok((!fresh.is_empty(), Some(fresh)))
        }
        Cuando::Hoja {
            en,
            pred: Pred::Cambio,
        } => {
            if ctx.first {
                return Ok((false, None));
            }
            let before = ctx.antes.ultimo.as_ref().map(|u| at(en, u));
            Ok((before.as_ref() != Some(&at(en, valor)), None))
        }
        Cuando::Hoja { en, pred } => Ok((test(pred, &at(en, valor))?, None)),
        Cuando::Y(list) => {
            let mut all = true;
            let mut focus: Option<Vec<Value>> = None;
            for child in list {
                let (ok, items) = eval(child, valor, focus.clone(), ctx)?;
                all &= ok;
                if items.is_some() {
                    focus = items;
                }
            }
            let all = all && focus.as_ref().is_none_or(|f| !f.is_empty());
            Ok((all, focus))
        }
        Cuando::O(list) => {
            let mut any = false;
            let mut items: Option<Vec<Value>> = None;
            for child in list {
                let (ok, found) = eval(child, valor, None, ctx)?;
                any |= ok;
                if ok && let Some(found) = found {
                    let into = items.get_or_insert_with(Vec::new);
                    for item in found {
                        if !into.contains(&item) {
                            into.push(item);
                        }
                    }
                }
            }
            Ok((any, items))
        }
        Cuando::No(inner) => {
            let (ok, _) = eval(inner, valor, None, ctx)?;
            Ok((!ok, None))
        }
    }
}

/// Whether one item (after `nuevos`) passes `c`.
fn keeps(c: &Cuando, item: &Value) -> Result<bool, String> {
    match c {
        Cuando::Hoja {
            pred: Pred::Nuevos { .. } | Pred::Cambio,
            ..
        } => Err("cambio y nuevos no van después de nuevos".into()),
        Cuando::Hoja { en, pred } => test(pred, &at(en, item)),
        Cuando::Y(list) => {
            for child in list {
                if !keeps(child, item)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Cuando::O(list) => {
            for child in list {
                if keeps(child, item)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Cuando::No(inner) => Ok(!keeps(inner, item)?),
    }
}

fn item_id(id: &Option<Path>, item: &Value) -> String {
    if let Some(path) = id {
        return as_text(&path.get(item));
    }
    for field in ["id", "link", "url"] {
        match item.get(field) {
            Some(Value::String(s)) if !s.is_empty() => return s.clone(),
            Some(Value::Number(n)) => return n.to_string(),
            _ => {}
        }
    }
    as_text(item)
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "nada",
        Value::Bool(_) => "un sí/no",
        Value::Number(_) => "un número",
        Value::String(_) => "un texto",
        Value::Array(_) => "una lista",
        Value::Object(_) => "un objeto",
    }
}

/// Not null, not false, not 0, not an empty text, list or object.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn number(value: &Value) -> Result<f64, String> {
    match value {
        Value::Number(n) => n.as_f64().ok_or_else(|| "número fuera de rango".into()),
        Value::String(s) => parse_number(s)
            .ok_or_else(|| format!("«{}» no es un número", crate::texto::truncate(s, 40))),
        Value::Null => Err("no encontré el valor (el camino no dio nada)".into()),
        other => Err(format!("esperaba un número y encontré {}", kind(other))),
    }
}

/// Equal, loosely: numbers by value (also when written as text), texts
/// trimmed and without caring about case.
pub fn same(a: &Value, b: &Value) -> bool {
    if a == b {
        return true;
    }
    match (a, b) {
        (Value::String(x), Value::String(y)) => x.trim().to_lowercase() == y.trim().to_lowercase(),
        (Value::Number(_), Value::String(_)) | (Value::String(_), Value::Number(_)) => {
            matches!((number(a), number(b)), (Ok(x), Ok(y)) if x == y)
        }
        (Value::Bool(x), Value::String(y)) | (Value::String(y), Value::Bool(x)) => y
            .trim()
            .eq_ignore_ascii_case(if *x { "true" } else { "false" }),
        _ => false,
    }
}

fn texts(value: &Value) -> Vec<String> {
    match value {
        Value::Array(list) => list.iter().map(as_text).collect(),
        Value::Null => Vec::new(),
        other => vec![as_text(other)],
    }
}

fn test(pred: &Pred, subject: &Value) -> Result<bool, String> {
    Ok(match pred {
        Pred::Igual(want) => same(subject, want),
        Pred::Distinto(want) => !subject.is_null() && !same(subject, want),
        Pred::Menor(n) => number(subject)? < *n,
        Pred::Mayor(n) => number(subject)? > *n,
        Pred::Contiene(needle) => {
            let needle = normalize(needle);
            texts(subject)
                .iter()
                .any(|t| format!(" {} ", normalize(t)).contains(&format!(" {needle} ")))
        }
        Pred::Coincide(re) => texts(subject).iter().any(|t| re.is_match(t)),
        Pred::Existe(want) => truthy(subject) == *want,
        Pred::Cambio | Pred::Nuevos { .. } => unreachable!("handled by eval"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::parse_cuando;
    use serde_json::json;

    fn run(cuando: Value, valor: Value, antes: &Memoria) -> (Resultado, Memoria) {
        evaluar(Some(&parse_cuando(&cuando).unwrap()), &valor, antes).unwrap()
    }

    fn holds(cuando: Value, valor: Value) -> bool {
        run(cuando, valor, &Memoria::default()).0.cumple
    }

    #[test]
    fn comparisons_and_text() {
        assert!(holds(json!({ "igual": "APPROVED" }), json!("approved ")));
        assert!(!holds(
            json!({ "igual": "APPROVED" }),
            json!("CHANGES_REQUESTED")
        ));
        assert!(holds(json!({ "igual": 3 }), json!("3")));
        assert!(holds(json!({ "distinto": "Todo" }), json!("Done")));
        assert!(!holds(json!({ "distinto": "Todo" }), Value::Null));
        assert!(holds(json!({ "menor": 1000 }), json!("$ 980,50")));
        assert!(!holds(json!({ "menor": 1000 }), json!(1000)));
        assert!(holds(json!({ "mayor": "1.000" }), json!(1001)));
        assert!(holds(
            json!({ "contiene": "dolar" }),
            json!("Sube el DÓLAR blue")
        ));
        assert!(!holds(json!({ "contiene": "dolar" }), json!("dolares")));
        assert!(holds(
            json!({ "coincide": "^PR-\\d+$" }),
            json!(["x", "PR-7"])
        ));
        assert!(holds(json!({ "existe": true }), json!({ "a": 1 })));
        assert!(holds(json!({ "existe": false }), json!("")));
        assert!(holds(
            json!({ "igual": "Done", "en": "$.state.name" }),
            json!({ "state": { "name": "Done" } })
        ));
        assert!(holds(
            json!({ "y": [{ "mayor": 1 }, { "menor": 3 }] }),
            json!(2)
        ));
        assert!(holds(
            json!({ "o": [{ "igual": 1 }, { "igual": 2 }] }),
            json!(2)
        ));
        assert!(holds(json!({ "no": { "igual": 1 } }), json!(2)));
        assert!(
            evaluar(None, &json!("algo"), &Memoria::default())
                .unwrap()
                .0
                .cumple
        );
        assert!(
            !evaluar(None, &json!(false), &Memoria::default())
                .unwrap()
                .0
                .cumple
        );
    }

    #[test]
    fn a_number_that_is_not_there_is_an_error() {
        let c = parse_cuando(&json!({ "menor": 1000 })).unwrap();
        let error = evaluar(Some(&c), &Value::Null, &Memoria::default()).unwrap_err();
        assert!(error.contains("no encontré"));
        assert!(evaluar(Some(&c), &json!("hola"), &Memoria::default()).is_err());
    }

    #[test]
    fn cambio_learns_first_then_compares() {
        let c = json!({ "cambio": true, "en": "$.status" });
        let (r, m) = run(c.clone(), json!({ "status": "Todo" }), &Memoria::default());
        assert!(!r.cumple);
        let (r, m) = run(c.clone(), json!({ "status": "Todo", "x": 1 }), &m);
        assert!(!r.cumple);
        let (r, _) = run(c, json!({ "status": "Done" }), &m);
        assert!(r.cumple);
    }

    #[test]
    fn nuevos_then_filters_item_by_item() {
        let c = json!({ "y": [ { "nuevos": { "id": "$.link" } }, { "coincide": "(?i)d[oó]lar", "en": "$.titulo" } ] });
        let feed = |items: &[(&str, &str)]| {
            Value::Array(
                items
                    .iter()
                    .map(|(l, t)| json!({ "link": l, "titulo": t }))
                    .collect(),
            )
        };
        let (r, m) = run(
            c.clone(),
            feed(&[("1", "Sube el dólar")]),
            &Memoria::default(),
        );
        assert!(!r.cumple, "the first look only learns");
        assert_eq!(m.vistos, ["1"]);
        let (r, m) = run(
            c.clone(),
            feed(&[("1", "Sube el dólar"), ("2", "Fútbol")]),
            &m,
        );
        assert!(!r.cumple);
        assert_eq!(r.items, Some(vec![]));
        let (r, m) = run(
            c.clone(),
            feed(&[
                ("1", "x"),
                ("2", "Fútbol"),
                ("3", "Baja el DOLAR"),
                ("4", "Lluvia"),
            ]),
            &m,
        );
        assert!(r.cumple);
        assert_eq!(
            r.items.unwrap(),
            vec![json!({ "link": "3", "titulo": "Baja el DOLAR" })]
        );
        assert_eq!(m.vistos, ["1", "2", "3", "4"]);
        let (r, _) = run(c, feed(&[("3", "Baja el DOLAR")]), &m);
        assert!(!r.cumple, "seen ids do not come back");
    }

    #[test]
    fn nuevos_needs_a_list_and_no_state_after_it() {
        let c = parse_cuando(&json!({ "nuevos": true })).unwrap();
        assert!(evaluar(Some(&c), &json!("x"), &Memoria::default()).is_err());
        let c = parse_cuando(&json!({ "y": [{ "nuevos": true }, { "cambio": true }] })).unwrap();
        let seen = Memoria {
            ultimo: Some(json!([])),
            vistos: vec![],
        };
        assert!(evaluar(Some(&c), &json!([{ "id": "a" }]), &seen).is_err());
    }
}
