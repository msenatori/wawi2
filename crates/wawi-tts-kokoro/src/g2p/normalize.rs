//! Written text to speakable words: numbers, times, dates, money, symbols,
//! abbreviations, acronyms and common English words become plain Spanish
//! words, and markdown, emoji and the like are dropped.

use super::numbers;

/// What the phonemizer reads: a lowercase word, or punctuation the voice
/// pauses on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Word(String),
    Punct(char),
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    /// As written, case kept (acronyms need it).
    Word(String),
    /// Digits, maybe with inner `.`, `,`, `:` or `/` between digits.
    Number(String),
    Punct(char),
    Symbol(char),
}

/// Kept as pauses. `…` is in Kokoro's vocabulary; `...` is folded into it.
fn is_punct(c: char) -> bool {
    matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | '…')
}

fn is_symbol(c: char) -> bool {
    matches!(
        c,
        '%' | '$' | '€' | '&' | '+' | '@' | '°' | 'º' | 'ª' | '=' | '-' | '/' | '#'
    )
}

fn is_word_char(c: char) -> bool {
    // º and ª count as letters in Unicode; here they mark ordinals.
    c.is_alphabetic() && !matches!(c, 'º' | 'ª')
}

fn tokenize(text: &str) -> Vec<Token> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if is_word_char(c) {
            let start = i;
            while i < chars.len()
                && (is_word_char(chars[i])
                    // "o'clock", "Charly's": keep the apostrophe inside a word.
                    || (matches!(chars[i], '\'' | '’')
                        && chars.get(i + 1).is_some_and(|n| is_word_char(*n))))
            {
                i += 1;
            }
            let word: String = chars[start..i]
                .iter()
                .filter(|c| !matches!(c, '\'' | '’'))
                .collect();
            tokens.push(Token::Word(word));
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || (matches!(chars[i], '.' | ',' | ':' | '/')
                        && chars.get(i + 1).is_some_and(char::is_ascii_digit)))
            {
                i += 1;
            }
            tokens.push(Token::Number(chars[start..i].iter().collect()));
        } else if c == '.' && chars.get(i + 1) == Some(&'.') {
            while i < chars.len() && chars[i] == '.' {
                i += 1;
            }
            tokens.push(Token::Punct('…'));
        } else {
            if is_punct(c) {
                tokens.push(Token::Punct(c));
            } else if is_symbol(c) {
                tokens.push(Token::Symbol(c));
            } else if matches!(c, '(' | ')' | '—' | '–' | '[' | ']' | '{' | '}') {
                // Asides read as a short pause.
                tokens.push(Token::Punct(','));
            }
            // Anything else (quotes, markdown, emoji, ¿ ¡) is not spoken.
            i += 1;
        }
    }
    tokens
}

/// Abbreviations that are only expanded when followed by a period.
const DOTTED: &[(&str, &str)] = &[
    ("Sr", "señor"),
    ("Sra", "señora"),
    ("Srta", "señorita"),
    ("Sres", "señores"),
    ("Dr", "doctor"),
    ("Dra", "doctora"),
    ("Lic", "licenciado"),
    ("Ing", "ingeniero"),
    ("Prof", "profesor"),
    ("Ud", "usted"),
    ("Uds", "ustedes"),
    ("etc", "etcétera"),
    ("ej", "ejemplo"),
    ("Av", "avenida"),
    ("av", "avenida"),
    ("pág", "página"),
    ("tel", "teléfono"),
    ("Tel", "teléfono"),
    ("aprox", "aproximadamente"),
    ("Nº", "número"),
    ("nro", "número"),
    ("Nro", "número"),
    ("min", "minutos"),
    ("seg", "segundos"),
];

/// Whether `word` (as written, without its period) is an abbreviation that
/// takes one: "Sr", "etc".
pub fn is_abbreviation(word: &str) -> bool {
    DOTTED.iter().any(|(w, _)| *w == word)
}

/// Units after a number: (written, singular, plural).
const UNITS: &[(&str, &str, &str)] = &[
    ("km", "kilómetro", "kilómetros"),
    ("kg", "kilo", "kilos"),
    ("g", "gramo", "gramos"),
    ("gr", "gramo", "gramos"),
    ("cm", "centímetro", "centímetros"),
    ("mm", "milímetro", "milímetros"),
    ("m", "metro", "metros"),
    ("ml", "mililitro", "mililitros"),
    ("l", "litro", "litros"),
    ("h", "hora", "horas"),
    ("hs", "hora", "horas"),
    ("min", "minuto", "minutos"),
    ("seg", "segundo", "segundos"),
    ("s", "segundo", "segundos"),
    ("ms", "milisegundo", "milisegundos"),
    ("kb", "ka be", "ka bes"),
    ("mb", "mega", "megas"),
    ("gb", "giga", "gigas"),
    ("tb", "tera", "teras"),
    ("k", "ka", "ka"),
];

