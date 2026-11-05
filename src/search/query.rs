//! The query syntax: bare words and "quoted phrases".

/// Words are matched stemmed; phrases must appear exactly, in order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedQuery {
    /// The words outside quotes, in order.
    pub words: Vec<String>,
    /// The texts between pairs of double quotes, trimmed; an empty pair
    /// adds none.
    pub phrases: Vec<String>,
}

/// Splits `q` on whitespace into words, except that text between two double
/// quotes is one phrase. An unmatched quote turns the rest of `q` into words.
pub fn parse_query(q: &str) -> ParsedQuery {
    let mut parsed = ParsedQuery::default();
    let mut rest = q;
    loop {
        rest = rest.trim_start();
        let Some(first) = rest.chars().next() else {
            break;
        };
        if first == '"' {
            let after = &rest[1..];
            let Some(close) = after.find('"') else {
                parsed
                    .words
                    .extend(after.split_whitespace().map(str::to_string));
                break;
            };
            let phrase = after[..close].trim();
            if !phrase.is_empty() {
                parsed.phrases.push(phrase.to_string());
            }
            rest = &after[close + 1..];
        } else {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '"')
                .unwrap_or(rest.len());
            parsed.words.push(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_words_and_phrases() {
        let q = parse_query(r#"channel "mutual information" capacity"#);
        assert_eq!(q.words, ["channel", "capacity"]);
        assert_eq!(q.phrases, ["mutual information"]);
        assert_eq!(parse_query(r#"open "phrase"#).words, ["open", "phrase"]);
    }

    #[test]
    fn a_quote_ends_the_word_before_it() {
        let q = parse_query(r#"a"b c"d"#);
        assert_eq!(q.words, ["a", "d"]);
        assert_eq!(q.phrases, ["b c"]);
    }

    #[test]
    fn empty_phrases_and_blank_input_parse_to_nothing() {
        assert_eq!(parse_query(""), ParsedQuery::default());
        assert_eq!(parse_query("   \t "), ParsedQuery::default());
        assert_eq!(parse_query(r#""" "  ""#), ParsedQuery::default());
    }

    #[test]
    fn phrase_whitespace_is_trimmed_at_the_ends_only() {
        let q = parse_query(r#"" a   b ""#);
        assert_eq!(q.phrases, ["a   b"]);
    }
}
