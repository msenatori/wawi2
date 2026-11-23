//! Word error rate, after a light normalization: lowercase, punctuation
//! removed, accents kept ("sí" and "si" are different words in Spanish).

/// Lowercase words, with every character that is not a letter or a digit
/// treated as a space. Numbers are not spelled out: "3" and "tres" differ.
pub fn normalize(text: &str) -> Vec<String> {
    let cleaned: String = text
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    cleaned.split_whitespace().map(str::to_owned).collect()
}

/// Edit distance in words between a reference and a hypothesis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WordErrors {
    /// Substitutions + deletions + insertions.
    pub errors: usize,
    pub reference_words: usize,
}

impl WordErrors {
    pub fn rate(&self) -> f64 {
        if self.reference_words == 0 {
            return if self.errors == 0 { 0.0 } else { 1.0 };
        }
        self.errors as f64 / self.reference_words as f64
    }

    pub fn add(&mut self, other: WordErrors) {
        self.errors += other.errors;
        self.reference_words += other.reference_words;
    }
}

pub fn word_errors(reference: &str, hypothesis: &str) -> WordErrors {
    let reference = normalize(reference);
    let hypothesis = normalize(hypothesis);
    let mut previous: Vec<usize> = (0..=hypothesis.len()).collect();
    let mut current = vec![0; hypothesis.len() + 1];
    for (i, word) in reference.iter().enumerate() {
        current[0] = i + 1;
        for (j, other) in hypothesis.iter().enumerate() {
            let substitution = previous[j] + usize::from(word != other);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    WordErrors {
        errors: previous[hypothesis.len()],
        reference_words: reference.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_drops_case_and_punctuation_but_keeps_accents() {
        assert_eq!(
            normalize("¿Che, qué hora es?  ¡Sí!"),
            vec!["che", "qué", "hora", "es", "sí"]
        );
        assert_eq!(normalize("Ñandú"), vec!["ñandú"]);
    }

    #[test]
    fn identical_text_has_no_errors() {
        let errors = word_errors("Poné música.", "poné música");
        assert_eq!(errors.errors, 0);
        assert_eq!(errors.rate(), 0.0);
    }

    #[test]
    fn counts_substitutions_deletions_and_insertions() {
        assert_eq!(word_errors("a b c", "a x c").errors, 1);
        assert_eq!(word_errors("a b c", "a c").errors, 1);
        assert_eq!(word_errors("a b c", "a b c d").errors, 1);
        assert_eq!(word_errors("a b c", "").errors, 3);
        assert_eq!(word_errors("sí", "si").errors, 1);
    }

    #[test]
    fn rate_is_over_reference_words() {
        let errors = word_errors("uno dos tres cuatro", "uno dos");
        assert_eq!(errors.reference_words, 4);
        assert!((errors.rate() - 0.5).abs() < 1e-9);
    }
}
