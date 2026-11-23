//! A cheap guard for transcripts that are probably not Spanish.
//!
//! Parakeet v3 picks the language on its own and takes no hint, so a short or
//! noisy take sometimes comes back in French or English ("Alors oui, mais
//! vous"). Sending that to the brain wastes a turn; asking the owner to repeat
//! is better. The guard counts function words (articles, pronouns,
//! conjunctions) that belong to one language and not to Spanish, plus letters
//! Spanish never uses, and compares that with the Spanish evidence. Content
//! words do not count, so English tech words inside a Spanish sentence ("hacé
//! el push del branch") pass. When in doubt it lets the text through: a
//! rejected Spanish request is worse than a foreign one reaching the brain.

use wawi_core::{Confirmation, is_hard_stop, parse_confirmation};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    French,
    English,
    Italian,
    Portuguese,
    German,
}

/// Foreign evidence needed before a transcript is rejected.
const MIN_FOREIGN: usize = 2;
/// The foreign evidence must be more than this many times the Spanish one, so
/// an English song title inside a Spanish request ("Poné Here comes the sun")
/// still passes.
const SPANISH_WEIGHT: usize = 2;

/// Spanish function words. Words another language uses in a different role,
/// like "o" (the Portuguese "the"), are left out: they prove nothing.
const SPANISH: &[&str] = &[
    "el", "la", "los", "las", "lo", "un", "una", "unos", "unas", "de", "del", "al", "a", "que",
    "qué", "y", "en", "es", "son", "no", "sí", "si", "por", "para", "con", "sin", "me", "te", "se",
    "le", "les", "nos", "mi", "mis", "tu", "tus", "su", "sus", "yo", "vos", "usted", "él", "ella",
    "ellos", "eso", "esto", "esta", "este", "está", "estás", "estoy", "están", "hay", "hola",
    "dale", "che", "bueno", "pero", "más", "muy", "ya", "también", "cómo", "como", "cuándo",
    "cuando", "dónde", "donde", "cuál", "cuánto", "porque", "porqué", "tengo", "tenés", "podés",
    "querés", "sos", "soy", "fue", "hace", "hacé", "decime", "contame", "gracias", "ahora", "hoy",
    "mañana", "ayer", "nada", "algo", "todo", "bien", "mal", "acá", "ahí", "allá", "entonces",
    "cuanto", "quiero", "puedo", "vamos", "ver", "eh", "mirá", "fijate", "poné",
];

const FRENCH: &[&str] = &[
    "je",
    "j'ai",
    "tu",
    "il",
    "elle",
    "nous",
    "vous",
    "ils",
    "elles",
    "oui",
    "non",
    "mais",
    "alors",
    "est",
    "c'est",
    "et",
    "les",
    "des",
    "du",
    "le",
    "une",
    "pas",
    "ne",
    "qui",
    "pour",
    "avec",
    "très",
    "être",
    "avoir",
    "suis",
    "sont",
    "ça",
    "cette",
    "ce",
    "dans",
    "sur",
    "au",
    "aux",
    "merci",
    "bonjour",
    "salut",
    "voilà",
    "quoi",
    "pourquoi",
    "comment",
    "oh",
    "bon",
    "peut",
    "fait",
    "faire",
    "aussi",
    "moi",
    "toi",
    "mon",
    "ma",
    "ton",
    "votre",
    "notre",
    "qu'est-ce",
    "beaucoup",
    "rien",
    "tout",
    "était",
];

const ENGLISH: &[&str] = &[
    "the", "and", "is", "are", "was", "were", "you", "i", "it", "it's", "i'm", "to", "of", "what",
    "could", "would", "should", "just", "this", "that", "with", "do", "does", "my", "your", "have",
    "has", "can", "how", "we", "they", "be", "for", "on", "in", "not", "but", "so", "she", "there",
    "about", "know", "think", "want", "will", "gonna", "like", "don't", "thank", "thanks", "hello",
    "hi", "yeah", "oh", "okay", "right", "really", "here", "where", "why", "who", "if", "at", "an",
    "or", "get", "going", "because", "well", "all",
];

const ITALIAN: &[&str] = &[
    "il",
    "non",
    "sono",
    "gli",
    "della",
    "dello",
    "questo",
    "questa",
    "molto",
    "grazie",
    "perché",
    "anche",
    "però",
    "ciao",
    "io",
    "ho",
    "hai",
    "è",
    "per",
    "bene",
    "voglio",
    "sei",
    "siamo",
    "come",
    "cosa",
    "allora",
    "adesso",
    "niente",
    "tutto",
    "buongiorno",
    "prego",
    "sì",
    "di",
    "nel",
    "alla",
    "lei",
    "lui",
    "noi",
    "voi",
    "loro",
    "so",
    "sai",
];

const PORTUGUESE: &[&str] = &[
    "não", "você", "vocês", "eu", "é", "isso", "isto", "muito", "obrigado", "obrigada", "também",
    "uma", "com", "do", "da", "dos", "das", "ele", "ela", "pra", "tá", "né", "vou", "tem", "acho",
    "então", "agora", "aqui", "nós", "meu", "minha", "seu", "sua", "olá", "tudo", "bem", "mas",
    "mais", "foi", "estou", "está", "fazer", "coisa", "nada", "sim", "bom", "dia", "sei", "quer",
    "dizer", "falar", "fala", "gente", "ainda",
];