/// Words that keep their spelling but are said the English way, respelled
/// as an Argentine would read them out loud.
const ENGLISH: &[(&str, &str)] = &[
    ("app", "ap"),
    ("apps", "aps"),
    ("bluetooth", "blutút"),
    ("bye", "bai"),
    ("chrome", "crom"),
    ("claude", "clod"),
    ("click", "clic"),
    ("cool", "cul"),
    ("download", "dáunloud"),
    ("email", "imeil"),
    ("emails", "imeils"),
    ("facebook", "féisbuk"),
    ("feedback", "fídbak"),
    ("github", "guítjab"),
    ("gmail", "yimeil"),
    ("google", "gúgel"),
    ("hardware", "járdguer"),
    ("hello", "jelóu"),
    ("instagram", "ínstagram"),
    ("iphone", "áifon"),
    ("ipad", "áipad"),
    ("laptop", "láptop"),
    ("like", "laik"),
    ("link", "linc"),
    ("links", "lincs"),
    ("login", "loguin"),
    ("mac", "mac"),
    ("macbook", "mácbuk"),
    ("mail", "meil"),
    ("mails", "meils"),
    ("meeting", "míting"),
    ("netflix", "nétflis"),
    ("notebook", "nóutbuk"),
    ("ok", "oquéi"),
    ("okay", "oquéi"),
    ("okey", "oquéi"),
    ("online", "onláin"),
    ("orca", "orca"),
    ("password", "pásguord"),
    ("play", "pléi"),
    ("playlist", "pléilist"),
    ("podcast", "pódcast"),
    ("router", "ráuter"),
    ("show", "chou"),
    ("slack", "eslac"),
    ("smartphone", "esmártfon"),
    ("software", "sófguer"),
    ("sorry", "sóri"),
    ("spotify", "espotifái"),
    ("streaming", "estríming"),
    ("thanks", "tenks"),
    ("tweet", "tuit"),
    ("twitter", "túiter"),
    ("update", "ápdeit"),
    ("web", "güeb"),
    ("weekend", "uíquend"),
    ("whatsapp", "guatsáp"),
    ("wifi", "guaifái"),
    ("windows", "guíndous"),
    ("youtube", "iutúb"),
    ("zoom", "zum"),
];

/// Letter names, for acronyms that are spelled out ("USB" -> "u ese be").
fn letter_name(c: char) -> &'static str {
    match c.to_lowercase().next().unwrap_or(c) {
        'a' | 'á' => "á",
        'b' => "be",
        'c' => "ce",
        'd' => "de",
        'e' | 'é' => "é",
        'f' => "efe",
        'g' => "ge",
        'h' => "hache",
        'i' | 'í' => "í",
        'j' => "jota",
        'k' => "ka",
        'l' => "ele",
        'm' => "eme",
        'n' => "ene",
        'ñ' => "eñe",
        'o' | 'ó' => "ó",
        'p' => "pe",
        'q' => "cu",
        'r' => "erre",
        's' => "ese",
        't' => "te",
        'u' | 'ú' | 'ü' => "ú",
        'v' => "ve",
        'w' => "doble ve",
        'x' => "equis",
        'y' => "ye",
        'z' => "zeta",
        _ => "",
    }
}

fn is_vowel_letter(c: char) -> bool {
    matches!(
        c.to_lowercase().next().unwrap_or(c),
        'a' | 'e' | 'i' | 'o' | 'u' | 'á' | 'é' | 'í' | 'ó' | 'ú'
    )
}

/// An all-caps word that reads as a word ("NASA", "AFIP") rather than letter
/// by letter ("USB", "HTML"): four letters or more, at least two vowels and
/// never three consonants in a row.
fn pronounceable(word: &str) -> bool {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() < 4 || chars.iter().filter(|c| is_vowel_letter(**c)).count() < 2 {
        return false;
    }
    let mut run = 0;
    for c in &chars {
        run = if is_vowel_letter(*c) { 0 } else { run + 1 };
        if run >= 3 {
            return false;
        }
    }
    true
}

fn push_words(out: &mut Vec<Piece>, words: &str) {
    for word in words.split_whitespace() {
        out.push(Piece::Word(word.to_owned()));
    }
}

