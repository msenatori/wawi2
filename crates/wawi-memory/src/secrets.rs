//! Cheap heuristics for text that must never be written to memory. They err on
//! the side of refusing: a lost fact is better than a stored password.

const WORDS: &[&str] = &[
    "password",
    "passwd",
    "contraseña",
    "contrasena",
    "api key",
    "api_key",
    "api-key",
    "apikey",
    "secret",
    "token",
    "private key",
    "clave privada",
    "bearer ",
    "-----begin",
];

/// Known key prefixes, each followed by at least 8 more characters.
const PREFIXES: &[&str] = &[
    "sk-",
    "sk_",
    "pk_live_",
    "rk_live_",
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "akia",
    "aiza",
];

pub fn looks_like_secret(text: &str) -> bool {
    let lower = text.to_lowercase();
    if WORDS.iter().any(|word| lower.contains(word)) {
        return true;
    }
    lower
        .split(|c: char| c.is_whitespace() || "\"'`,;()[]{}<>=:".contains(c))
        .any(|word| {
            PREFIXES
                .iter()
                .any(|prefix| word.starts_with(prefix) && word.len() >= prefix.len() + 8)
                || looks_random(word)
        })
}

/// A long run of key-like characters mixing letters and digits.
fn looks_random(word: &str) -> bool {
    word.len() >= 24
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_+/=".contains(c))
        && word.chars().any(|c| c.is_ascii_digit())
        && word.chars().any(|c| c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_secret_looking_text() {
        for text in [
            "Mi password es hunter2",
            "la contraseña del wifi es pepito",
            "API key: abc",
            "usar sk-proj-abcdefgh1234 para OpenAI",
            "token=xyz",
            "ghp_0123456789abcdefABCDEF",
            "AKIAIOSFODNN7EXAMPLE",
            "clave a8f3k2j9d8s7f6g5h4j3k2l1m0n9",
            "-----BEGIN OPENSSH PRIVATE KEY-----",
        ] {
            assert!(looks_like_secret(text), "{text}");
        }
    }

    #[test]
    fn lets_ordinary_facts_through() {
        for text in [
            "Toma mate amargo a la tarde.",
            "Su perra se llama Skippy.",
            "Trabaja en el proyecto Wawi desde 2025.",
            "Prefiere que le hable de vos.",
            "El cumpleaños de su mamá es el 12 de marzo.",
            "Es clave no despertarlo antes de las 8.",
        ] {
            assert!(!looks_like_secret(text), "{text}");
        }
    }
}
