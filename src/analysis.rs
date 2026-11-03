//! The two analyzers behind the lexical index, built in one place so the index
//! and the highlighter can never disagree about what a token is.

use tantivy::Index;
use tantivy::tokenizer::{
    Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer, TextAnalyzer,
};

/// Tokens of this many bytes or more are dropped, as URLs and encoded blobs
/// are noise in both fields.
const TOKEN_MAX_BYTES: usize = 40;

/// Lowercased, English-stemmed tokens. Indexed in `text` and `section`.
pub const STEMMED: &str = "ebook_hero_en_stem";
/// Lowercased tokens, unstemmed. Indexed in `text_exact`, used for phrases.
pub const EXACT: &str = "ebook_hero_en_exact";

/// A token as the analyzer emits it: `text[start..end]` is the source word,
/// and `position` is the slot tantivy indexes it at. Tokens the analyzer drops
/// leave a gap in `position`, so adjacent tokens can still be apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tok {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub position: usize,
}

/// Registers both analyzers on `index` under `STEMMED` and `EXACT`.
pub fn register(index: &Index) {
    let manager = index.tokenizers();
    manager.register(STEMMED, build_analyzer(STEMMED));
    manager.register(EXACT, build_analyzer(EXACT));
}

/// The tokens `analyzer` (`STEMMED` or `EXACT`) produces for `text`, with byte
/// offsets into `text`. Panics on any other name.
pub fn tokens(analyzer: &str, text: &str) -> Vec<Tok> {
    let mut analyzer = build_analyzer(analyzer);
    let mut stream = analyzer.token_stream(text);
    let mut tokens = Vec::new();
    stream.process(&mut |token| {
        tokens.push(Tok {
            text: token.text.clone(),
            start: token.offset_from,
            end: token.offset_to,
            position: token.position,
        });
    });
    tokens
}

/// The only place the pipelines are defined: `register` hands them to the
/// index and `tokens` runs them directly, so the two cannot drift apart.
fn build_analyzer(name: &str) -> TextAnalyzer {
    let exact = TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(TOKEN_MAX_BYTES))
        .filter(LowerCaser)
        .dynamic();
    match name {
        EXACT => exact.build(),
        STEMMED => exact
            .filter_dynamic(Stemmer::new(Language::English))
            .build(),
        other => panic!("unknown analyzer {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stemmed_and_exact_analyzers() {
        assert_eq!(
            tokens(STEMMED, "entropies")[0].text,
            tokens(STEMMED, "entropy")[0].text
        );
        assert_eq!(tokens(EXACT, "Entropies")[0].text, "entropies");
        let t = tokens(EXACT, "a  Bc");
        assert_eq!((t[1].start, t[1].end), (3, 5));
    }

    #[test]
    fn exact_keeps_inflections_apart() {
        assert_ne!(
            tokens(EXACT, "entropies")[0].text,
            tokens(EXACT, "entropy")[0].text
        );
    }

    #[test]
    fn offsets_are_bytes_into_the_source() {
        let text = "é Ünï x";
        let t = tokens(EXACT, text);
        let words: Vec<&str> = t.iter().map(|tok| &text[tok.start..tok.end]).collect();
        assert_eq!(words, ["é", "Ünï", "x"]);
        assert_eq!(
            t.iter().map(|tok| tok.text.as_str()).collect::<Vec<_>>(),
            ["é", "ünï", "x"]
        );
    }

    #[test]
    fn a_dropped_long_token_leaves_a_gap_in_positions() {
        let long = "x".repeat(45);
        let t = tokens(EXACT, &format!("a {long} b"));
        assert_eq!(
            t.iter().map(|tok| tok.text.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!((t[0].position, t[1].position), (0, 2));
    }
}