/// Spanish words for one written word.
fn expand_word(word: &str, out: &mut Vec<Piece>) {
    let lower = word.to_lowercase();
    if let Some((_, said)) = ENGLISH.iter().find(|(w, _)| *w == lower) {
        push_words(out, said);
        return;
    }
    let letters = word.chars().count();
    let all_caps = letters >= 2 && word.chars().all(|c| c.is_uppercase());
    let lone_consonant = letters == 1 && !matches!(lower.as_str(), "a" | "e" | "o" | "u" | "y");
    if (all_caps && !pronounceable(word)) || lone_consonant {
        for c in word.chars() {
            push_words(out, letter_name(c));
        }
        return;
    }
    out.push(Piece::Word(lower));
}

/// Words for a number token, given the word that follows it (for "un"/"una"
/// and units).
fn expand_number(raw: &str, next_word: Option<&str>) -> String {
    if let Some(words) = time_of_day(raw).or_else(|| date(raw)) {
        return words;
    }
    let (int_part, decimals) = split_decimal(raw);
    let Some(int_words) = integer(&int_part) else {
        return numbers::digits(raw);
    };
    match decimals {
        Some(decimals) => format!("{int_words} coma {}", decimal_words(&decimals)),
        None => agree(&int_words, next_word),
    }
}

/// "uno" agrees with the noun after it: "1 hora" -> "una hora".
fn agree(words: &str, next_word: Option<&str>) -> String {
    match next_word {
        Some(next) if words.ends_with("uno") => {
            let next = next.to_lowercase();
            if next.ends_with('a') || next.ends_with("as") {
                numbers::feminine(words)
            } else {
                numbers::apocope(words)
            }
        }
        _ => words.to_owned(),
    }
}

/// Integer digits, possibly with thousands dots already removed.
fn integer(digits: &str) -> Option<String> {
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // "007", "0800": leading zeros are read one by one.
    if digits.len() > 1 && digits.starts_with('0') {
        return Some(numbers::digits(digits));
    }
    digits
        .parse::<u64>()
        .ok()
        .and_then(numbers::cardinal)
        .or_else(|| Some(numbers::digits(digits)))
}

fn decimal_words(decimals: &str) -> String {
    let zeros = decimals.chars().take_while(|c| *c == '0').count();
    let rest = &decimals[zeros..];
    let mut words: Vec<String> = vec!["cero".to_owned(); zeros];
    if !rest.is_empty() {
        if rest.len() <= 3 {
            words.push(integer(rest).unwrap_or_default());
        } else {
            words.push(numbers::digits(rest));
        }
    }
    words.join(" ")
}

/// Splits "1.250,5" into ("1250", Some("5")). The comma is the decimal mark.
/// A dot groups thousands when every group after the first has three digits
/// ("1.250"); any other dot is a decimal mark too ("3.5").
fn split_decimal(raw: &str) -> (String, Option<String>) {
    let (int_part, decimals) = match raw.rsplit_once(',') {
        Some((int_part, decimals)) => (int_part, Some(decimals.to_owned())),
        None => (raw, None),
    };
    let groups: Vec<&str> = int_part.split('.').collect();
    let thousands = groups.len() > 1
        && (1..=3).contains(&groups[0].len())
        && groups[1..].iter().all(|g| g.len() == 3);
    if thousands || groups.len() == 1 {
        return (groups.concat(), decimals);
    }
    match (int_part.split_once('.'), decimals) {
        (Some((whole, fraction)), None) => (whole.to_owned(), Some(fraction.replace('.', ""))),
        _ => (groups.concat(), None),
    }
}

/// "10:30" -> "diez y media". Only H:MM with a valid hour and minute.
fn time_of_day(raw: &str) -> Option<String> {
    let (h, m) = raw.split_once(':')?;
    if m.len() != 2 || m.contains(':') {
        return None;
    }
    let (h, m): (u64, u64) = (h.parse().ok()?, m.parse().ok()?);
    if h > 24 || m > 59 {
        return None;
    }
    let hour = if h == 1 {
        "una".to_owned()
    } else {
        numbers::feminine(&numbers::cardinal(h)?)
    };
    Some(match m {
        0 => hour,
        15 => format!("{hour} y cuarto"),
        30 => format!("{hour} y media"),
        _ => format!("{hour} y {}", numbers::cardinal(m)?),
    })
}

