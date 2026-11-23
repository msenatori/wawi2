//! Turns a stream of text deltas into spoken chunks.
//!
//! The core speaks every `BrainText` as its own utterance, so sending each
//! token-sized delta would make the voice stutter. Sentences are released as
//! soon as their end is certain: a terminator followed by whitespace, or a
//! line break. A dot after an abbreviation ("Sr.", "etc.") or an initial does
//! not end a sentence.
//!
//! The first chunk of a turn goes out sooner. A sentence end at the very end
//! of the text so far counts without waiting for the space after it (unless
//! it may be a decimal point or an abbreviation). And it may end at a clause
//! boundary (a comma,
//! semicolon or colon followed by whitespace, or right before " y " or
//! " pero "), once it has at least [`MIN_FIRST_CLAUSE_CHARS`] characters: the
//! owner hears the voice while the model is still writing the rest of the
//! sentence. Later chunks are whole sentences, which sound better. Whatever is
//! left goes out on `flush`.

/// The shortest first chunk released at a clause boundary.
pub const MIN_FIRST_CLAUSE_CHARS: usize = 30;

/// Conjunctions a first chunk may end right before.
const CLAUSE_WORDS: &[&str] = &["y", "pero"];

/// A dot after one of these never ends a sentence: a name follows.
const TITLES: &[&str] = &[
    "sr", "sra", "srta", "dr", "dra", "lic", "ing", "prof", "av", "gral", "sto", "sta", "st",
];

/// A dot after one of these ends a sentence only if a capital letter follows.
const ABBREVIATIONS: &[&str] = &[
    "etc", "ej", "aprox", "pág", "pag", "núm", "num", "tel", "vs", "cap", "art", "ud", "uds", "p",
    "nro", "mín", "máx", "min", "max",
];

#[derive(Debug)]
pub struct SentenceBuffer {
    pending: String,
    /// Something was already released, so clause boundaries no longer count.
    released: bool,
    clause_first: bool,
}

impl Default for SentenceBuffer {
    /// Releases the first chunk at a clause boundary, then whole sentences.
    fn default() -> Self {
        Self {
            pending: String::new(),
            released: false,
            clause_first: true,
        }
    }
}

impl SentenceBuffer {
    /// Whole sentences only, the first chunk included.
    pub fn sentences_only() -> Self {
        Self {
            clause_first: false,
            ..Self::default()
        }
    }

    /// Adds a delta and returns the chunks it completed, in order.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.pending.push_str(delta);
        let end = last_boundary(&self.pending).or_else(|| {
            (self.clause_first && !self.released)
                .then(|| {
                    first_sentence_at_tail(&self.pending)
                        .or_else(|| first_clause_end(&self.pending))
                })
                .flatten()
        });
        let Some(end) = end else {
            return Vec::new();
        };
        let rest = self.pending.split_off(end);
        let done = std::mem::replace(&mut self.pending, rest);
        self.released(split_sentences(&done))
    }

    /// Everything still held, as sentences. Empty when nothing is held.
    pub fn flush(&mut self) -> Vec<String> {
        let done = std::mem::take(&mut self.pending);
        self.released(split_sentences(&done))
    }

    fn released(&mut self, out: Vec<String>) -> Vec<String> {
        self.released |= !out.is_empty();
        out
    }
}

fn is_terminator(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…')
}

/// The word right before byte offset `end`, letters only.
fn word_before(text: &str, end: usize) -> &str {
    let head = &text[..end];
    let start = head
        .char_indices()
        .rev()
        .find(|&(_, c)| !c.is_alphabetic())
        .map_or(0, |(index, c)| index + c.len_utf8());
    &head[start..]
}

/// The first character after byte offset `from` that is not whitespace.
fn next_visible(text: &str, from: usize) -> Option<char> {
    text[from..].chars().find(|c| !c.is_whitespace())
}

