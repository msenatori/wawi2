use crate::MemoryError;
use crate::date::is_date;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    /// `YYYY-MM-DD`, when it was saved.
    pub date: String,
    pub text: String,
}

/// MEMORY.md: a free-form header, then one `- YYYY-MM-DD: fact` per line,
/// newest first. Blank lines among the facts are ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryDoc {
    pub header: String,
    pub facts: Vec<Fact>,
}

pub const DEFAULT_HEADER: &str = "# Memoria\n\n\
Un dato por línea, el más nuevo arriba, cada uno con la fecha en que se guardó.";

impl Default for MemoryDoc {
    fn default() -> Self {
        Self {
            header: DEFAULT_HEADER.into(),
            facts: Vec::new(),
        }
    }
}

impl MemoryDoc {
    pub fn parse(text: &str) -> Result<Self, MemoryError> {
        let mut header = Vec::new();
        let mut facts = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let is_fact_line = line.starts_with("- ");
            if facts.is_empty() && !is_fact_line {
                header.push(line);
                continue;
            }
            if line.trim().is_empty() {
                continue;
            }
            let fact = parse_fact(line).ok_or(MemoryError::Malformed { line: index + 1 })?;
            facts.push(fact);
        }
        Ok(Self {
            header: header.join("\n").trim().to_owned(),
            facts,
        })
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        if !self.header.is_empty() {
            out.push_str(self.header.trim());
            out.push_str("\n\n");
        }
        for fact in &self.facts {
            out.push_str(&format!("- {}: {}\n", fact.date, fact.text));
        }
        out
    }

    pub fn contains(&self, text: &str) -> bool {
        self.facts.iter().any(|fact| fact.text == text)
    }
}

fn parse_fact(line: &str) -> Option<Fact> {
    let rest = line.strip_prefix("- ")?;
    let (date, text) = rest.split_once(": ")?;
    let text = text.trim();
    (is_date(date) && !text.is_empty()).then(|| Fact {
        date: date.into(),
        text: text.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_renders_back() {
        let text = "# Memoria\n\nAlgo.\n\n- 2026-11-20: Toma mate amargo.\n\n- 2026-11-01: Vive en Rosario.\n";
        let doc = MemoryDoc::parse(text).unwrap();
        assert_eq!(doc.header, "# Memoria\n\nAlgo.");
        assert_eq!(doc.facts.len(), 2);
        assert_eq!(doc.facts[0].text, "Toma mate amargo.");
        assert_eq!(MemoryDoc::parse(&doc.render()).unwrap(), doc);
    }

    #[test]
    fn a_stray_line_after_the_facts_start_is_malformed() {
        let text = "# M\n- 2026-11-20: uno\ntexto suelto\n";
        assert!(matches!(
            MemoryDoc::parse(text),
            Err(MemoryError::Malformed { line: 3 })
        ));
        assert!(MemoryDoc::parse("# M\n- ayer: uno\n").is_err());
        assert!(MemoryDoc::parse("# M\n- 2026-11-20:   \n").is_err());
    }

    #[test]
    fn empty_or_header_only_is_fine() {
        assert_eq!(MemoryDoc::parse("").unwrap().facts, []);
        let doc = MemoryDoc::default();
        assert_eq!(MemoryDoc::parse(&doc.render()).unwrap(), doc);
    }
}
