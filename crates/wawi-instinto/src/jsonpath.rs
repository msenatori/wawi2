//! A small JSONPath subset, enough to pick a field out of an API answer:
//!
//! - `$` the whole value; `.name` or `['name']` a field; `[2]` an index
//!   (`[-1]` the last); `[*]` or `.*` every element; `..name` the field at
//!   any depth.
//!
//! No filters, no slices, no scripts. A path without `*` or `..` gives one
//! value (or nothing); with them, a list.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
enum Step {
    Field(String),
    Index(i64),
    All,
    Deep(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    steps: Vec<Step>,
    text: String,
}

impl Path {
    pub fn parse(text: &str) -> Result<Self, String> {
        let bad = |why: &str| format!("el camino «{text}» no sirve: {why}");
        let mut rest = text.trim();
        rest = rest
            .strip_prefix('$')
            .ok_or_else(|| bad("tiene que empezar con $"))?;
        let mut steps = Vec::new();
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("..") {
                let (name, tail) = take_name(after);
                if name.is_empty() {
                    return Err(bad("falta el nombre después de .."));
                }
                steps.push(Step::Deep(name.to_owned()));
                rest = tail;
            } else if let Some(after) = rest.strip_prefix('.') {
                if let Some(tail) = after.strip_prefix('*') {
                    steps.push(Step::All);
                    rest = tail;
                    continue;
                }
                let (name, tail) = take_name(after);
                if name.is_empty() {
                    return Err(bad("falta el nombre después de ."));
                }
                steps.push(Step::Field(name.to_owned()));
                rest = tail;
            } else if let Some(after) = rest.strip_prefix('[') {
                let end = after.find(']').ok_or_else(|| bad("falta cerrar ]"))?;
                let inside = after[..end].trim();
                rest = &after[end + 1..];
                if inside == "*" {
                    steps.push(Step::All);
                } else if let Some(quoted) = inside
                    .strip_prefix('\'')
                    .and_then(|s| s.strip_suffix('\''))
                    .or_else(|| inside.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
                {
                    steps.push(Step::Field(quoted.to_owned()));
                } else if let Ok(index) = inside.parse::<i64>() {
                    steps.push(Step::Index(index));
                } else {
                    return Err(bad("entre [] va un número, * o un 'nombre' (sin filtros)"));
                }
            } else {
                return Err(bad("después de $ va . o ["));
            }
        }
        Ok(Self {
            steps,
            text: text.trim().to_owned(),
        })
    }

    /// Whether the path can give more than one value.
    pub fn is_multi(&self) -> bool {
        self.steps
            .iter()
            .any(|s| matches!(s, Step::All | Step::Deep(_)))
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Every value the path reaches.
    pub fn all(&self, value: &Value) -> Vec<Value> {
        let mut current = vec![value.clone()];
        for step in &self.steps {
            let mut next = Vec::new();
            for item in &current {
                match step {
                    Step::Field(name) => {
                        if let Some(found) = item.get(name) {
                            next.push(found.clone());
                        }
                    }
                    Step::Index(index) => {
                        if let Some(list) = item.as_array() {
                            let at = if *index < 0 {
                                list.len() as i64 + index
                            } else {
                                *index
                            };
                            if at >= 0
                                && let Some(found) = list.get(at as usize)
                            {
                                next.push(found.clone());
                            }
                        }
                    }
                    Step::All => match item {
                        Value::Array(list) => next.extend(list.iter().cloned()),
                        Value::Object(map) => next.extend(map.values().cloned()),
                        _ => {}
                    },
                    Step::Deep(name) => deep(item, name, &mut next),
                }
            }
            current = next;
        }
        current
    }

    /// The value at the path: one value (or `null`) for a plain path, a
    /// list for a path with `*` or `..`.
    pub fn get(&self, value: &Value) -> Value {
        let mut found = self.all(value);
        if self.is_multi() {
            Value::Array(found)
        } else {
            found.pop().unwrap_or(Value::Null)
        }
    }
}

fn take_name(text: &str) -> (&str, &str) {
    let end = text.find(['.', '[']).unwrap_or(text.len());
    (&text[..end], &text[end..])
}

fn deep(value: &Value, name: &str, out: &mut Vec<Value>) {
    match value {
        Value::Object(map) => {
            if let Some(found) = map.get(name) {
                out.push(found.clone());
            }
            for child in map.values() {
                deep(child, name, out);
            }
        }
        Value::Array(list) => {
            for child in list {
                deep(child, name, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_subset() {
        let v = json!({
            "reviewDecision": "APPROVED",
            "state": { "name": "In Progress" },
            "items": [ { "id": 1, "t": "a" }, { "id": 2, "t": "b" } ],
            "odd key": 3
        });
        let get = |p: &str| Path::parse(p).unwrap().get(&v);
        assert_eq!(get("$"), v);
        assert_eq!(get("$.reviewDecision"), "APPROVED");
        assert_eq!(get("$.state.name"), "In Progress");
        assert_eq!(get("$['odd key']"), 3);
        assert_eq!(get("$.items[1].t"), "b");
        assert_eq!(get("$.items[-1].id"), 2);
        assert_eq!(get("$.items[*].id"), json!([1, 2]));
        assert_eq!(get("$.items.*.t"), json!(["a", "b"]));
        assert_eq!(get("$..name"), json!(["In Progress"]));
        assert_eq!(get("$.nope"), Value::Null);
        assert_eq!(get("$.items[9]"), Value::Null);
        for bad in ["reviewDecision", "$.", "$[?(@.a)]", "$.a[", "$x"] {
            assert!(Path::parse(bad).is_err(), "{bad}");
        }
    }
}
