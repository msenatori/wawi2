//! `vigia.extraer`: filters that narrow what the source gave. Each one
//! takes a JSON value and gives another; a list of them runs in order.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::spec::Extraer;
use crate::texto::truncate;

/// The most RSS items kept from one read.
const MAX_ITEMS: usize = 100;

/// Text as JSON when it is JSON, else as a string.
pub fn as_json(text: &str) -> Value {
    let trimmed = text.trim();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(value) = serde_json::from_str(trimmed)
    {
        return value;
    }
    Value::String(text.to_owned())
}

/// The text of a value: a string as is, anything else as JSON.
pub fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub fn apply(steps: &[Extraer], value: Value) -> Value {
    steps
        .iter()
        .fold(value, |value, step| apply_one(step, value))
}

fn apply_one(step: &Extraer, value: Value) -> Value {
    match step {
        Extraer::JsonPath(path) => {
            let value = match value {
                Value::String(text) => as_json(&text),
                other => other,
            };
            path.get(&value)
        }
        Extraer::Regex { re, todos } => {
            let text = as_text(&value);
            let pick = |caps: regex::Captures<'_>| -> Value {
                let m = caps.get(1).or_else(|| caps.get(0));
                m.map_or(Value::Null, |m| Value::String(m.as_str().to_owned()))
            };
            if *todos {
                Value::Array(re.captures_iter(&text).map(pick).collect())
            } else {
                re.captures(&text).map_or(Value::Null, pick)
            }
        }
        Extraer::Rss => Value::Array(rss_items(&as_text(&value))),
    }
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("a fixed regex"))
}

/// The items of an RSS or Atom feed, as `{id, titulo, link, fecha,
/// resumen}`. A cheap scanner, not an XML parser: it reads the usual tags
/// and ignores the rest.
pub fn rss_items(xml: &str) -> Vec<Value> {
    static ITEM: OnceLock<Regex> = OnceLock::new();
    let item = re(&ITEM, r"(?s)<(item|entry)\b[^>]*>(.*?)</(?:item|entry)>");
    item.captures_iter(xml)
        .take(MAX_ITEMS)
        .map(|caps| {
            let body = caps.get(2).map_or("", |m| m.as_str());
            let titulo = tag(body, "title").unwrap_or_default();
            let link = tag(body, "link")
                .filter(|l| !l.is_empty())
                .or_else(|| atom_link(body))
                .unwrap_or_default();
            let id = tag(body, "guid")
                .or_else(|| tag(body, "id"))
                .filter(|i| !i.is_empty())
                .unwrap_or_else(|| {
                    if link.is_empty() {
                        titulo.clone()
                    } else {
                        link.clone()
                    }
                });
            let fecha = tag(body, "pubDate")
                .or_else(|| tag(body, "published"))
                .or_else(|| tag(body, "updated"))
                .unwrap_or_default();
            let resumen = tag(body, "description")
                .or_else(|| tag(body, "summary"))
                .map(|s| truncate(&strip_tags(&s), 300))
                .unwrap_or_default();
            json!({ "id": id, "titulo": titulo, "link": link, "fecha": fecha, "resumen": resumen })
        })
        .collect()
}

fn tag(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}");
    let mut from = 0;
    while let Some(at) = body[from..].find(&open) {
        let start = from + at;
        let after = &body[start + open.len()..];
        // `<link>` but not `<linkedin>`.
        if !after.starts_with(['>', ' ', '\t', '\n', '/']) {
            from = start + open.len();
            continue;
        }
        let gt = after.find('>')?;
        if after[..gt].ends_with('/') {
            return Some(String::new());
        }
        let inner = &after[gt + 1..];
        let close = format!("</{name}>");
        let end = inner.find(&close)?;
        return Some(decode(inner[..end].trim()));
    }
    None
}

fn atom_link(body: &str) -> Option<String> {
    static LINK: OnceLock<Regex> = OnceLock::new();
    let link = re(&LINK, r#"<link\b[^>]*\bhref\s*=\s*["']([^"']+)["']"#);
    link.captures(body)
        .and_then(|c| c.get(1))
        .map(|m| decode(m.as_str()))
}

fn decode(text: &str) -> String {
    let text = text
        .strip_prefix("<![CDATA[")
        .and_then(|t| t.strip_suffix("]]>"))
        .unwrap_or(text);
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn strip_tags(text: &str) -> String {
    static TAGS: OnceLock<Regex> = OnceLock::new();
    let tags = re(&TAGS, r"<[^>]*>");
    let plain = tags.replace_all(text, " ");
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::parse_vigia;

    fn steps(extraer: Value) -> Vec<Extraer> {
        parse_vigia(&json!({ "fuente": { "reloj": {} }, "extraer": extraer }))
            .unwrap()
            .extraer
    }

    #[test]
    fn jsonpath_reads_json_text_too() {
        let s = steps(json!({ "jsonpath": "$.venta" }));
        assert_eq!(apply(&s, Value::String(r#"{"venta": 980}"#.into())), 980);
        assert_eq!(apply(&s, json!({ "venta": 1 })), 1);
    }

    #[test]
    fn regex_first_group_or_all() {
        let one = steps(json!({ "regex": "Venta: \\$ ?([0-9.,]+)" }));
        assert_eq!(
            apply(&one, json!("Compra $950 Venta: $ 1.020,50")),
            "1.020,50"
        );
        assert_eq!(apply(&one, json!("nada")), Value::Null);
        let all = steps(json!({ "regex": "#(\\d+)", "todos": true }));
        assert_eq!(apply(&all, json!("#1 y #22")), json!(["1", "22"]));
        let chain = steps(json!([{ "jsonpath": "$.html" }, { "regex": "<h1>(.*?)</h1>" }]));
        assert_eq!(apply(&chain, json!({ "html": "<h1>Hola</h1>" })), "Hola");
    }

    #[test]
    fn rss_and_atom() {
        let rss = r#"<rss><channel><title>Diario</title>
            <item><title><![CDATA[Sube el dólar &amp; más]]></title><link>https://d.com/1</link>
            <guid isPermaLink="false">n-1</guid><pubDate>Mon, 23 Nov 2026 10:00:00 -0300</pubDate>
            <description>&lt;p&gt;Resumen &lt;b&gt;corto&lt;/b&gt;&lt;/p&gt;</description></item>
            <item><title>Otra</title><link>https://d.com/2</link></item>
            </channel></rss>"#;
        let items = rss_items(rss);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["titulo"], "Sube el dólar & más");
        assert_eq!(items[0]["id"], "n-1");
        assert_eq!(items[0]["resumen"], "Resumen corto");
        assert_eq!(items[1]["id"], "https://d.com/2");
        let atom = r#"<feed><entry><title>A</title><link rel="alternate" href="https://a.com/x"/><id>tag:a,1</id><updated>2026-11-23</updated></entry></feed>"#;
        let items = rss_items(atom);
        assert_eq!(items[0]["link"], "https://a.com/x");
        assert_eq!(items[0]["id"], "tag:a,1");
    }
}
