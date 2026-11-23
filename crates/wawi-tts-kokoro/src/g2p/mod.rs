//! Spanish text to the phonemes Kokoro reads.
//!
//! Kokoro was trained on espeak-ng's IPA, so this produces the same symbols
//! and conventions espeak-ng uses for Spanish (stress mark right before the
//! stressed vowel, `β ð ɣ` between vowels, `ɪ ʊ` for falling diphthongs,
//! `ɛ` in stressed syllables closed by a nasal), with rules instead of
//! espeak-ng itself. Spanish spelling is regular enough for that, and it keeps
//! GPL code out of the binary.
//!
//! [`Accent::Castilian`], the default, reproduces what Wawi 1 got from
//! espeak-ng's `es` voice (`θ` for "z"/"ce", `ʎ` for "ll"): the owner chose
//! to keep Wawi 1's sound. [`Accent::Rioplatense`] says them the way the owner
//! does: "s" for both and `ʃ` for "ll" and "y" ("ʃo", "kaʃe").

pub mod normalize;
pub mod numbers;

use normalize::Piece;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Accent {
    /// Seseo and sheísmo: "zapato" with s, "calle" and "yo" with ʃ.
    Rioplatense,
    /// Seseo, "ll" and "y" as ʝ: most of Latin America.
    Neutral,
    /// θ for "z"/"ce"/"ci", ʎ for "ll": what espeak-ng's `es` (and so Wawi 1)
    /// produces. The default: it is how Wawi 1 sounded.
    #[default]
    Castilian,
}

impl Accent {
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "rioplatense" | "ar" | "es-ar" => Some(Accent::Rioplatense),
            "neutral" | "latam" | "es-419" => Some(Accent::Neutral),
            "castilian" | "castellano" | "es" | "es-es" => Some(Accent::Castilian),
            _ => None,
        }
    }

    fn zeta(self) -> &'static str {
        match self {
            Accent::Castilian => "θ",
            _ => "s",
        }
    }

    fn elle(self) -> &'static str {
        match self {
            Accent::Rioplatense => "ʃ",
            Accent::Neutral => "ʝ",
            Accent::Castilian => "ʎ",
        }
    }

    fn ye(self) -> &'static str {
        match self {
            Accent::Rioplatense => "ʃ",
            _ => "ʝ",
        }
    }
}

/// Text (any sentence) to a phoneme string.
pub fn phonemize(text: &str, accent: Accent) -> String {
    let pieces = normalize::normalize(text);
    let mut items: Vec<Item> = Vec::new();
    for piece in &pieces {
        match piece {
            Piece::Word(word) => {
                if !items.is_empty() {
                    items.push(Item::Space);
                }
                word_items(word, accent, &mut items);
            }
            Piece::Punct(c) => {
                if !items.is_empty() {
                    items.push(Item::Pause(*c));
                }
            }
        }
    }
    render(&items)
}

