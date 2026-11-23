//! The guard for the memory tidy-up. A model will propose a whole new
//! MEMORY.md (and topic files); the proposal replaces the current one only if
//! it passes here. The checks keep a bad run from wiping or flooding memory.

use std::collections::HashSet;
use std::fmt;

use crate::MemoryError;
use crate::doc::MemoryDoc;
use crate::home::is_safe_topic_name;
use crate::secrets::looks_like_secret;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Facts MEMORY.md may hold after the tidy-up.
    pub max_facts: usize,
    /// Facts a single tidy-up may add.
    pub max_new_facts: usize,
    /// A single tidy-up may remove at most `1 / remove_divisor` of the current
    /// facts, or more when that is what it takes to get back under the cap.
    pub remove_divisor: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_facts: 60,
            max_new_facts: 6,
            remove_divisor: 5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicFile {
    /// File stem: the file is `memory/<name>.md`.
    pub name: String,
    pub content: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TidyProposal {
    pub memory: String,
    pub topics: Vec<TopicFile>,
}

/// Why a proposal was refused. Secret refusals say where, never what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TidyRejection {
    CurrentMalformed {
        line: usize,
    },
    Malformed {
        line: usize,
    },
    HeaderChanged,
    DuplicateFact,
    TooManyFacts {
        count: usize,
        max: usize,
    },
    TooManyNew {
        count: usize,
        max: usize,
    },
    TooManyRemoved {
        count: usize,
        max: usize,
    },
    /// `where_` is "MEMORY.md" or the topic name.
    Secret {
        where_: String,
    },
    UnsafeTopicName {
        name: String,
    },
}

impl fmt::Display for TidyRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentMalformed { line } => {
                write!(f, "current MEMORY.md line {line} is not a fact")
            }
            Self::Malformed { line } => write!(f, "proposed MEMORY.md line {line} is not a fact"),
            Self::HeaderChanged => write!(f, "the MEMORY.md header must be kept"),
            Self::DuplicateFact => write!(f, "a fact appears twice"),
            Self::TooManyFacts { count, max } => write!(f, "{count} facts, at most {max}"),
            Self::TooManyNew { count, max } => write!(f, "{count} new facts, at most {max}"),
            Self::TooManyRemoved { count, max } => {
                write!(f, "{count} facts removed, at most {max}")
            }
            Self::Secret { where_ } => write!(f, "secret-looking text in {where_}"),
            Self::UnsafeTopicName { name } => write!(f, "unsafe topic name {name:?}"),
        }
    }
}

impl std::error::Error for TidyRejection {}

