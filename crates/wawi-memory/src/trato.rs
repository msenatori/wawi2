//! `<home>/trato.md`: how to talk to THIS owner. Style, not facts (facts
//! live in MEMORY.md). The «carácter» plugin learns it and writes it; the
//! owner can read it, edit it or delete it. The brain sees it next to the
//! facts, so a change never touches the system prompt.
//!
//! ```text
//! # Trato
//!
//! (a free header)
//!
//! - Respuestas bien cortas: una frase si alcanza.
//! - Los chistes de doble sentido le causan gracia.
//! ```

use crate::secrets::looks_like_secret;

pub const TRATO_FILE: &str = "trato.md";
/// At most this many rules...
pub const MAX_RULES: usize = 12;
/// ...and this many characters, all rules together.
pub const MAX_CHARS: usize = 600;
/// The longest single rule.
pub const MAX_RULE_CHARS: usize = 120;

pub const TRATO_HEADER: &str = "# Trato\n\n\
Cómo le gusta a tu dueño que le hables. Lo aprende el plugin carácter con lo \
que él dice; podés editarlo o borrarlo cuando quieras. Una regla por línea.";

/// The heading of the trato inside the brain's memory block.
pub const TRATO_HEADING: &str = "# Cómo le gusta que le hables";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trato {
    pub rules: Vec<String>,
}

impl Trato {
    /// Every `- rule` line, in order. Anything else (the header, notes) is
    /// ignored. Secret-looking or overlong lines are skipped, and the caps
    /// hold even for a file edited by hand.
    pub fn parse(text: &str) -> Self {
        let mut trato = Self::default();
        for line in text.lines() {
            let Some(rule) = line.trim().strip_prefix("- ") else {
                continue;
            };
            let rule = rule.trim();
            if rule.is_empty()
                || rule.chars().count() > MAX_RULE_CHARS
                || looks_like_secret(rule)
                || trato.rules.iter().any(|known| known == rule)
            {
                continue;
            }
            if trato.rules.len() == MAX_RULES || trato.chars() + rule.chars().count() > MAX_CHARS {
                break;
            }
            trato.rules.push(rule.to_owned());
        }
        trato
    }

    pub fn render(&self) -> String {
        let mut out = format!("{TRATO_HEADER}\n\n");
        for rule in &self.rules {
            out.push_str(&format!("- {rule}\n"));
        }
        out
    }

    /// Characters of all rules together.
    pub fn chars(&self) -> usize {
        self.rules.iter().map(|rule| rule.chars().count()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The section for the brain's memory block, or empty.
    pub fn context_section(&self) -> String {
        if self.rules.is_empty() {
            return String::new();
        }
        let mut out = format!(
            "\n{TRATO_HEADING}\nLo aprendiste de tu dueño. Es estilo, no datos: seguilo sin nombrarlo. \
             Si te pregunta cómo lo tratás, contáselo.\n"
        );
        for rule in &self.rules {
            out.push_str(&format!("- {rule}\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rules_and_renders_them_back() {
        let trato = Trato::parse(
            "# Trato\n\nnota libre\n\n- Corto.\n-   Sin chistes a la mañana.  \n- Corto.\n* otra cosa\n",
        );
        assert_eq!(trato.rules, ["Corto.", "Sin chistes a la mañana."]);
        assert_eq!(Trato::parse(&trato.render()), trato);
        assert!(trato.context_section().contains("- Corto."));
        assert_eq!(Trato::default().context_section(), "");
    }

    #[test]
    fn the_caps_hold_for_a_file_edited_by_hand() {
        let many: String = (0..20).map(|i| format!("- regla {i}\n")).collect();
        assert_eq!(Trato::parse(&many).rules.len(), MAX_RULES);
        let long: String = (0..10)
            .map(|i| format!("- {i} {}\n", "x".repeat(100)))
            .collect();
        assert!(Trato::parse(&long).chars() <= MAX_CHARS);
        let text = format!("- {}\n- mi password es hunter2\n- bien", "y".repeat(200));
        assert_eq!(Trato::parse(&text).rules, ["bien"]);
    }
}