const GERMAN: &[&str] = &[
    "ich", "und", "nicht", "das", "ist", "der", "die", "ein", "eine", "sie", "wir", "auch", "mit",
    "auf", "für", "aber", "was", "wie", "mir", "mich", "bitte", "danke", "ja", "nein", "kann",
    "es", "du", "er", "zu", "den", "dem", "haben", "habe", "sind", "noch", "jetzt", "hallo", "gut",
    "so", "doch", "schon", "oder",
];

/// Letters a language writes and Spanish never does.
fn marked(language: Language, word: &str) -> bool {
    let letters: &[char] = match language {
        Language::French => &['è', 'ê', 'à', 'ç', 'ù', 'œ', 'â', 'î', 'ô', 'û', 'ë', 'ï'],
        Language::English => &[],
        Language::Italian => &['è', 'ì', 'ò', 'à', 'ù'],
        Language::Portuguese => &['ã', 'õ', 'ç', 'ê', 'â', 'ô', 'à'],
        Language::German => &['ß', 'ä', 'ö'],
    };
    word.contains(letters)
}

fn words_of(language: Language) -> &'static [&'static str] {
    match language {
        Language::French => FRENCH,
        Language::English => ENGLISH,
        Language::Italian => ITALIAN,
        Language::Portuguese => PORTUGUESE,
        Language::German => GERMAN,
    }
}

const LANGUAGES: [Language; 5] = [
    Language::French,
    Language::English,
    Language::Italian,
    Language::Portuguese,
    Language::German,
];

/// Lowercase words, keeping inner apostrophes ("c'est", "don't").
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace('’', "'")
        .split(|c: char| !(c.is_alphabetic() || c == '\'' || c == '-'))
        .map(|word| word.trim_matches(|c| c == '\'' || c == '-'))
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// How much `text` looks like Spanish, and like each other language.
fn scores(text: &str) -> (usize, [usize; 5]) {
    let words = words(text);
    let mut spanish = text.matches(['¿', '¡']).count();
    let mut foreign = [0; 5];
    for word in &words {
        let is_spanish = SPANISH.contains(&word.as_str());
        if is_spanish || word.contains('ñ') {
            spanish += 1;
        }
        for (index, language) in LANGUAGES.iter().enumerate() {
            // A word Spanish shares ("de", "la", "no") is no evidence.
            if !is_spanish
                && (words_of(*language).contains(&word.as_str()) || marked(*language, word))
            {
                foreign[index] += 1;
            }
        }
    }
    (spanish, foreign)
}

/// The language `text` is probably in, if it is probably not Spanish. `None`
/// means "let it through": Spanish, too short to tell, or something the core
/// understands in any language ("ok", "stop", "do it").
pub fn foreign_language(text: &str) -> Option<Language> {
    if parse_confirmation(text) != Confirmation::Unclear || is_hard_stop(text) {
        return None;
    }
    let (spanish, foreign) = scores(text);
    let (index, &best) = foreign
        .iter()
        .enumerate()
        .max_by_key(|&(index, score)| (*score, std::cmp::Reverse(index)))?;
    (best >= MIN_FOREIGN && best > SPANISH_WEIGHT * spanish).then_some(LANGUAGES[index])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spanish_passes_tech_words_and_all() {
        for text in [
            "Hola Wawi, ¿cómo andás?",
            "Me parece que está en francés.",
            "Estás andando rapidísimo, me parece genial la velocidad que tenés para responder ahora.",
            "Nada, ahora te vamos a dar un poco más de fuerza y de cuerpo para después seguir \
             trabajando, pero ahora sos una i, sos un cli corriendo en máquina, nada más.",
            "Cabro si puedo hacer que te detengas cuando está hablando, a ver. I could just",
            "Ahora quiero ver si puedes cortarte.",
            "Ah, te corté.",
            "Hacé el push del branch a main y abrí el pull request.",
            "Fijate el status del build en GitHub.",
            "Poné Let it be de los Beatles.",
            "Poné Here comes the sun.",
            "Poné The Sound of Silence de Simon and Garfunkel.",
            "Mandale un mail a John que el meeting es a las tres.",
            "Pará.",
            "Sí, dale.",
            "Okay.",
            "Do it.",
            "Stop.",
            "Thanks.",
            "Bowie",
            "",
        ] {
            assert_eq!(foreign_language(text), None, "{text}");
        }
    }

    #[test]
    fn other_languages_are_caught_even_when_short() {
        for (text, language) in [
            ("Alors oui, mais vous", Language::French),
            ("Je ne sais pas.", Language::French),
            ("Merci beaucoup.", Language::French),
            ("I could just", Language::English),
            ("Thank you.", Language::English),
            ("What is the weather like today?", Language::English),
            ("Non lo so, grazie.", Language::Italian),
            ("Io sono molto stanco.", Language::Italian),
            ("Eu não sei, obrigado.", Language::Portuguese),
            ("Você não sabe?", Language::Portuguese),
            ("Eu não o sei o que você quer dizer.", Language::Portuguese),
            ("Ich weiß es nicht.", Language::German),
            ("Das ist gut, danke.", Language::German),
        ] {
            assert_eq!(foreign_language(text), Some(language), "{text}");
        }
    }
}