/// Accepts `proposal` as the replacement for `current_memory` only if every
/// rule holds. Facts are compared by text: a reworded fact counts as one
/// removed and one new.
pub fn validate_tidy(
    current_memory: &str,
    proposal: &TidyProposal,
    limits: &Limits,
) -> Result<(), TidyRejection> {
    let current = MemoryDoc::parse(current_memory).map_err(|error| match error {
        MemoryError::Malformed { line } => TidyRejection::CurrentMalformed { line },
        _ => TidyRejection::CurrentMalformed { line: 0 },
    })?;
    let proposed = MemoryDoc::parse(&proposal.memory).map_err(|error| match error {
        MemoryError::Malformed { line } => TidyRejection::Malformed { line },
        _ => TidyRejection::Malformed { line: 0 },
    })?;

    if proposed.header != current.header {
        return Err(TidyRejection::HeaderChanged);
    }
    let proposed_texts: HashSet<&str> = proposed.facts.iter().map(|f| f.text.as_str()).collect();
    if proposed_texts.len() != proposed.facts.len() {
        return Err(TidyRejection::DuplicateFact);
    }
    let current_texts: HashSet<&str> = current.facts.iter().map(|f| f.text.as_str()).collect();

    let count = proposed.facts.len();
    if count > limits.max_facts {
        return Err(TidyRejection::TooManyFacts {
            count,
            max: limits.max_facts,
        });
    }
    let added = proposed_texts.difference(&current_texts).count();
    if added > limits.max_new_facts {
        return Err(TidyRejection::TooManyNew {
            count: added,
            max: limits.max_new_facts,
        });
    }
    let removed = current_texts.difference(&proposed_texts).count();
    let held = current_texts.len();
    let max_removed =
        (held / limits.remove_divisor.max(1)).max(held.saturating_sub(limits.max_facts));
    if removed > max_removed {
        return Err(TidyRejection::TooManyRemoved {
            count: removed,
            max: max_removed,
        });
    }

    if looks_like_secret(&proposal.memory) {
        return Err(TidyRejection::Secret {
            where_: "MEMORY.md".into(),
        });
    }
    for topic in &proposal.topics {
        if !is_safe_topic_name(&topic.name) {
            return Err(TidyRejection::UnsafeTopicName {
                name: topic.name.clone(),
            });
        }
        if looks_like_secret(&topic.content) {
            return Err(TidyRejection::Secret {
                where_: topic.name.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Fact;

    const HEADER: &str = "# Memoria\n\nUn dato por línea.";

    fn memory(texts: &[String]) -> String {
        MemoryDoc {
            header: HEADER.into(),
            facts: texts
                .iter()
                .map(|text| Fact {
                    date: "2026-11-20".into(),
                    text: text.clone(),
                })
                .collect(),
        }
        .render()
    }

    fn facts(range: std::ops::Range<usize>) -> Vec<String> {
        range.map(|i| format!("Dato número {i}.")).collect()
    }

    fn proposal(texts: &[String]) -> TidyProposal {
        TidyProposal {
            memory: memory(texts),
            topics: Vec::new(),
        }
    }

    fn check(current: &[String], proposed: &TidyProposal) -> Result<(), TidyRejection> {
        validate_tidy(&memory(current), proposed, &Limits::default())
    }

    #[test]
    fn accepts_a_modest_tidy_up() {
        let current = facts(0..20);
        // Drops 4 (1/5 of 20), adds 6, reorders.
        let mut next = facts(4..20);
        next.reverse();
        next.extend(facts(100..106));
        assert_eq!(check(&current, &proposal(&next)), Ok(()));
        assert_eq!(check(&current, &proposal(&current)), Ok(()));
    }

    #[test]
    fn accepts_a_reworded_header_only_if_identical() {
        let current = facts(0..3);
        let mut next = proposal(&current);
        next.memory = next.memory.replace("Un dato por línea.", "Otra cosa.");
        assert_eq!(check(&current, &next), Err(TidyRejection::HeaderChanged));
    }

    #[test]
    fn refuses_more_than_the_fact_cap() {
        let current = facts(0..58);
        let next = facts(0..61);
        assert_eq!(
            check(&current, &proposal(&next)),
            Err(TidyRejection::TooManyFacts { count: 61, max: 60 })
        );
    }

    #[test]
    fn refuses_more_than_six_new_facts() {
        let current = facts(0..10);
        let mut next = current.clone();
        next.extend(facts(100..107));
        assert_eq!(
            check(&current, &proposal(&next)),
            Err(TidyRejection::TooManyNew { count: 7, max: 6 })
        );
    }

    #[test]
    fn refuses_removing_more_than_a_fifth() {
        let current = facts(0..20);
        assert_eq!(
            check(&current, &proposal(&facts(5..20))),
            Err(TidyRejection::TooManyRemoved { count: 5, max: 4 })
        );
        // Small memories cannot lose anything at once.
        assert_eq!(
            check(&facts(0..4), &proposal(&facts(1..4))),
            Err(TidyRejection::TooManyRemoved { count: 1, max: 0 })
        );
        // Wiping everything is the case this exists for.
        assert!(check(&current, &proposal(&[])).is_err());
    }

    #[test]
    fn over_the_cap_it_may_remove_what_it_takes_to_get_back_under() {
        let current = facts(0..80);
        // 1/5 of 80 is 16; getting to 60 takes 20.
        assert_eq!(check(&current, &proposal(&facts(20..80))), Ok(()));
        assert_eq!(
            check(&current, &proposal(&facts(21..80))),
            Err(TidyRejection::TooManyRemoved { count: 21, max: 20 })
        );
    }

    #[test]
    fn refuses_secrets_without_echoing_them() {
        let current = facts(0..5);
        let mut next = current.clone();
        next.push("La contraseña del banco es 1234.".into());
        let rejection = check(&current, &proposal(&next)).unwrap_err();
        assert_eq!(
            rejection,
            TidyRejection::Secret {
                where_: "MEMORY.md".into()
            }
        );
        assert!(!rejection.to_string().contains("1234"));

        let mut with_topic = proposal(&current);
        with_topic.topics.push(TopicFile {
            name: "trabajo".into(),
            content: "usa la key sk-abcdefghijklmnop".into(),
        });
        assert!(matches!(
            check(&current, &with_topic),
            Err(TidyRejection::Secret { .. })
        ));
    }

    #[test]
    fn refuses_unsafe_topic_names() {
        let current = facts(0..5);
        for name in [
            "../SOUL", "a/b", "..", "", ".oculto", "Trabajo", "x.md", "a\\b", "-x",
        ] {
            let mut next = proposal(&current);
            next.topics.push(TopicFile {
                name: name.into(),
                content: "algo".into(),
            });
            assert_eq!(
                check(&current, &next),
                Err(TidyRejection::UnsafeTopicName { name: name.into() }),
                "{name}"
            );
        }
        let mut next = proposal(&current);
        next.topics.push(TopicFile {
            name: "musica_y-series2".into(),
            content: "Le gustan los Beatles.".into(),
        });
        assert_eq!(check(&current, &next), Ok(()));
    }

    #[test]
    fn refuses_malformed_or_duplicated_facts() {
        let current = facts(0..5);
        let mut next = proposal(&current);
        next.memory.push_str("una línea suelta\n");
        assert!(matches!(
            check(&current, &next),
            Err(TidyRejection::Malformed { .. })
        ));
        let mut doubled = current.clone();
        doubled.push(current[0].clone());
        assert_eq!(
            check(&current, &proposal(&doubled)),
            Err(TidyRejection::DuplicateFact)
        );
        assert!(matches!(
            validate_tidy("# M\n- mal\n", &proposal(&current), &Limits::default()),
            Err(TidyRejection::CurrentMalformed { line: 2 })
        ));
    }
}
