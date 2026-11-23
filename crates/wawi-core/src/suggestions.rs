//! Things Wawi's initiative proposes instead of doing. The owner accepts one
//! (it becomes an owner turn with its `ask` text) or dismisses it, and a
//! dismissed title is never proposed again. Pure: persisting the dismissed
//! titles across runs is up to the host (`dismissed`, `remember_dismissed`).

use std::fmt;

use crate::json::Json;
use crate::registry::{PermissionLevel, ToolSpec};
use crate::text::normalize;

/// The built-in tool initiative turns use to propose something.
pub const SUGGEST_TOOL: &str = "suggest";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SuggestionId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub id: SuggestionId,
    pub title: String,
    /// Why Wawi thinks it is worth it.
    pub why: String,
    /// The sentence the owner would say to do it.
    pub ask: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestError {
    /// A field is missing or blank.
    Missing(&'static str),
    /// The owner already dismissed something with this title.
    Dismissed,
    /// An open suggestion already has this title.
    Duplicate,
    /// Too many suggestions are waiting for the owner.
    Full,
}

/// Messages go to the initiative brain, so they say what to do instead.
impl fmt::Display for SuggestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuggestError::Missing(field) => write!(f, "falta el campo {field}"),
            SuggestError::Dismissed => write!(f, "el dueño ya descartó esto; no lo propongas más"),
            SuggestError::Duplicate => write!(f, "ya hay una sugerencia abierta con ese título"),
            SuggestError::Full => write!(f, "hay demasiadas sugerencias abiertas; esperá"),
        }
    }
}

impl std::error::Error for SuggestError {}

#[derive(Clone, Debug)]
pub struct Suggestions {
    open: Vec<Suggestion>,
    /// Normalized titles.
    dismissed: Vec<String>,
    max_open: usize,
    last_id: u64,
}

impl Suggestions {
    pub fn new(max_open: usize) -> Self {
        Self {
            open: Vec::new(),
            dismissed: Vec::new(),
            max_open,
            last_id: 0,
        }
    }

    /// Open suggestions, oldest first.
    pub fn open(&self) -> &[Suggestion] {
        &self.open
    }

    pub fn get(&self, id: SuggestionId) -> Option<&Suggestion> {
        self.open.iter().find(|s| s.id == id)
    }

    pub fn add(&mut self, title: &str, why: &str, ask: &str) -> Result<SuggestionId, SuggestError> {
        let (title, why, ask) = (title.trim(), why.trim(), ask.trim());
        let key = title_key(title);
        if key.is_empty() {
            return Err(SuggestError::Missing("title"));
        }
        if ask.is_empty() {
            return Err(SuggestError::Missing("ask"));
        }
        if why.is_empty() {
            return Err(SuggestError::Missing("why"));
        }
        if self.is_dismissed(title) {
            return Err(SuggestError::Dismissed);
        }
        if self.open.iter().any(|s| title_key(&s.title) == key) {
            return Err(SuggestError::Duplicate);
        }
        if self.open.len() >= self.max_open {
            return Err(SuggestError::Full);
        }
        self.last_id += 1;
        let id = SuggestionId(self.last_id);
        self.open.push(Suggestion {
            id,
            title: title.into(),
            why: why.into(),
            ask: ask.into(),
        });
        Ok(id)
    }

    /// Reads the arguments of a `suggest` call and adds it.
    pub fn add_from_args(&mut self, args: &Json) -> Result<Suggestion, SuggestError> {
        let field = |name: &'static str| {
            args.get(name)
                .and_then(Json::as_str)
                .ok_or(SuggestError::Missing(name))
        };
        let id = self.add(field("title")?, field("why")?, field("ask")?)?;
        Ok(self.get(id).cloned().expect("just added"))
    }

    /// Takes the suggestion out of the open list so the owner can act on it.
    /// It is not dismissed: the initiative may propose it again later.
    pub fn accept(&mut self, id: SuggestionId) -> Option<Suggestion> {
        let index = self.open.iter().position(|s| s.id == id)?;
        Some(self.open.remove(index))
    }

    pub fn dismiss(&mut self, id: SuggestionId) -> Option<Suggestion> {
        let suggestion = self.accept(id)?;
        self.remember_dismissed(&suggestion.title);
        Some(suggestion)
    }

    pub fn is_dismissed(&self, title: &str) -> bool {
        let key = title_key(title);
        self.dismissed.contains(&key)
    }

    /// Normalized dismissed titles, for the host to persist.
    pub fn dismissed(&self) -> &[String] {
        &self.dismissed
    }

    /// Restores a dismissed title, for example from a previous run. Any open
    /// suggestion with that title is dropped.
    pub fn remember_dismissed(&mut self, title: &str) {
        let key = title_key(title);
        if key.is_empty() {
            return;
        }
        self.open.retain(|s| title_key(&s.title) != key);
        if !self.dismissed.contains(&key) {
            self.dismissed.push(key);
        }
    }
}