/// One sound or marker of the phrase, before context is applied.
#[derive(Clone, Debug, PartialEq)]
enum Item {
    /// A sound with no context rules.
    Fixed(&'static str),
    /// b/v: `b` after a pause or a nasal, `β` elsewhere.
    B,
    /// d: `d` after a pause or a nasal, `ð` elsewhere.
    D,
    /// g: `ɡ` after a pause or a nasal, `ɣ` elsewhere.
    G,
    /// n: `m` before b/p, `ŋ` before g/x.
    N,
    /// A vowel; stressed ones carry their mark.
    Vowel(char, Stress),
    Space,
    Pause(char),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stress {
    None,
    Primary,
    Secondary,
}

/// A written vowel, before syllables are worked out.
#[derive(Clone, Copy, Debug)]
struct V {
    base: char,
    accented: bool,
    /// A final "y" ("muy", "estoy").
    from_y: bool,
}

impl V {
    fn weak(&self) -> bool {
        matches!(self.base, 'i' | 'u') && !self.accented
    }
}

#[derive(Clone, Debug)]
enum Unit {
    C(Item),
    V(V),
}

/// Unstressed little words: articles, clitics, prepositions, conjunctions.
const UNSTRESSED: &[&str] = &[
    "el", "la", "los", "las", "lo", "le", "les", "me", "te", "se", "nos", "os", "mi", "mis", "tu",
    "tus", "su", "sus", "de", "del", "a", "al", "en", "con", "por", "sin", "que", "y", "e", "o",
    "u", "i", "ni", "si",
];

/// Function words espeak-ng gives only a secondary stress.
const WEAKLY_STRESSED: &[&str] = &["para", "sobre", "como", "cuando", "hasta"];

/// Words whose spelling misleads the rules.
const EXCEPTIONS: &[(&str, &str)] = &[
    ("méxico", "méjico"),
    ("mexicano", "mejicano"),
    ("mexicana", "mejicana"),
    ("texas", "tejas"),
    ("oaxaca", "oajaca"),
    ("y", "i"),
];

fn vowel(base: char, accented: bool) -> Unit {
    Unit::V(V {
        base,
        accented,
        from_y: false,
    })
}

fn is_vowel_char(c: char) -> bool {
    matches!(
        c,
        'a' | 'e'
            | 'i'
            | 'o'
            | 'u'
            | 'á'
            | 'é'
            | 'í'
            | 'ó'
            | 'ú'
            | 'ü'
            | 'à'
            | 'è'
            | 'ì'
            | 'ò'
            | 'ù'
    )
}

fn front(c: Option<&char>) -> bool {
    matches!(c, Some('e' | 'i' | 'é' | 'í' | 'è' | 'ì'))
}

/// Spelling to sounds, inside one word.
fn units(word: &str, accent: Accent) -> Vec<Unit> {
    let chars: Vec<char> = word.chars().collect();
    let mut out: Vec<Unit> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1);
        let mut step = 1;
        // English-style doubled letters ("app", "zz") are said once.
        if next == Some(&c)
            && matches!(c, 'b' | 'd' | 'f' | 'g' | 'k' | 'm' | 'p' | 's' | 't' | 'z')
        {
            i += 1;
            continue;
        }
        match c {
            'a' | 'e' | 'o' => out.push(vowel(c, false)),
            'i' | 'u' => out.push(vowel(c, false)),
            'á' | 'à' => out.push(vowel('a', true)),
            'é' | 'è' => out.push(vowel('e', true)),
            'í' | 'ì' => out.push(vowel('i', true)),
            'ó' | 'ò' => out.push(vowel('o', true)),
            'ú' | 'ù' => out.push(vowel('u', true)),
            'ü' => out.push(vowel('u', false)),
            'w' => out.push(vowel('u', false)),
            'y' => {
                if next.is_some_and(|n| is_vowel_char(*n)) {
                    out.push(Unit::C(Item::Fixed(accent.ye())));
                } else {
                    out.push(Unit::V(V {
                        base: 'i',
                        accented: false,
                        from_y: true,
                    }));
                }
            }
            'h' => {}
            'c' => {
                if next == Some(&'h') {
                    out.push(Unit::C(Item::Fixed("t")));
                    out.push(Unit::C(Item::Fixed("ʃ")));
                    step = 2;
                } else if front(next) {
                    out.push(Unit::C(Item::Fixed(accent.zeta())));
                } else {
                    out.push(Unit::C(Item::Fixed("k")));
                }
            }
            'z' => out.push(Unit::C(Item::Fixed(accent.zeta()))),
            'k' => out.push(Unit::C(Item::Fixed("k"))),
            'q' => {
                out.push(Unit::C(Item::Fixed("k")));
                if next == Some(&'u') {
                    step = 2;
                }
            }
            'g' => {
                if i == 0 && next == Some(&'n') {
                    // "gnomo"
                } else if front(next) {
                    out.push(Unit::C(Item::Fixed("x")));
                } else if next == Some(&'u') && front(chars.get(i + 2)) {
                    out.push(Unit::C(Item::G));
                    step = 2;
                } else {
                    out.push(Unit::C(Item::G));
                }
            }
            'j' => out.push(Unit::C(Item::Fixed("x"))),
            'l' => {
                if next == Some(&'l') {
                    out.push(Unit::C(Item::Fixed(accent.elle())));
                    step = 2;
                } else {
                    out.push(Unit::C(Item::Fixed("l")));
                }
            }
            'r' => {
                let after_strong = matches!(
                    out.last(),
                    Some(Unit::C(Item::N | Item::Fixed("l") | Item::Fixed("s")))
                );
                if next == Some(&'r') {
                    out.push(Unit::C(Item::Fixed("r")));
                    step = 2;
                } else if out.is_empty() || after_strong {
                    out.push(Unit::C(Item::Fixed("r")));
                } else {
                    out.push(Unit::C(Item::Fixed("ɾ")));
                }
            }
            'ñ' => out.push(Unit::C(Item::Fixed("ɲ"))),
            'n' => out.push(Unit::C(Item::N)),
            'm' => out.push(Unit::C(Item::Fixed("m"))),
            'b' | 'v' => out.push(Unit::C(Item::B)),
            // espeak-ng keeps a final "d" as a stop: "ciudad", "Madrid".
            'd' if i + 1 == chars.len() => out.push(Unit::C(Item::Fixed("d"))),
            'd' => out.push(Unit::C(Item::D)),
            'p' => {
                if !(i == 0 && next == Some(&'s')) {
                    out.push(Unit::C(Item::Fixed("p")));
                }
            }
            't' => out.push(Unit::C(Item::Fixed("t"))),
            'f' => out.push(Unit::C(Item::Fixed("f"))),
            's' => {
                if next == Some(&'h') {
                    out.push(Unit::C(Item::Fixed("ʃ")));
                    step = 2;
                } else {
                    out.push(Unit::C(Item::Fixed("s")));
                }
            }
            'x' => {
                if i > 0 {
                    out.push(Unit::C(Item::Fixed("k")));
                }
                out.push(Unit::C(Item::Fixed("s")));
            }
            'ç' => out.push(Unit::C(Item::Fixed("s"))),
            _ => {}
        }
        i += step;
    }
    out
}

