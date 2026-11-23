//! Numbers as Spanish words: "1810" -> "mil ochocientos diez".

const UNITS: [&str; 30] = [
    "cero",
    "uno",
    "dos",
    "tres",
    "cuatro",
    "cinco",
    "seis",
    "siete",
    "ocho",
    "nueve",
    "diez",
    "once",
    "doce",
    "trece",
    "catorce",
    "quince",
    "dieciséis",
    "diecisiete",
    "dieciocho",
    "diecinueve",
    "veinte",
    "veintiuno",
    "veintidós",
    "veintitrés",
    "veinticuatro",
    "veinticinco",
    "veintiséis",
    "veintisiete",
    "veintiocho",
    "veintinueve",
];

const TENS: [&str; 10] = [
    "",
    "",
    "",
    "treinta",
    "cuarenta",
    "cincuenta",
    "sesenta",
    "setenta",
    "ochenta",
    "noventa",
];

const HUNDREDS: [&str; 10] = [
    "",
    "ciento",
    "doscientos",
    "trescientos",
    "cuatrocientos",
    "quinientos",
    "seiscientos",
    "setecientos",
    "ochocientos",
    "novecientos",
];

/// Largest number read as a whole; longer digit strings are read digit by digit.
pub const MAX_CARDINAL: u64 = 999_999_999_999_999;

/// 0..1000.
fn below_thousand(n: u64) -> String {
    debug_assert!(n < 1000);
    if n == 100 {
        return "cien".into();
    }
    let (h, rest) = (n / 100, n % 100);
    let mut out = String::new();
    if h > 0 {
        out.push_str(HUNDREDS[h as usize]);
    }
    if rest > 0 || h == 0 {
        if !out.is_empty() {
            out.push(' ');
        }
        if rest < 30 {
            out.push_str(UNITS[rest as usize]);
        } else {
            out.push_str(TENS[(rest / 10) as usize]);
            if rest % 10 > 0 {
                out.push_str(" y ");
                out.push_str(UNITS[(rest % 10) as usize]);
            }
        }
    }
    out
}

/// "uno" loses its last vowel before a noun or "mil": "veintiún mil".
pub fn apocope(words: &str) -> String {
    if let Some(stem) = words.strip_suffix("veintiuno") {
        return format!("{stem}veintiún");
    }
    if let Some(stem) = words.strip_suffix("uno") {
        return format!("{stem}un");
    }
    words.to_owned()
}

/// The feminine form, for "una hora", "veintiuna personas".
pub fn feminine(words: &str) -> String {
    if let Some(stem) = words.strip_suffix("uno") {
        return format!("{stem}una");
    }
    words.to_owned()
}

/// A cardinal number in words, masculine ("uno"). `None` above
/// [`MAX_CARDINAL`].
pub fn cardinal(n: u64) -> Option<String> {
    if n > MAX_CARDINAL {
        return None;
    }
    if n < 1000 {
        return Some(below_thousand(n));
    }
    let mut parts: Vec<String> = Vec::new();
    let billions = n / 1_000_000_000_000;
    let millions = (n / 1_000_000) % 1_000_000;
    let thousands = (n / 1000) % 1000;
    let rest = n % 1000;
    if billions > 0 {
        parts.push(if billions == 1 {
            "un billón".into()
        } else {
            format!("{} billones", apocope(&thousands_part(billions)))
        });
    }
    if millions > 0 {
        parts.push(if millions == 1 {
            "un millón".into()
        } else {
            format!("{} millones", apocope(&thousands_part(millions)))
        });
    }
    if thousands > 0 {
        parts.push(if thousands == 1 {
            "mil".into()
        } else {
            format!("{} mil", apocope(&below_thousand(thousands)))
        });
    }
    if rest > 0 {
        parts.push(below_thousand(rest));
    }
    Some(parts.join(" "))
}

/// 1..1_000_000, for the count of millions or billions ("dos mil quinientos").
fn thousands_part(n: u64) -> String {
    let (high, low) = (n / 1000, n % 1000);
    match (high, low) {
        (0, _) => below_thousand(low),
        (1, 0) => "mil".into(),
        (1, _) => format!("mil {}", below_thousand(low)),
        (_, 0) => format!("{} mil", apocope(&below_thousand(high))),
        _ => format!(
            "{} mil {}",
            apocope(&below_thousand(high)),
            below_thousand(low)
        ),
    }
}

/// Each digit on its own: "0800" -> "cero ocho cero cero".
pub fn digits(text: &str) -> String {
    text.chars()
        .filter_map(|c| c.to_digit(10))
        .map(|d| UNITS[d as usize])
        .collect::<Vec<_>>()
        .join(" ")
}

const ORDINALS_M: [&str; 11] = [
    "", "primero", "segundo", "tercero", "cuarto", "quinto", "sexto", "séptimo", "octavo",
    "noveno", "décimo",
];

/// "1º" -> "primero", "3ª" -> "tercera". Past ten it falls back to the
/// cardinal, which is how most people say them anyway.
pub fn ordinal(n: u64, feminine_form: bool) -> Option<String> {
    if (1..=10).contains(&n) {
        let word = ORDINALS_M[n as usize];
        return Some(if feminine_form {
            format!("{}a", &word[..word.len() - 1])
        } else {
            word.to_owned()
        });
    }
    cardinal(n)
}

pub const MONTHS: [&str; 12] = [
    "enero",
    "febrero",
    "marzo",
    "abril",
    "mayo",
    "junio",
    "julio",
    "agosto",
    "septiembre",
    "octubre",
    "noviembre",
    "diciembre",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn c(n: u64) -> String {
        cardinal(n).unwrap()
    }

    #[test]
    fn small_numbers() {
        assert_eq!(c(0), "cero");
        assert_eq!(c(15), "quince");
        assert_eq!(c(21), "veintiuno");
        assert_eq!(c(42), "cuarenta y dos");
        assert_eq!(c(100), "cien");
        assert_eq!(c(101), "ciento uno");
        assert_eq!(c(555), "quinientos cincuenta y cinco");
        assert_eq!(c(999), "novecientos noventa y nueve");
    }

    #[test]
    fn big_numbers() {
        assert_eq!(c(1000), "mil");
        assert_eq!(c(1810), "mil ochocientos diez");
        assert_eq!(c(2026), "dos mil veintiséis");
        assert_eq!(c(21_000), "veintiún mil");
        assert_eq!(c(31_500), "treinta y un mil quinientos");
        assert_eq!(c(100_000), "cien mil");
        assert_eq!(c(1_000_000), "un millón");
        assert_eq!(c(2_500_000), "dos millones quinientos mil");
        assert_eq!(c(21_000_000), "veintiún millones");
        assert_eq!(c(1_000_000_000), "mil millones");
        assert_eq!(c(1_000_000_000_000), "un billón");
        assert_eq!(cardinal(MAX_CARDINAL + 1), None);
    }

    #[test]
    fn agreement_and_ordinals() {
        assert_eq!(apocope("veintiuno"), "veintiún");
        assert_eq!(apocope("treinta y uno"), "treinta y un");
        assert_eq!(feminine("uno"), "una");
        assert_eq!(ordinal(1, false).unwrap(), "primero");
        assert_eq!(ordinal(3, true).unwrap(), "tercera");
        assert_eq!(digits("0800"), "cero ocho cero cero");
    }
}