/// "20/11" or "20/11/2026" -> "veinte de noviembre (de dos mil veintiséis)".
fn date(raw: &str) -> Option<String> {
    let parts: Vec<&str> = raw.split('/').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let day: u64 = parts[0].parse().ok()?;
    let month: usize = parts[1].parse().ok()?;
    if !(1..=31).contains(&day) || !(1..=12).contains(&month) {
        return None;
    }
    let day_words = if day == 1 {
        "primero".to_owned()
    } else {
        numbers::cardinal(day)?
    };
    let mut out = format!("{day_words} de {}", numbers::MONTHS[month - 1]);
    if let Some(year) = parts.get(2) {
        let mut year: u64 = year.parse().ok()?;
        if year < 100 {
            year += 2000;
        }
        out.push_str(&format!(" de {}", numbers::cardinal(year)?));
    }
    Some(out)
}

fn unit(word: &str) -> Option<(&'static str, &'static str)> {
    let lower = word.to_lowercase();
    UNITS
        .iter()
        .find(|(w, _, _)| *w == lower)
        .map(|(_, one, many)| (*one, *many))
}

fn word_at(tokens: &[Token], i: usize) -> Option<&str> {
    match tokens.get(i) {
        Some(Token::Word(w)) => Some(w),
        _ => None,
    }
}

/// The whole pass: text in, words and pauses out.
pub fn normalize(text: &str) -> Vec<Piece> {
    let tokens = tokenize(text);
    let mut out: Vec<Piece> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Word(word) => {
                let dotted = matches!(tokens.get(i + 1), Some(Token::Punct('.')));
                if dotted && let Some((_, said)) = DOTTED.iter().find(|(w, _)| w == word) {
                    push_words(&mut out, said);
                    i += 2;
                    // At the very end the period still closes the sentence.
                    if i >= tokens.len() {
                        out.push(Piece::Punct('.'));
                    }
                    continue;
                }
                // "U$S", "US$", "USD" before or after an amount.
                if matches!(word.as_str(), "USD" | "US" | "U")
                    && matches!(tokens.get(i + 1), Some(Token::Symbol('$')))
                {
                    i += 2;
                    if word_at(&tokens, i) == Some("S") {
                        i += 1;
                    }
                    if let Some(Token::Number(raw)) = tokens.get(i) {
                        let n = expand_number(raw, Some("dólares"));
                        push_words(&mut out, &money(&n, "dólar", "dólares"));
                        i += 1;
                    } else {
                        push_words(&mut out, "dólares");
                    }
                    continue;
                }
                if word == "N" && matches!(tokens.get(i + 1), Some(Token::Symbol('º' | '°'))) {
                    push_words(&mut out, "número");
                    i += 2;
                    continue;
                }
                expand_word(word, &mut out);
                i += 1;
            }
            Token::Number(raw) => {
                i += expand_number_token(&tokens, i, raw, &mut out);
            }
            Token::Punct(c) => {
                let c = *c;
                // Collapse runs of pauses into the strongest one.
                match out.last_mut() {
                    Some(Piece::Punct(prev)) => {
                        if c != ',' {
                            *prev = c;
                        }
                    }
                    Some(_) => out.push(Piece::Punct(c)),
                    None => {}
                }
                i += 1;
            }
            Token::Symbol(c) => {
                i += expand_symbol(&tokens, i, *c, &mut out);
            }
        }
    }
    out
}

fn money(amount: &str, one: &str, many: &str) -> String {
    let amount = numbers::apocope(amount);
    if amount == "un" {
        format!("un {one}")
    } else if amount.ends_with("millón") || amount.ends_with("millones") {
        format!("{amount} de {many}")
    } else {
        format!("{amount} {many}")
    }
}

/// Handles a number and whatever modifies it right after (`%`, `º`, units).
/// Returns how many tokens it used.
fn expand_number_token(tokens: &[Token], i: usize, raw: &str, out: &mut Vec<Piece>) -> usize {
    match tokens.get(i + 1) {
        Some(Token::Symbol('%')) => {
            let n = expand_number(raw, None);
            push_words(out, &format!("{n} por ciento"));
            return 2;
        }
        Some(Token::Symbol(c @ ('º' | 'ª' | '°'))) => {
            let n: Option<u64> = raw.parse().ok();
            let words = match (c, n) {
                ('°', Some(n)) if n != 1 => format!("{} grados", expand_number(raw, None)),
                (_, Some(n)) => numbers::ordinal(n, *c == 'ª').unwrap_or_default(),
                _ => expand_number(raw, None),
            };
            push_words(out, &words);
            // "25°C"
            if *c == '°' && word_at(tokens, i + 2).is_some_and(|w| w == "C") {
                return 3;
            }
            return 2;
        }
        Some(Token::Symbol('-')) => {
            // "10-20": a range.
            if let Some(Token::Number(_)) = tokens.get(i + 2) {
                push_words(out, &expand_number(raw, None));
                push_words(out, "a");
                return 2;
            }
        }
        _ => {}
    }
    if let Some(word) = word_at(tokens, i + 1)
        && let Some((one, many)) = unit(word)
    {
        let n = expand_number(raw, Some(one));
        let is_one = n == "un" || n == "una" || n == "uno";
        let n = if is_one { "un".to_owned() } else { n };
        push_words(out, &format!("{n} {}", if is_one { one } else { many }));
        return 2;
    }
    push_words(out, &expand_number(raw, word_at(tokens, i + 1)));
    1
}