/// How a vowel ends up: the syllable nucleus, or a glide on one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Nucleus,
    OnGlide,
    OffGlide,
}

/// Roles for every vowel unit, by position in `units`.
fn roles(units: &[Unit]) -> Vec<Option<Role>> {
    let mut roles = vec![None; units.len()];
    let mut i = 0;
    while i < units.len() {
        if !matches!(units[i], Unit::V(_)) {
            i += 1;
            continue;
        }
        let start = i;
        while i < units.len() && matches!(units[i], Unit::V(_)) {
            i += 1;
        }
        let run: Vec<V> = units[start..i]
            .iter()
            .filter_map(|u| match u {
                Unit::V(v) => Some(*v),
                Unit::C(_) => None,
            })
            .collect();
        let mut nucleus: Vec<bool> = run.iter().map(|v| !v.weak()).collect();
        if !nucleus.iter().any(|n| *n) {
            // Only weak vowels: the second one carries the syllable ("cuida",
            // "ciudad"), unless it is a final "y" ("muy").
            let last = run.len() - 1;
            let pick = if run.len() > 1 && run[last].from_y {
                last - 1
            } else {
                last
            };
            nucleus[pick] = true;
        }
        for (k, is_nucleus) in nucleus.iter().enumerate() {
            let role = if *is_nucleus {
                Role::Nucleus
            } else if nucleus.get(k + 1).copied().unwrap_or(false) {
                Role::OnGlide
            } else {
                Role::OffGlide
            };
            roles[start + k] = Some(role);
        }
    }
    roles
}

fn stress_rule_word(word: &str) -> bool {
    !UNSTRESSED.contains(&word)
}

/// Appends the items for one word.
fn word_items(word: &str, accent: Accent, items: &mut Vec<Item>) {
    let word = EXCEPTIONS
        .iter()
        .find(|(w, _)| *w == word)
        .map_or(word, |(_, said)| said);
    // "-mente" adverbs keep the adjective's stress and add their own.
    if let Some(stem) = word.strip_suffix("mente")
        && stem.chars().count() >= 3
        && stem.ends_with(is_vowel_char)
    {
        syllables(stem, accent, true, true, items);
        // espeak-ng says "-mˈente", not "-mˈɛnte".
        syllables("mente", accent, true, false, items);
        return;
    }
    syllables(word, accent, stress_rule_word(word), true, items);
}