/// Whether the dot at byte offset `dot` (followed by whitespace) ends a
/// sentence. `None` when that depends on text that has not arrived yet.
fn dot_ends_sentence(text: &str, dot: usize) -> Option<bool> {
    let word = word_before(text, dot);
    let lower = word.to_lowercase();
    let is_initial = word.chars().count() == 1 && word.chars().all(char::is_uppercase);
    if is_initial || TITLES.contains(&lower.as_str()) {
        return Some(false);
    }
    if ABBREVIATIONS.contains(&lower.as_str()) {
        return next_visible(text, dot + 1).map(char::is_uppercase);
    }
    Some(true)
}

/// Byte offsets where a sentence certainly ends: just past a terminator that
/// is followed by whitespace, or at a line break.
fn boundaries(text: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if c == '\n' {
            found.push(index + 1);
        } else if is_terminator(c)
            && chars.peek().is_some_and(|&(_, next)| next.is_whitespace())
            && (c != '.' || dot_ends_sentence(text, index) == Some(true))
        {
            found.push(index + c.len_utf8());
        }
    }
    found
}

fn last_boundary(text: &str) -> Option<usize> {
    boundaries(text).last().copied()
}

/// The end of `text` when it ends a sentence for sure even without the space
/// after it: a question or exclamation mark, an ellipsis, or a dot after a
/// word that is no abbreviation or initial. A dot after a digit may be a
/// decimal point, so it waits. Only for the first chunk, which should not wait
/// for the next delta just to see a space.
fn first_sentence_at_tail(text: &str) -> Option<usize> {
    let mut chars = text.chars().rev();
    let last = chars.next()?;
    let before = chars.next()?;
    let ends = match last {
        '?' | '!' | '…' => true,
        '.' => before.is_alphabetic() && dot_ends_sentence(text, text.len() - 1) == Some(true),
        _ => false,
    };
    ends.then_some(text.len())
}

/// The first byte offset where a first chunk of at least
/// [`MIN_FIRST_CLAUSE_CHARS`] may end at a clause boundary, if there is one
/// already certain. Never inside an open question or exclamation, and never
/// between two numbers ("3, 4 y 5" stays whole).
fn first_clause_end(text: &str) -> Option<usize> {
    let long_enough = |end: usize| text[..end].trim().chars().count() >= MIN_FIRST_CLAUSE_CHARS;
    let open_mark = |end: usize| {
        let head = &text[..end];
        head.matches('¿').count() > head.matches('?').count()
            || head.matches('¡').count() > head.matches('!').count()
    };
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (i, &(index, c)) in chars.iter().enumerate() {
        let next = chars.get(i + 1).map(|&(_, next)| next);
        let candidate = if matches!(c, ',' | ';' | ':') && next.is_some_and(char::is_whitespace) {
            let after = index + c.len_utf8();
            // After a digit, wait to see whether another number follows.
            let between_numbers = i > 0
                && chars[i - 1].1.is_ascii_digit()
                && next_visible(text, after).is_none_or(|c| c.is_ascii_digit());
            (!between_numbers).then_some(after)
        } else if c.is_whitespace() && !(i > 0 && chars[i - 1].1.is_ascii_digit()) {
            let rest = &text[index + c.len_utf8()..];
            CLAUSE_WORDS
                .iter()
                .any(|word| {
                    rest.strip_prefix(word)
                        .and_then(|after| after.chars().next())
                        .is_some_and(char::is_whitespace)
                })
                .then_some(index)
        } else {
            None
        };
        if let Some(end) = candidate
            && long_enough(end)
            && !open_mark(end)
        {
            return Some(end);
        }
    }
    None
}

fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    for end in boundaries(text) {
        push_clean(&mut out, &text[start..end]);
        start = end;
    }
    push_clean(&mut out, &text[start..]);
    out
}

