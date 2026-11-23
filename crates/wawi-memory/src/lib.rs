//! Wawi's soul and memory as plain files in a home folder:
//!
//! ```text
//! <home>/SOUL.md          who Wawi is
//! <home>/MEMORY.md        one dated fact per line, newest first, under a header
//! <home>/memory/<topic>.md
//! <home>/trato.md         how to talk to the owner (style, not facts)
//! ```
//!
//! The caller chooses the folder. Nothing here talks to a model: the tidy-up
//! only has its validator, which decides whether a proposed MEMORY.md may
//! replace the current one. Secret-looking text is never written.

mod date;
mod doc;
mod home;
mod secrets;
mod tidy;
mod trato;

use std::fmt;
use std::io;

pub use date::{is_date, today_utc};
pub use doc::{Fact, MemoryDoc};
pub use home::{
    Home, MAX_ANCHOR_CHARS, MEMORY_FILE, SOUL_FILE, STARTER_SOUL, TOPICS_DIR, is_safe_topic_name,
    soul_anchor, split_context,
};
pub use secrets::looks_like_secret;
pub use tidy::{Limits, TidyProposal, TidyRejection, TopicFile, validate_tidy};
pub use trato::{
    MAX_CHARS as TRATO_MAX_CHARS, MAX_RULE_CHARS as TRATO_MAX_RULE_CHARS,
    MAX_RULES as TRATO_MAX_RULES, TRATO_FILE, TRATO_HEADER, TRATO_HEADING, Trato,
};

#[derive(Debug)]
pub enum MemoryError {
    Io(io::Error),
    /// The text looks like a password, key or token, so it is not written.
    Secret,
    Empty,
    /// A fact must fit on one line.
    MultiLine,
    /// Dates are `YYYY-MM-DD`.
    BadDate,
    /// A MEMORY.md line after the header that is not a fact (1-based).
    Malformed {
        line: usize,
    },
    UnsafeTopicName,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryError::Io(error) => write!(f, "memory file error: {error}"),
            MemoryError::Secret => write!(f, "refused: the text looks like a secret"),
            MemoryError::Empty => write!(f, "refused: empty fact"),
            MemoryError::MultiLine => write!(f, "refused: a fact must be one line"),
            MemoryError::BadDate => write!(f, "refused: dates are YYYY-MM-DD"),
            MemoryError::Malformed { line } => write!(f, "MEMORY.md line {line} is not a fact"),
            MemoryError::UnsafeTopicName => write!(f, "refused: unsafe topic name"),
        }
    }
}

impl std::error::Error for MemoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MemoryError::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for MemoryError {
    fn from(error: io::Error) -> Self {
        MemoryError::Io(error)
    }
}