fn syllables(word: &str, accent: Accent, stressed: bool, open_e: bool, items: &mut Vec<Item>) {
    let units = units(word, accent);
    let roles = roles(&units);
    let nuclei: Vec<usize> = roles
        .iter()
        .enumerate()
        .filter(|(_, r)| **r == Some(Role::Nucleus))
        .map(|(i, _)| i)
        .collect();
    let primary = if stressed {
        primary_stress(word, &units, &nuclei)
    } else {
        None
    };
    let mut stress = vec![Stress::None; units.len()];
    if let Some(p) = primary {
        stress[nuclei[p]] = if WEAKLY_STRESSED.contains(&word) {
            Stress::Secondary
        } else {
            Stress::Primary
        };
        // Long words get a secondary stress on the first syllable, and on
        // every other one after it, as espeak-ng does.
        let mut k = 0;
        while p >= 2 && k + 1 < p {
            stress[nuclei[k]] = Stress::Secondary;
            k += 2;
        }
    }
    for (i, unit) in units.iter().enumerate() {
        match unit {
            Unit::C(item) => items.push(item.clone()),
            Unit::V(v) => {
                let role = roles[i].unwrap_or(Role::Nucleus);
                let sound = match role {
                    Role::Nucleus => {
                        // Stressed "e" closed by a nasal is open: "tengo",
                        // "gente", "entre".
                        let open = open_e
                            && v.base == 'e'
                            && stress[i] == Stress::Primary
                            && matches!(units.get(i + 1), Some(Unit::C(Item::N)))
                            && matches!(units.get(i + 2), Some(Unit::C(_)));
                        items.push(Item::Vowel(if open { 'ɛ' } else { v.base }, stress[i]));
                        continue;
                    }
                    Role::OnGlide => match v.base {
                        'i' => "j",
                        _ => "w",
                    },
                    Role::OffGlide => {
                        let after_u = matches!(items.last(), Some(Item::Vowel('u', _)));
                        match (v.base, after_u) {
                            ('i', true) => "j",
                            ('i', false) => "ɪ",
                            _ => "ʊ",
                        }
                    }
                };
                items.push(Item::Fixed(sound));
            }
        }
    }
}

/// Which nucleus carries the stress.
fn primary_stress(word: &str, units: &[Unit], nuclei: &[usize]) -> Option<usize> {
    if nuclei.is_empty() {
        return None;
    }
    if let Some(k) = nuclei
        .iter()
        .rposition(|&i| matches!(units[i], Unit::V(V { accented: true, .. })))
    {
        return Some(k);
    }
    if nuclei.len() == 1 {
        return Some(0);
    }
    // Ending in a vowel, "n" or "s": next to last syllable; otherwise the last.
    let last = word.chars().last().unwrap_or(' ');
    let paroxytone = matches!(last, 'a' | 'e' | 'i' | 'o' | 'u' | 'n' | 's');
    Some(if paroxytone {
        nuclei.len() - 2
    } else {
        nuclei.len() - 1
    })
}

fn is_nasal(sound: &str) -> bool {
    matches!(sound, "m" | "n" | "ɲ" | "ŋ")
}

