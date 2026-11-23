//! Reading the owner's words. Matching is by whole phrases over the whole
//! utterance: "no sé", "si llueve" or "para mañana" must not count as an answer
//! or a stop just because they contain one of the words.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmation {
    Yes,
    No,
    Unclear,
}

const YES: &[&str] = &[
    "si",
    "sip",
    "dale",
    "claro",
    "obvio",
    "ok",
    "okay",
    "oka",
    "confirmo",
    "confirmar",
    "afirmativo",
    "hacelo",
    "de una",
    "de acuerdo",
    "yes",
    "yeah",
    "yep",
    "sure",
    "confirm",
    "do it",
    "go ahead",
];

const NO: &[&str] = &[
    "no",
    "nop",
    "nope",
    "nah",
    "negativo",
    "cancela",
    "cancelar",
    "cancel",
    "mejor no",
    "ni loco",
    "no lo hagas",
    "no hagas",
    "dont",
    "dont do it",
    "para",
    "stop",
];

const STOP: &[&str] = &[
    "para", "pare", "stop", "basta", "ya basta", "frena", "callate", "shut up",
];

const FILLER: &[&str] = &[
    "por favor",
    "please",
    "gracias",
    "thanks",
    "wawi",
    "che",
    "eso",
];

/// Lowercases, folds Spanish accents, drops apostrophes and turns every other
/// non-alphanumeric run into one space.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        match c {
            'á' => out.push('a'),
            'é' => out.push('e'),
            'í' => out.push('i'),
            'ó' => out.push('o'),
            'ú' | 'ü' => out.push('u'),
            'ñ' => out.push('n'),
            '\'' | '’' => {}
            c if c.is_alphanumeric() => out.push(c),
            _ => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
        }
    }
    out.truncate(out.trim_end().len());
    out
}

fn phrase_len(tokens: &[&str], phrase: &str) -> Option<usize> {
    let words: Vec<&str> = phrase.split(' ').collect();
    tokens.starts_with(&words).then_some(words.len())
}

fn longest_match(tokens: &[&str], list: &[&str]) -> usize {
    list.iter()
        .filter_map(|phrase| phrase_len(tokens, phrase))
        .max()
        .unwrap_or(0)
}

pub fn parse_confirmation(text: &str) -> Confirmation {
    let normalized = normalize(text);
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let (mut said_yes, mut said_no) = (false, false);
    let mut i = 0;
    while i < tokens.len() {
        let rest = &tokens[i..];
        let (yes, no, filler) = (
            longest_match(rest, YES),
            longest_match(rest, NO),
            longest_match(rest, FILLER),
        );
        let step = yes.max(no).max(filler);
        if step == 0 {
            return Confirmation::Unclear;
        }
        said_yes |= yes == step;
        said_no |= no == step;
        i += step;
    }
    match (said_yes, said_no) {
        (true, false) => Confirmation::Yes,
        (false, true) => Confirmation::No,
        _ => Confirmation::Unclear,
    }
}

pub fn is_hard_stop(text: &str) -> bool {
    let normalized = normalize(text);
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let mut said_stop = false;
    let mut i = 0;
    while i < tokens.len() {
        let rest = &tokens[i..];
        let (stop, filler) = (longest_match(rest, STOP), longest_match(rest, FILLER));
        let step = stop.max(filler);
        if step == 0 {
            return false;
        }
        said_stop |= stop == step;
        i += step;
    }
    said_stop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_folds_case_accents_and_punctuation() {
        assert_eq!(normalize("¿Qué HORA es?"), "que hora es");
        assert_eq!(normalize("  Sí,  dale!! "), "si dale");
        assert_eq!(normalize("Don't"), "dont");
        assert_eq!(normalize("¡¿?!"), "");
    }

    #[test]
    fn yes_in_both_languages() {
        for text in [
            "sí",
            "Si.",
            "dale",
            "sí, por favor",
            "de una",
            "hacelo, wawi",
            "yes",
            "Yeah!",
            "go ahead",
            "ok, thanks",
            "sí sí",
        ] {
            assert_eq!(parse_confirmation(text), Confirmation::Yes, "{text}");
        }
    }

    #[test]
    fn no_in_both_languages() {
        for text in [
            "no",
            "No, gracias",
            "negativo",
            "mejor no",
            "no lo hagas",
            "no, no",
            "nope",
            "don't do it",
            "cancel",
            "no, pará",
        ] {
            assert_eq!(parse_confirmation(text), Confirmation::No, "{text}");
        }
    }

    #[test]
    fn anything_else_is_unclear() {
        for text in [
            "",
            "no sé",
            "si llueve",
            "sí, pero no",
            "dale no",
            "yesterday",
            "nothing",
            "sin duda",
            "por favor",
            "bueno",
            "qué hora es",
        ] {
            assert_eq!(parse_confirmation(text), Confirmation::Unclear, "{text}");
        }
    }

    #[test]
    fn hard_stop_only_as_the_whole_utterance() {
        for text in [
            "pará",
            "Para!",
            "stop",
            "stop please",
            "pará, wawi",
            "basta",
        ] {
            assert!(is_hard_stop(text), "{text}");
        }
        for text in [
            "",
            "para mañana",
            "no pares",
            "stop the music",
            "gracias",
            "un stopper",
        ] {
            assert!(!is_hard_stop(text), "{text}");
        }
    }
}
