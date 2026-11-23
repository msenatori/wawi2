//! The turn feed for plugins with `observa_charla = true`: after every
//! finished turn, what the owner said and what Wawi answered, nothing else
//! (no memory, no tool results, no secrets). Pure: the host reports what
//! happened and sends what comes out.

use serde_json::{Value, json};

/// One finished turn, as the plugin receives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedItem {
    pub turn: u64,
    /// Local ISO times, to the second.
    pub started: String,
    pub ended: String,
    pub owner: String,
    pub wawi: String,
    /// The owner cut Wawi off before she finished.
    pub interrupted: bool,
    /// The turn was Wawi's own initiative, not something the owner asked.
    pub initiative: bool,
}

impl FeedItem {
    /// The arguments of the `observar_charla` call.
    pub fn to_args(&self) -> Value {
        json!({
            "turno": self.turn,
            "inicio": self.started,
            "fin": self.ended,
            "dueno": self.owner,
            "wawi": self.wawi,
            "interrumpida": self.interrupted,
            "iniciativa": self.initiative,
        })
    }
}

#[derive(Debug, Default)]
struct Open {
    turn: Option<u64>,
    started: String,
    owner: Vec<String>,
    wawi: Vec<String>,
    initiative: bool,
}

#[derive(Debug, Default)]
pub struct TurnFeed {
    open: Option<Open>,
    /// For turns that never got a core turn id (nothing was said).
    fallback: u64,
}

impl TurnFeed {
    /// The owner's words (a final transcript). Words after Wawi already
    /// answered start a new turn: the previous one is returned, finished.
    pub fn heard(&mut self, text: &str, now: &str) -> Option<FeedItem> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let finished = match &self.open {
            Some(open) if !open.wawi.is_empty() => self.finish(false, now),
            _ => None,
        };
        let open = self.open.get_or_insert_with(|| Open {
            started: now.to_owned(),
            ..Open::default()
        });
        open.owner.push(text.to_owned());
        finished
    }

    /// Something Wawi said for `turn` (an answer, a question, a route reply).
    pub fn said(&mut self, turn: u64, text: &str, initiative: bool, now: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let open = self.open.get_or_insert_with(|| Open {
            started: now.to_owned(),
            initiative,
            ..Open::default()
        });
        open.turn.get_or_insert(turn);
        open.wawi.push(text.to_owned());
    }

    /// The turn ended (back to idle, or cut off). Nothing if nothing was said.
    pub fn finish(&mut self, interrupted: bool, now: &str) -> Option<FeedItem> {
        let open = self.open.take()?;
        if open.owner.is_empty() && open.wawi.is_empty() {
            return None;
        }
        let turn = match open.turn {
            Some(turn) => turn,
            None => {
                self.fallback += 1;
                self.fallback
            }
        };
        Some(FeedItem {
            turn,
            started: open.started,
            ended: now.to_owned(),
            owner: open.owner.join(" "),
            wawi: open.wawi.join(" "),
            interrupted,
            initiative: open.initiative,
        })
    }

    /// The owner started talking over Wawi: whatever she was saying ends
    /// there, marked. Before she said anything, the turn simply goes on.
    pub fn interrupted(&mut self, now: &str) -> Option<FeedItem> {
        match &self.open {
            Some(open) if !open.wawi.is_empty() => self.finish(true, now),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_is_the_owner_then_wawi() {
        let mut feed = TurnFeed::default();
        assert_eq!(feed.heard("qué hay hoy", "t1"), None);
        feed.said(4, "Tenés dos tareas.", false, "t2");
        feed.said(4, "¿Arrancamos?", false, "t2");
        let item = feed.finish(false, "t3").unwrap();
        assert_eq!(item.turn, 4);
        assert_eq!(item.owner, "qué hay hoy");
        assert_eq!(item.wawi, "Tenés dos tareas. ¿Arrancamos?");
        assert_eq!((item.started.as_str(), item.ended.as_str()), ("t1", "t3"));
        assert!(!item.interrupted);
        assert_eq!(item.to_args()["dueno"], "qué hay hoy");
        assert_eq!(feed.finish(false, "t4"), None);
    }

    #[test]
    fn talking_over_wawi_closes_her_turn_marked() {
        let mut feed = TurnFeed::default();
        feed.heard("contame", "t1");
        assert_eq!(feed.interrupted("t1"), None);
        feed.said(2, "Había una vez", false, "t2");
        let item = feed.interrupted("t3").unwrap();
        assert!(item.interrupted);
        feed.heard("pará", "t4");
        let item = feed.finish(false, "t5").unwrap();
        assert_eq!(item.owner, "pará");
        assert_eq!(item.wawi, "");
    }

    #[test]
    fn initiative_turns_have_no_owner_words() {
        let mut feed = TurnFeed::default();
        feed.said(7, "Buen día, hoy tenés tres tareas.", true, "t1");
        let item = feed.finish(false, "t2").unwrap();
        assert!(item.initiative && item.owner.is_empty());
    }
}