/// Applies the context rules and writes the phoneme string.
fn render(items: &[Item]) -> String {
    let mut out = String::new();
    // The last sound written, for b/d/g; "" means after a pause.
    let mut prev: &str = "";
    for (i, item) in items.iter().enumerate() {
        let sound: String = match item {
            Item::Fixed(s) => s.to_string(),
            Item::B | Item::D | Item::G => {
                let stop = prev.is_empty() || is_nasal(prev);
                match (item, stop) {
                    (Item::B, true) => "b",
                    (Item::B, false) => "β",
                    (Item::D, true) => "d",
                    (Item::D, false) => "ð",
                    (_, true) => "ɡ",
                    (_, false) => "ɣ",
                }
                .to_owned()
            }
            Item::N => {
                let next = items[i + 1..]
                    .iter()
                    .find(|n| !matches!(n, Item::Space))
                    .cloned();
                match next {
                    Some(Item::B | Item::Fixed("p")) => "m",
                    Some(Item::G | Item::Fixed("x")) => "ŋ",
                    _ => "n",
                }
                .to_owned()
            }
            Item::Vowel(v, stress) => {
                match stress {
                    Stress::Primary => out.push('ˈ'),
                    Stress::Secondary => out.push('ˌ'),
                    Stress::None => {}
                }
                v.to_string()
            }
            Item::Space => {
                out.push(' ');
                continue;
            }
            Item::Pause(c) => {
                // Punctuation sits right after the word, as espeak writes it.
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push(*c);
                prev = "";
                continue;
            }
        };
        out.push_str(&sound);
        prev = match sound.as_str() {
            "m" => "m",
            "n" => "n",
            "ŋ" => "ŋ",
            "ɲ" => "ɲ",
            _ => "x",
        };
    }
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn es(text: &str) -> String {
        phonemize(text, Accent::Castilian)
    }

    fn ar(text: &str) -> String {
        phonemize(text, Accent::Rioplatense)
    }

    #[test]
    fn matches_espeak_on_common_words() {
        // Every expected string here is espeak-ng's own output for `es`.
        assert_eq!(es("Hola, ¿cómo andás?"), "ˈola, kˈomo andˈas?");
        assert_eq!(
            es("¿Querés que te lea las noticias?"),
            "keɾˈes ke te lˈea las notˈiθjas?"
        );
        assert_eq!(es("La cerveza y el zapato."), "la θeɾβˈeθa i el θapˈato.");
        assert_eq!(es("El perro corre."), "el pˈero kˈore.");
        assert_eq!(es("Bien."), "bjˈen.");
        assert_eq!(es("Auto, deuda, causa."), "ˈaʊto, dˈeʊða, kˈaʊsa.");
        assert_eq!(es("Un gato. Tengo hambre."), "ˈuŋ ɡˈato. tˈɛŋɡo ˈambɾe.");
        assert_eq!(es("Un vaso, con permiso."), "ˈum bˈaso, kom peɾmˈiso.");
        assert_eq!(es("Hay muy poco."), "ˈaɪ mˈuj pˈoko.");
        assert_eq!(es("Ayer, yema, hielo."), "aʝˈeɾ, ʝˈema, jˈelo.");
        assert_eq!(es("Programación."), "pɾˌoɣɾamaθjˈon.");
        assert_eq!(es("Teléfono, música."), "telˈefono, mˈusika.");
        assert_eq!(es("Rápidamente."), "rˈapiðamˈente.");
        assert_eq!(
            es("Israel, alrededor, honra."),
            "ˌisraˈel, ˌalreðeðˈoɾ, ˈonra."
        );
        assert_eq!(es("Guerra, guitarra, gente."), "ɡˈera, ɡitˈara, xˈɛnte.");
        assert_eq!(es("Excelente, examen."), "ˌeksθelˈɛnte, eksˈamen.");
        assert_eq!(es("Dale, che, ¿todo bien?"), "dˈale, tʃˈe, tˈoðo βjˈen?");
    }

    #[test]
    fn glides_follow_spanish_syllables() {
        // espeak-ng reads "üe" and "ue" after ʎ/r as two syllables; Spanish
        // (and these rules) make them one.
        assert_eq!(es("Averigüé."), "ˌaβeɾiɣwˈe.");
        assert_eq!(es("Ciudad, cuidado, fui."), "θjuðˈad, kwiðˈaðo, fwˈi.");
        assert_eq!(es("Estoy, rey."), "estˈoɪ, rˈeɪ.");
    }

    #[test]
    fn rioplatense_has_seseo_and_sheismo() {
        assert_eq!(ar("La cerveza."), "la seɾβˈesa.");
        assert_eq!(ar("Yo me voy a la calle."), "ʃˈo me βˈoɪ a la kˈaʃe.");
        assert_eq!(
            ar("¿Querés que te lea las noticias?"),
            "keɾˈes ke te lˈea las notˈisjas?"
        );
        assert_eq!(
            phonemize("Calle y playa.", Accent::Neutral),
            "kˈaʝe i plˈaʝa."
        );
    }

    #[test]
    fn numbers_and_loanwords_become_spanish_sounds() {
        assert_eq!(ar("Son las 3."), "sˈon las tɾˈes.");
        assert_eq!(ar("Abrí WhatsApp."), "aβɾˈi ɣwatsˈap.");
        assert_eq!(ar("El USB."), "el ˈu ˈese βˈe.");
    }

    #[test]
    fn output_uses_only_kokoro_symbols() {
        let text = "¡Qué lindo día! Son las 10:30 y tengo $1.500, 25% más que ayer; \
                    ¿vos querés pingüinos, whisky o un show en YouTube?";
        for accent in [Accent::Rioplatense, Accent::Neutral, Accent::Castilian] {
            let phonemes = phonemize(text, accent);
            for c in phonemes.chars() {
                assert!(crate::vocab::token(c).is_some(), "{c:?} in {phonemes}");
            }
        }
    }
}
