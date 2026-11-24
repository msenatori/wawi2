//! Unanswered questions: when Wawi asks the owner something and the owner
//! does not answer, she remembers it and brings it back later, once
//! («che, al final…?»). The idea and the numbers come from Orca's bots:
//! 91 % of the owner's answers come within 2 turns of the question.
//!
//! All rules, no model, nothing on the turn path. The host gives every
//! finished turn (the same summary `observa_charla` gets) to a [`Tracker`]:
//!
//! 1. **A question for the owner**: her answer to an owner turn ends with a
//!    question (`¿…?`), of three words or more, that talks to the owner
//!    (voseo, «vos», «te»…). Filler («¿algo más?»), tags («¿viste?»), the
//!    core's own yes-or-no and turns the owner cut off do not count, and
//!    neither do her initiative turns (offering again is nagging).
//! 2. **Answered**: one of the owner's next two turns answers it: a short
//!    reply (six words or fewer), a yes-or-no word first, or a content word
//!    in common with the question.
//! 3. **Unanswered**: two owner turns that do not answer it, or no owner turn
//!    at all within 15 minutes. It becomes a low-priority fact in
//!    `<home>/cola/hechos` (`origen: "preguntas"`), not before 2 hours after
//!    the question and gone after 3 days. Its id comes from the question's
//!    words, and the inbox never says an id twice: the same question is
//!    never retaken twice.
//! 4. **A late answer** to a question already handed over takes it back
//!    (`Seen::Answered`): the host moves its fact to `cur/` as `respondida`.
//!
//! The bench (`bench/preguntas.tsv`, `preguntas-bench`) measures the rules.

use wawi_core::normalize;

/// `origen` of the facts.
pub const ORIGIN: &str = "preguntas";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Owner turns after the question that may answer it.
    pub turns: u8,
    /// With no owner turn in this long, it is unanswered.
    pub window_s: i64,
    /// Brought back no sooner than this after it was asked.
    pub retake_after_s: i64,
    /// And no later than this.
    pub expire_after_s: i64,
    /// Questions followed at once.
    pub max_open: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            turns: 2,
            window_s: 15 * 60,
            retake_after_s: 2 * 3600,
            expire_after_s: 3 * 24 * 3600,
            max_open: 3,
        }
    }
}

/// A finished turn, as far as the tracker cares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn<'a> {
    pub owner: &'a str,
    pub wawi: &'a str,
    pub interrupted: bool,
    pub initiative: bool,
}

/// A question the owner did not answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unanswered {
    /// `pregunta-<hash>`: the same words give the same id.
    pub id: String,
    pub question: String,
    /// Unix seconds.
    pub asked_at: i64,
}

impl Unanswered {
    pub fn not_before(&self, config: &Config) -> i64 {
        self.asked_at + config.retake_after_s
    }

    pub fn expires(&self, config: &Config) -> i64 {
        self.asked_at + config.expire_after_s
    }
}

/// What the tracker learned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    Unanswered(Unanswered),
    /// A question already handed over was answered late: take it back.
    Answered {
        id: String,
    },
}

#[derive(Clone, Debug)]
struct Open {
    question: String,
    asked_at: i64,
    turns: u8,
}

#[derive(Debug, Default)]
pub struct Tracker {
    config: Config,
    open: Vec<Open>,
    /// Handed over recently, in case a late answer comes.
    handed: Vec<Unanswered>,
}