/// Lowercase, accents folded, punctuation and spacing ignored.
fn title_key(title: &str) -> String {
    normalize(title)
}

/// What the initiative brain sees for the built-in `suggest` tool.
pub fn suggest_spec() -> ToolSpec {
    ToolSpec {
        name: SUGGEST_TOOL.into(),
        description: "Propone algo al dueño en vez de hacerlo. El dueño lo acepta o lo descarta."
            .into(),
        params_schema_json: r#"{"type":"object","properties":{"title":{"type":"string","description":"título corto"},"why":{"type":"string","description":"por qué vale la pena"},"ask":{"type":"string","description":"la frase que diría el dueño para hacerlo"}},"required":["title","why","ask"]}"#.into(),
        permission: PermissionLevel::Read,
        needs_confirmation: false,
        ack: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse;

    #[test]
    fn dismissed_titles_never_come_back_whatever_the_spelling() {
        let mut store = Suggestions::new(5);
        let id = store
            .add("Regar las plantas", "hace calor", "regá")
            .unwrap();
        assert!(store.dismiss(id).is_some());
        assert_eq!(
            store.add("  regar LAS plantás! ", "otra vez", "regá"),
            Err(SuggestError::Dismissed)
        );
        assert_eq!(store.dismissed(), ["regar las plantas"]);
    }

    #[test]
    fn accepted_titles_may_come_back() {
        let mut store = Suggestions::new(5);
        let id = store.add("Mate", "son las 5", "poné la pava").unwrap();
        assert_eq!(store.accept(id).map(|s| s.ask), Some("poné la pava".into()));
        assert!(store.open().is_empty());
        assert!(store.add("Mate", "son las 5", "poné la pava").is_ok());
    }

    #[test]
    fn caps_open_suggestions_and_refuses_duplicates_and_blanks() {
        let mut store = Suggestions::new(2);
        store.add("uno", "x", "a").unwrap();
        assert_eq!(store.add("Uno", "x", "a"), Err(SuggestError::Duplicate));
        store.add("dos", "x", "a").unwrap();
        assert_eq!(store.add("tres", "x", "a"), Err(SuggestError::Full));
        assert_eq!(
            store.add("¿?", "x", "a"),
            Err(SuggestError::Missing("title"))
        );
        assert_eq!(store.add("c", "x", " "), Err(SuggestError::Missing("ask")));
        assert_eq!(store.open().len(), 2);
    }

    #[test]
    fn unknown_ids_do_nothing() {
        let mut store = Suggestions::new(5);
        assert_eq!(store.accept(SuggestionId(9)), None);
        assert_eq!(store.dismiss(SuggestionId(9)), None);
        assert!(store.dismissed().is_empty());
    }

    #[test]
    fn remembering_a_dismissal_drops_the_open_one() {
        let mut store = Suggestions::new(5);
        store.add("Backup", "x", "hacé el backup").unwrap();
        store.remember_dismissed("backup");
        assert!(store.open().is_empty());
        assert!(store.is_dismissed("BACKUP"));
    }

    #[test]
    fn reads_tool_arguments() {
        let mut store = Suggestions::new(5);
        let args = parse(r#"{"title":"Luz","why":"es de noche","ask":"prendé la luz"}"#).unwrap();
        assert_eq!(store.add_from_args(&args).unwrap().ask, "prendé la luz");
        let args = parse(r#"{"title":"Luz2","why":"x"}"#).unwrap();
        assert_eq!(
            store.add_from_args(&args),
            Err(SuggestError::Missing("ask"))
        );
        let args = parse(r#"{"title":1,"why":"x","ask":"y"}"#).unwrap();
        assert_eq!(
            store.add_from_args(&args),
            Err(SuggestError::Missing("title"))
        );
    }
}