/// Returns how many tokens it used.
fn expand_symbol(tokens: &[Token], i: usize, c: char, out: &mut Vec<Piece>) -> usize {
    match c {
        '$' | '€' => {
            let (one, many) = if c == '$' {
                ("peso", "pesos")
            } else {
                ("euro", "euros")
            };
            if let Some(Token::Number(raw)) = tokens.get(i + 1) {
                let n = expand_number(raw, Some(many));
                push_words(out, &money(&n, one, many));
                return 2;
            }
            push_words(out, many);
        }
        '%' => push_words(out, "por ciento"),
        '&' => push_words(out, "y"),
        '+' => push_words(out, "más"),
        '=' => push_words(out, "igual a"),
        '@' => push_words(out, "arroba"),
        '#' => {}
        '-' => {
            // "-5" at the start or after a space: a negative number.
            let starts_number = matches!(tokens.get(i + 1), Some(Token::Number(_)));
            if starts_number && !matches!(tokens.get(i.wrapping_sub(1)), Some(Token::Number(_))) {
                push_words(out, "menos");
            }
        }
        // "y/o" reads "y o"; a slash anywhere else is not spoken.
        '/' => {}
        _ => {}
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(text: &str) -> String {
        normalize(text)
            .iter()
            .map(|p| match p {
                Piece::Word(w) => format!(" {w}"),
                Piece::Punct(c) => c.to_string(),
            })
            .collect::<String>()
            .trim()
            .to_owned()
    }

    #[test]
    fn plain_text_is_lowercased_and_marks_dropped() {
        assert_eq!(
            said("¿Querés que te lea las noticias?"),
            "querés que te lea las noticias?"
        );
        assert_eq!(said("**Hola**, che 😀!"), "hola, che!");
        assert_eq!(said("Mirá... no sé."), "mirá… no sé.");
    }

    #[test]
    fn numbers_times_and_dates() {
        assert_eq!(said("Tengo 25 años."), "tengo veinticinco años.");
        assert_eq!(said("Son las 10:30."), "son las diez y media.");
        assert_eq!(said("A las 21:05"), "a las veintiuna y cinco");
        assert_eq!(
            said("Hoy es 20/11/2026."),
            "hoy es veinte de noviembre de dos mil veintiséis."
        );
        assert_eq!(said("Pesa 3,5 kg"), "pesa tres coma cinco kilos");
        assert_eq!(said("1.250 pesos"), "mil doscientos cincuenta pesos");
        assert_eq!(said("1 hora y 21 minutos"), "una hora y veintiún minutos");
        assert_eq!(said("El 100%"), "el cien por ciento");
        assert_eq!(said("$1.500"), "mil quinientos pesos");
        assert_eq!(said("US$ 1"), "un dólar");
        assert_eq!(said("Hace 25°C"), "hace veinticinco grados");
        assert_eq!(said("el 2º piso"), "el segundo piso");
        assert_eq!(said("de 10-20 personas"), "de diez a veinte personas");
        assert_eq!(said("-5 grados"), "menos cinco grados");
        assert_eq!(said("1 km"), "un kilómetro");
    }

    #[test]
    fn abbreviations_acronyms_and_english() {
        assert_eq!(said("El Sr. Pérez, etc."), "el señor pérez, etcétera.");
        assert_eq!(said("Conectá el USB"), "conectá el ú ese be");
        assert_eq!(said("la NASA y la AFIP"), "la nasa y la afip");
        assert_eq!(said("Abrí WhatsApp"), "abrí guatsáp");
        assert_eq!(said("OK, dale"), "oquéi, dale");
        assert_eq!(said("la letra B"), "la letra be");
        assert_eq!(said("y/o"), "y o");
        assert_eq!(said("el Nº 5"), "el número cinco");
    }
}
