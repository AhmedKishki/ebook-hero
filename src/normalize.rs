//! Text normalization shared by every reader and by quote verification.

use unicode_normalization::UnicodeNormalization;

/// Normalizes text in this order: NFC; expand the Latin ligatures U+FB00 to
/// U+FB06 to their letters; drop soft hyphens, zero-width spaces, byte-order
/// marks and control characters other than tab, newline and carriage return;
/// collapse every whitespace run to one space and trim.
pub fn normalize(text: &str) -> String {
    let mut expanded = String::with_capacity(text.len());
    for c in text.nfc() {
        match ligature_letters(c) {
            Some(letters) => expanded.push_str(letters),
            None => expanded.push(c),
        }
    }
    let kept: String = expanded.chars().filter(|&c| !is_dropped(c)).collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn ligature_letters(c: char) -> Option<&'static str> {
    match c {
        '\u{FB00}' => Some("ff"),
        '\u{FB01}' => Some("fi"),
        '\u{FB02}' => Some("fl"),
        '\u{FB03}' => Some("ffi"),
        '\u{FB04}' => Some("ffl"),
        '\u{FB05}' | '\u{FB06}' => Some("st"),
        _ => None,
    }
}

fn is_dropped(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{200B}' | '\u{FEFF}')
        || (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn expands_ligatures() {
        assert_eq!(normalize("ﬁnal ﬂow ﬀ ﬃ ﬄ ﬆ"), "final flow ff ffi ffl st");
    }

    #[test]
    fn composes_to_nfc() {
        assert_eq!(normalize("e\u{0301}"), "é");
    }

    #[test]
    fn removes_soft_hyphens_and_invisible_characters() {
        assert_eq!(normalize("co\u{00AD}op\u{200B}er\u{FEFF}ate"), "cooperate");
    }

    #[test]
    fn removes_control_characters() {
        assert_eq!(normalize("a\u{0007}b"), "ab");
    }

    #[test]
    fn collapses_whitespace() {
        assert_eq!(normalize("  one \n\t two  "), "one two");
    }

    #[test]
    fn preserves_accents_scripts_and_superscripts() {
        assert_eq!(normalize("naïve Ελληνικά x²"), "naïve Ελληνικά x²");
    }
}