/// Drops markdown the model should not have written anyway: it would be read
/// out loud as symbols.
fn push_clean(out: &mut Vec<String>, piece: &str) {
    // Dots left over from an ellipsis cut after its first dot.
    let piece = piece.trim_start().trim_start_matches(['.', '…']);
    let mut text: String = piece
        .chars()
        .filter(|c| !matches!(c, '*' | '#' | '`' | '_'))
        .collect();
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("- ") {
        text = rest.to_owned();
    }
    let text = text.trim();
    if text.chars().any(char::is_alphanumeric) {
        out.push(text.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_with(mut buffer: SentenceBuffer, deltas: &[&str]) -> Vec<String> {
        let mut out = Vec::new();
        for delta in deltas {
            out.extend(buffer.push(delta));
        }
        out.extend(buffer.flush());
        out
    }

    fn feed(deltas: &[&str]) -> Vec<String> {
        feed_with(SentenceBuffer::sentences_only(), deltas)
    }

    /// Clause-first, one character per delta, like the slowest stream.
    fn trickle(text: &str) -> Vec<String> {
        let chars: Vec<String> = text.chars().map(String::from).collect();
        let deltas: Vec<&str> = chars.iter().map(String::as_str).collect();
        feed_with(SentenceBuffer::default(), &deltas)
    }

    #[test]
    fn releases_a_sentence_once_its_end_is_certain() {
        let mut buffer = SentenceBuffer::sentences_only();
        assert!(buffer.push("Hola, ").is_empty());
        assert!(buffer.push("che.").is_empty());
        assert_eq!(buffer.push(" ¿Todo"), vec!["Hola, che."]);
        assert!(buffer.push(" bien?").is_empty());
        assert_eq!(buffer.flush(), vec!["¿Todo bien?"]);
        assert!(buffer.flush().is_empty());
    }

    #[test]
    fn numbers_and_several_sentences_in_one_delta() {
        assert_eq!(
            feed(&["Son 3.5 grados. Abrigate! Y llevá ", "paraguas…"]),
            vec!["Son 3.5 grados.", "Abrigate!", "Y llevá paraguas…"]
        );
    }

    #[test]
    fn line_breaks_end_sentences_and_markdown_goes() {
        assert_eq!(
            feed(&["**Lista**\n- uno\n", "- dos"]),
            vec!["Lista", "uno", "dos"]
        );
    }

    #[test]
    fn whitespace_alone_is_never_spoken() {
        assert!(feed(&["  ", "\n\n", " "]).is_empty());
    }

    #[test]
    fn the_first_chunk_ends_at_a_comma_once_it_is_long_enough() {
        assert_eq!(
            trickle(
                "Un arco iris es la luz del sol que se descompone, y por eso ves colores. \
                 Pasa cuando llueve, con sol de frente, a la tarde."
            ),
            vec![
                "Un arco iris es la luz del sol que se descompone,",
                "y por eso ves colores.",
                // Later chunks are whole sentences, commas and all.
                "Pasa cuando llueve, con sol de frente, a la tarde.",
            ]
        );
    }

    #[test]
    fn the_first_chunk_may_end_before_y_or_pero() {
        assert_eq!(
            trickle("Mañana va a estar soleado a la mañana y a la tarde llueve."),
            vec![
                "Mañana va a estar soleado a la mañana",
                "y a la tarde llueve."
            ]
        );
        assert_eq!(
            trickle("Me encantaría ayudarte con eso ahora pero no tengo esa herramienta."),
            vec![
                "Me encantaría ayudarte con eso ahora",
                "pero no tengo esa herramienta."
            ]
        );
        // "y" inside a word or with nothing after it yet does not count.
        let mut buffer = SentenceBuffer::default();
        assert!(
            buffer
                .push("Esto es una prueba bastante larga y")
                .is_empty()
        );
        assert_eq!(
            buffer.push(" listo"),
            vec!["Esto es una prueba bastante larga"]
        );
        assert!(
            trickle("Trabajo con el yerbatero de Misiones yendo y viniendo.")
                .contains(&"Trabajo con el yerbatero de Misiones yendo".to_owned())
        );
    }

    #[test]
    fn short_first_clauses_wait_for_more() {
        assert_eq!(
            trickle("Sí, claro, dale. Ahí va."),
            vec!["Sí, claro, dale.", "Ahí va."]
        );
    }

    #[test]
    fn numbers_never_split() {
        assert_eq!(
            trickle("La temperatura de hoy es de 3,5 grados y sube."),
            vec!["La temperatura de hoy es de 3,5 grados", "y sube."]
        );
        assert_eq!(
            trickle("Los números que salieron hoy son el 3, 4 y 5. Nada más."),
            vec!["Los números que salieron hoy son el 3, 4 y 5.", "Nada más."]
        );
        assert_eq!(
            trickle("Te despierto mañana a las 7:30 en punto. Dale."),
            vec!["Te despierto mañana a las 7:30 en punto.", "Dale."]
        );
    }

    #[test]
    fn open_questions_are_not_split() {
        assert_eq!(
            trickle("¿Querés que te cuente algo del mate, o preferís otra cosa?"),
            vec!["¿Querés que te cuente algo del mate, o preferís otra cosa?"]
        );
    }

    #[test]
    fn abbreviations_and_initials_do_not_end_sentences() {
        assert_eq!(
            feed(&["Llamá a la Dra. Fernández y al Sr. Pérez. Listo."]),
            vec!["Llamá a la Dra. Fernández y al Sr. Pérez.", "Listo."]
        );
        assert_eq!(
            feed(&["Traé pan, leche, etc. y algo dulce. Etc. Fin."]),
            vec!["Traé pan, leche, etc. y algo dulce.", "Etc.", "Fin."]
        );
        assert_eq!(
            feed(&["Lo escribió J. R. R. Tolkien. Genial."]),
            vec!["Lo escribió J. R. R. Tolkien.", "Genial."]
        );
        // Whether "etc." ends it depends on what follows: wait for it.
        let mut buffer = SentenceBuffer::sentences_only();
        assert!(buffer.push("Pan, leche, etc. ").is_empty());
        assert_eq!(buffer.push("Y listo"), vec!["Pan, leche, etc."]);
    }

    #[test]
    fn the_first_sentence_does_not_wait_for_the_space_after_it() {
        let mut buffer = SentenceBuffer::default();
        assert_eq!(buffer.push("Canberra."), vec!["Canberra."]);
        // Later sentences do wait, as before.
        assert!(buffer.push(" Mucha gente cree que es Sídney.").is_empty());
        assert_eq!(buffer.push(" "), vec!["Mucha gente cree que es Sídney."]);
        for (text, first) in [
            ("¡Hola, Marcos!", "¡Hola, Marcos!"),
            ("¿Todo bien?", "¿Todo bien?"),
        ] {
            assert_eq!(SentenceBuffer::default().push(text), vec![first]);
        }
        // A decimal point, an abbreviation or an initial may go on.
        for text in [
            "Son 3.",
            "Hablé con el Sr.",
            "Traé pan, etc.",
            "Lo escribió J.",
        ] {
            assert!(SentenceBuffer::default().push(text).is_empty(), "{text}");
        }
        // An ellipsis cut after its first dot loses nothing but dots.
        assert_eq!(
            feed_with(SentenceBuffer::default(), &["Mmm.", ".. a ver.", " Sí."]),
            vec!["Mmm.", "a ver.", "Sí."]
        );
    }

    #[test]
    fn only_the_first_chunk_of_a_turn_is_a_clause() {
        let mut buffer = SentenceBuffer::default();
        assert_eq!(buffer.push("Hola."), vec!["Hola."]);
        // After a tool call, the turn goes on with whole sentences.
        assert!(
            buffer
                .push("Ya lo busqué en tu agenda, y tenés dos cosas")
                .is_empty()
        );
        assert_eq!(
            buffer.push(" el jueves. "),
            vec!["Ya lo busqué en tu agenda, y tenés dos cosas el jueves."]
        );
    }
}