impl Tracker {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            open: Vec::new(),
            handed: Vec::new(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// A finished turn at `now` (Unix seconds).
    pub fn turn(&mut self, turn: &Turn<'_>, now: i64) -> Vec<Seen> {
        let mut seen = Vec::new();
        let owner_spoke = !turn.initiative && !turn.owner.trim().is_empty();
        if owner_spoke {
            let window = self.config.window_s;
            let max_turns = self.config.turns;
            let mut still = Vec::new();
            for mut open in self.open.drain(..) {
                if answers(&open.question, turn.owner) {
                    continue;
                }
                open.turns += 1;
                if open.turns >= max_turns || now - open.asked_at > window {
                    let gone = unanswered(&open);
                    self.handed.push(gone.clone());
                    seen.push(Seen::Unanswered(gone));
                } else {
                    still.push(open);
                }
            }
            self.open = still;
            let expire = self.config.retake_after_s;
            self.handed.retain(|h| now - h.asked_at < expire);
            if let Some(index) = self
                .handed
                .iter()
                .position(|h| mentions(&h.question, turn.owner))
            {
                let late = self.handed.remove(index);
                seen.push(Seen::Answered { id: late.id });
            }
        }
        if owner_spoke
            && !turn.interrupted
            && let Some(question) = final_question(turn.wawi)
        {
            self.open.retain(|o| o.question != question);
            self.open.push(Open {
                question,
                asked_at: now,
                turns: 0,
            });
            while self.open.len() > self.config.max_open.max(1) {
                self.open.remove(0);
            }
        }
        seen
    }

    /// On the host's tick: questions nobody came back to.
    pub fn tick(&mut self, now: i64) -> Vec<Seen> {
        let window = self.config.window_s;
        if self.open.iter().all(|o| now - o.asked_at <= window) {
            return Vec::new();
        }
        let (gone, still): (Vec<Open>, Vec<Open>) =
            self.open.drain(..).partition(|o| now - o.asked_at > window);
        self.open = still;
        gone.iter()
            .map(|open| {
                let gone = unanswered(open);
                self.handed.push(gone.clone());
                Seen::Unanswered(gone)
            })
            .collect()
    }

    pub fn open_len(&self) -> usize {
        self.open.len()
    }
}

fn unanswered(open: &Open) -> Unanswered {
    Unanswered {
        id: question_id(&open.question),
        question: open.question.clone(),
        asked_at: open.asked_at,
    }
}

/// `pregunta-` and a hash of the question's normalized words (FNV-1a).
pub fn question_id(question: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in normalize(question).bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("pregunta-{:012x}", hash & 0xffff_ffff_ffff)
}

/// Offers and fillers that are not worth bringing back, normalized.
const FILLER: &[&str] = &[
    "algo mas",
    "otra cosa",
    "en que te ayudo",
    "en que te puedo ayudar",
    "te ayudo con algo",
    "te puedo ayudar",
    "que mas",
    "necesitas algo",
    "si o no",
    "me lo repetis",
    "me repetis",
    "como dijiste",
    "que dijiste",
];

/// Second-person words without a telltale accent, normalized.
const SECOND_PERSON: &[&str] = &[
    "vos",
    "te",
    "tu",
    "tus",
    "tuyo",
    "tuya",
    "contigo",
    "vas",
    "sos",
    "estas",
    "tenes",
    "queres",
    "podes",
    "sabes",
    "preferis",
    "decis",
    "venis",
    "salis",
    "seguis",
    "pensas",
    "necesitas",
    "llegaste",
    "pudiste",
    "hiciste",
    "fuiste",
    "tuviste",
    "viste",
    "dijiste",
    "pensaste",
    "decidiste",
    "terminaste",
    "probaste",
    "elegiste",
    "anduvo",
    "andas",
];

/// The question at the end of Wawi's words, if it is one for the owner.
pub fn final_question(wawi: &str) -> Option<String> {
    let sentences = sentences(wawi);
    let last = sentences.last()?;
    let candidate = if last.ends_with('?') {
        last
    } else {
        // «¿Lo agendo? Avisame.»: a short closer after the question.
        let before = sentences.len().checked_sub(2).map(|i| &sentences[i])?;
        (before.ends_with('?') && words(last) <= 3).then_some(before)?
    };
    let question = match candidate.rfind('¿') {
        Some(start) => candidate[start..].trim(),
        None => candidate.trim(),
    };
    let plain = normalize(question);
    // Filler is short: «lo que más te cansa del laburo» is a real question.
    let filler = words(&plain) <= 6 && FILLER.iter().any(|f| contains_words(&plain, f));
    if words(&plain) < 3 || filler {
        return None;
    }
    let to_owner = plain.split_whitespace().any(|w| SECOND_PERSON.contains(&w))
        || question
            .split(|c: char| !c.is_alphabetic())
            .any(|w| w.ends_with("ás") || w.ends_with("és") || w.ends_with("ís"));
    to_owner.then(|| question.to_owned())
}

/// The owner's words answer the question: a short reply, a yes-or-no
/// word first, or a content word in common.
pub fn answers(question: &str, owner: &str) -> bool {
    let owner = normalize(owner);
    let owner_words: Vec<&str> = owner.split_whitespace().collect();
    if owner_words.is_empty() {
        return false;
    }
    if owner_words.len() <= 6 {
        return true;
    }
    const OPENERS: &[&str] = &[
        // Not «bueno» or «listo»: they open anything.
        "si", "no", "dale", "obvio", "claro", "ni", "capaz", "todavia", "ya", "nah", "ok", "creo",
        "seguro",
    ];
    if OPENERS.contains(&owner_words[0]) {
        return true;
    }
    mentions(question, &owner)
}

/// A content word in common: the stronger sign, alone enough for a late
/// answer (a late «sí» may be about anything).
pub fn mentions(question: &str, owner: &str) -> bool {
    let (question, owner) = (normalize(question), normalize(owner));
    let stems: Vec<String> = content_words(&question).map(stem).collect();
    content_words(&owner).any(|w| stems.contains(&stem(w)))
}

const STOPWORDS: &[&str] = &[
    "que", "como", "cuando", "donde", "quien", "cual", "cuanto", "para", "pero", "porque", "esto",
    "esta", "este", "eso", "esos", "esas", "estos", "estas", "algo", "alguna", "alguno", "todo",
    "toda", "todos", "mucho", "poco", "mas", "menos", "muy", "tambien", "ahora", "despues",
    "antes", "hoy", "manana", "ayer", "aca", "ahi", "alla", "bien", "mal", "queres", "tenes",
    "podes", "sabes", "vos", "decime", "contame", "fijate", "hacer", "hago", "haces", "tengo",
    "quiero", "puedo", "sobre", "entre", "desde", "hasta", "sino", "nada", "cosa", "cosas", "vez",
    "veces", "estoy", "estas", "esta", "estan", "habia", "tiene", "tienen", "dijiste", "dije",
    "eso", "aquel", "otro", "otra", "otros", "otras",
];

fn content_words(text: &str) -> impl Iterator<Item = &str> {
    text.split_whitespace()
        .filter(|w| w.chars().count() >= 4 && !STOPWORDS.contains(w))
}

/// The first four letters: «avise» and «avisame» meet, «cumple» and
/// «cumpleaños» too.
fn stem(word: &str) -> String {
    word.chars().take(4).collect()
}

fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

fn contains_words(text: &str, phrase: &str) -> bool {
    format!(" {text} ").contains(&format!(" {phrase} "))
}

/// Sentences, each with its closing mark.
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        current.push(c);
        if matches!(c, '.' | '!' | '?' | '…') {
            let sentence = current.trim().to_owned();
            if !sentence.is_empty() {
                out.push(sentence);
            }
            current.clear();
        }
    }
    let rest = current.trim();
    if !rest.is_empty() {
        out.push(rest.to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60;

    fn owner_turn<'a>(owner: &'a str, wawi: &'a str) -> Turn<'a> {
        Turn {
            owner,
            wawi,
            interrupted: false,
            initiative: false,
        }
    }

    #[test]
    fn finds_the_question_for_the_owner_at_the_end() {
        assert_eq!(
            final_question("Listo, anotado. ¿Querés que te lo recuerde el lunes?").as_deref(),
            Some("¿Querés que te lo recuerde el lunes?")
        );
        assert_eq!(
            final_question("Está lindo. ¿Al final fuiste al cumple de Juli? Contame.").as_deref(),
            Some("¿Al final fuiste al cumple de Juli?")
        );
        for not in [
            "Son las cinco.",
            "¿Algo más?",
            "Hecho. ¿Te ayudo con algo más?",
            "Hace frío, ¿viste?",
            "No te entendí. ¿Sí o no?",
            "¿Qué será de la vida de los pingüinos?",
            "¿Lo agendo? Avisame cuando puedas así lo dejo listo hoy.",
        ] {
            assert_eq!(final_question(not), None, "{not}");
        }
    }

    #[test]
    fn short_replies_shared_words_and_yes_or_no_answer_it() {
        let q = "¿Querés que agende el turno del dentista?";
        assert!(answers(q, "sí, dale"));
        assert!(answers(q, "el martes a la tarde mejor"));
        assert!(answers(
            q,
            "mirá, lo del dentista lo veo yo después con calma, gracias igual"
        ));
        assert!(answers(
            q,
            "no, prefiero esperar a la semana que viene para ver qué onda"
        ));
        assert!(!answers(
            q,
            "poneme música tranquila para laburar un rato largo por favor"
        ));
    }

    #[test]
    fn two_turns_that_do_not_answer_make_it_unanswered_once() {
        let mut t = Tracker::new(Config::default());
        let asked = owner_turn(
            "anotá que tengo dentista",
            "Anotado. ¿Querés que te avise el día antes?",
        );
        assert!(t.turn(&asked, 0).is_empty());
        let other = owner_turn(
            "poneme música tranquila para laburar un rato largo por favor",
            "Dale.",
        );
        assert!(t.turn(&other, MIN).is_empty());
        let again = owner_turn(
            "y fijate cómo está el clima para el fin de semana en la costa",
            "Sol y 25 grados.",
        );
        let seen = t.turn(&again, 2 * MIN);
        let [Seen::Unanswered(gone)] = seen.as_slice() else {
            panic!("{seen:?}");
        };
        assert_eq!(gone.question, "¿Querés que te avise el día antes?");
        assert_eq!(gone.asked_at, 0);
        assert_eq!(gone.not_before(t.config()), 2 * 3600);
        assert_eq!(t.open_len(), 0);
        // A late answer takes it back.
        let late = owner_turn("ah, sí, avisame el día antes porfa", "Hecho.");
        assert_eq!(
            t.turn(&late, 30 * MIN),
            vec![Seen::Answered {
                id: gone.id.clone()
            }]
        );
    }

    #[test]
    fn an_answer_closes_it_and_silence_makes_it_unanswered_on_the_tick() {
        let mut t = Tracker::new(Config::default());
        t.turn(
            &owner_turn("hola", "¡Hola! ¿Cómo te fue en la entrevista?"),
            0,
        );
        assert!(
            t.turn(&owner_turn("bien, re bien", "¡Qué bueno!"), MIN)
                .is_empty()
        );
        assert_eq!(t.open_len(), 0);
        t.turn(
            &owner_turn("hola", "¡Hola! ¿Cómo te fue en la entrevista?"),
            0,
        );
        assert!(t.tick(15 * MIN).is_empty());
        let seen = t.tick(16 * MIN);
        assert!(matches!(seen.as_slice(), [Seen::Unanswered(_)]), "{seen:?}");
        assert!(t.tick(17 * MIN).is_empty());
    }

    #[test]
    fn initiative_and_cut_off_turns_ask_nothing() {
        let mut t = Tracker::new(Config::default());
        let mut turn = owner_turn("", "Vi que llueve. ¿Querés que te recuerde el paraguas?");
        turn.initiative = true;
        t.turn(&turn, 0);
        let mut cut = owner_turn("contame", "Te cuento: ¿sabés qué pasó con el PR?");
        cut.interrupted = true;
        t.turn(&cut, 0);
        assert_eq!(t.open_len(), 0);
    }

    #[test]
    fn the_same_question_has_the_same_id() {
        assert_eq!(
            question_id("¿Querés que te avise el día antes?"),
            question_id("¿queres que te avise el dia antes")
        );
        assert_ne!(
            question_id("¿Querés que te avise el día antes?"),
            question_id("¿Querés que te avise el día después?")
        );
    }
}
