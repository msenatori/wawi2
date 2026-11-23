//! Small text helpers: normalization for comparisons and quotes, ids,
//! truncation, and numbers written the Argentine way.

pub use wawi_core::normalize;

pub fn truncate(text: &str, max_chars: usize) -> String {
    wawi_plugins::manifest::truncate(text, max_chars)
}

/// Whether `quote` appears in `text`, both normalized (case, accents and
/// punctuation do not matter). An empty quote never appears.
pub fn quoted_in(quote: &str, text: &str) -> bool {
    let quote = normalize(quote);
    if quote.is_empty() {
        return false;
    }
    let text = format!(" {} ", normalize(text));
    text.contains(&format!(" {quote} "))
}

/// `entrevista-mercado-libre`: lowercase ascii words joined by `-`, at most
/// `max` characters, never empty.
pub fn slug(text: &str, max: usize) -> String {
    let mut out = String::new();
    for word in normalize(text).split(' ') {
        let word: String = word.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if word.is_empty() {
            continue;
        }
        if !out.is_empty() {
            if out.len() + 1 + word.len() > max {
                break;
            }
            out.push('-');
        }
        out.push_str(&word);
        if out.len() >= max {
            out.truncate(max);
            break;
        }
    }
    if out.is_empty() { "x".into() } else { out }
}

/// A number from text: `1234`, `1234.5`, `-3`, `1.234,56`, `$ 1.234`,
/// `1,234.56`. With both separators the last one is the decimal point; a
/// lone `,` is decimal unless it groups exactly three digits.
pub fn parse_number(text: &str) -> Option<f64> {
    let kept: String = text
        .trim()
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-'))
        .collect();
    if !kept.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let (last_dot, last_comma) = (kept.rfind('.'), kept.rfind(','));
    let plain = match (last_dot, last_comma) {
        (Some(d), Some(c)) if c > d => kept.replace('.', "").replace(',', "."),
        (Some(_), Some(_)) => kept.replace(',', ""),
        (None, Some(c)) => {
            let after = kept.len() - c - 1;
            if after == 3 && kept.matches(',').count() >= 1 && !kept[..c].is_empty() {
                kept.replace(',', "")
            } else {
                kept.replace(',', ".")
            }
        }
        (Some(d), None) => {
            // `1.234` and `1.234.567` group thousands; `1.5` is decimal.
            let groups = kept.matches('.').count();
            let after = kept.len() - d - 1;
            if groups > 1 || (after == 3 && kept.len() > 4) {
                kept.replace('.', "")
            } else {
                kept
            }
        }
        (None, None) => kept,
    };
    plain.parse().ok()
}

/// Token-set similarity of two texts (Jaccard over normalized words of
/// three letters or more).
pub fn similarity(a: &str, b: &str) -> f64 {
    let words = |t: &str| -> std::collections::BTreeSet<String> {
        normalize(t)
            .split(' ')
            .filter(|w| w.chars().count() >= 3)
            .map(str::to_owned)
            .collect()
    };
    let (a, b) = (words(a), words(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.intersection(&b).count() as f64;
    shared / a.union(&b).count() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_the_argentine_way() {
        for (text, n) in [
            ("1234", 1234.0),
            ("1234.5", 1234.5),
            ("1.234,56", 1234.56),
            ("$ 1.234", 1234.0),
            ("1,234.56", 1234.56),
            ("12,5", 12.5),
            ("-3", -3.0),
            ("1.234.567", 1_234_567.0),
            ("1.5", 1.5),
        ] {
            assert_eq!(parse_number(text), Some(n), "{text}");
        }
        assert_eq!(parse_number("nada"), None);
    }

    #[test]
    fn quotes_and_slugs() {
        assert!(quoted_in(
            "el lunes tengo entrevista",
            "Che, ¡el LUNES tengo entrevista! a las 10"
        ));
        assert!(!quoted_in("lunes tengo", "ellunes tengo"));
        assert!(!quoted_in("", "algo"));
        assert_eq!(slug("¿Entrevista con Mercado Libre?", 20), "entrevista-con");
        assert_eq!(slug("!!!", 10), "x");
        assert!(
            similarity(
                "entrevista en Mercado Libre",
                "la entrevista de Mercado Libre"
            ) > 0.5
        );
    }
}
