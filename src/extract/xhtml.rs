//! Turns one EPUB content document into raw blocks.

use scraper::{ElementRef, Html, Node};

use crate::document::BlockKind;
use crate::normalize::normalize;

/// One block of an XHTML document, before the EPUB reader gives it an id,
/// a section path and a locator.
#[derive(Debug, Clone, PartialEq)]
pub struct RawBlock {
    pub kind: BlockKind,
    pub text: String,
    /// The last element id met at or before the start of the block.
    pub anchor: Option<String>,
    /// Every element id met since the previous emitted block, plus the block's
    /// own id and the ids of its descendants.
    pub ids: Vec<String>,
    /// The printed page in effect where the block's text starts.
    pub print_page: Option<String>,
}

/// Elements whose content is kept apart from their neighbours: the loose text
/// of one of them forms paragraphs of its own, and a nested one ends the run.
const CONTAINERS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "caption",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "header",
    "hgroup",
    "html",
    "li",
    "main",
    "ol",
    "pre",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "ul",
];

/// Elements that XHTML may write as `<x/>` and HTML always leaves empty.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Turns one XHTML content document into blocks in document order.
///
/// `print_page` is the printed page in effect where the document starts. Page
/// breaks inside the document update it, so the caller passes the same value
/// to the next document of the book.
pub fn xhtml_blocks(html: &str, print_page: &mut Option<String>) -> Vec<RawBlock> {
    let document = Html::parse_document(&expand_empty_elements(html));
    let mut walker = Walker {
        blocks: Vec::new(),
        print_page,
        pending_ids: Vec::new(),
        last_id: None,
        open: None,
        runs: vec![BlockKind::Paragraph],
        leaf_depth: 0,
        skip_depth: 0,
    };
    // The walk keeps its own cursor and never recurses, so a deeply nested
    // document cannot overflow the stack.
    let root = *document.root_element();
    let mut node = root;
    'walk: loop {
        match node.value() {
            Node::Text(text) => walker.text(text),
            Node::Element(_) => {
                if let Some(el) = ElementRef::wrap(node) {
                    walker.enter(el);
                }
            }
            _ => {}
        }
        if let Some(child) = node.first_child() {
            node = child;
            continue;
        }
        loop {
            if let Some(el) = ElementRef::wrap(node) {
                walker.leave(el);
            }
            if node == root {
                break 'walk;
            }
            if let Some(next) = node.next_sibling() {
                node = next;
                break;
            }
            match node.parent() {
                Some(parent) => node = parent,
                None => break 'walk,
            }
        }
    }
    walker.blocks
}

/// What the walk does with an element.
enum Class {
    /// The element and everything inside it is dropped.
    Skipped,
    /// A printed page boundary, read and then dropped.
    PageBreak,
    /// The element's whole text is one block.
    Leaf(BlockKind),
    /// The element's children are blocks, and its loose text forms runs.
    Container,
    /// The element's content belongs to the text around it.
    Inline,
}

fn classify(el: ElementRef) -> Class {
    let has = |attr: &str, values: &[&str]| {
        el.value()
            .attr(attr)
            .is_some_and(|v| v.split_whitespace().any(|t| values.contains(&t)))
    };
    let tag = el.value().name();
    if has("epub:type", &["noteref"]) || has("role", &["doc-noteref"]) {
        Class::Skipped
    } else if has("epub:type", &["pagebreak"]) || has("role", &["doc-pagebreak"]) {
        Class::PageBreak
    } else if matches!(tag, "head" | "nav" | "script" | "style") {
        Class::Skipped
    } else if has("epub:type", &["footnote", "endnote", "rearnote"])
        || has("role", &["doc-footnote", "doc-endnote"])
    {
        Class::Leaf(BlockKind::Note {
            label: el.value().id().map(str::to_owned),
        })
    } else if let Some(level) = heading_level(tag) {
        Class::Leaf(BlockKind::Heading { level })
    } else if tag == "p" || has("role", &["paragraph"]) {
        Class::Leaf(BlockKind::Paragraph)
    } else if CONTAINERS.contains(&tag) {
        Class::Container
    } else {
        Class::Inline
    }
}

fn heading_level(tag: &str) -> Option<u8> {
    match tag {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

/// The block being filled with text.
struct Open {
    kind: BlockKind,
    text: String,
    anchor: Option<String>,
    ids: Vec<String>,
    print_page: Option<String>,
}

struct Walker<'a> {
    blocks: Vec<RawBlock>,
    print_page: &'a mut Option<String>,
    /// Ids met since the last emitted block while no block was open.
    pending_ids: Vec<String>,
    /// The last id met, inside a block or outside.
    last_id: Option<String>,
    open: Option<Open>,
    /// For each container being walked, the kind that its loose text becomes.
    runs: Vec<BlockKind>,
    /// Elements open inside the current leaf block, the leaf itself included;
    /// zero between leaf blocks.
    leaf_depth: usize,
    /// Elements open inside a skipped element, it included; zero elsewhere.
    skip_depth: usize,
}

impl Walker<'_> {
    fn enter(&mut self, el: ElementRef) {
        if self.skip_depth > 0 {
            self.skip_depth += 1;
            return;
        }
        let class = classify(el);
        let in_leaf = self.leaf_depth > 0;
        if !in_leaf && matches!(class, Class::Leaf(_) | Class::Container) {
            self.flush();
        }
        self.meet_id(el);
        match class {
            Class::Skipped => self.skip_depth = 1,
            Class::PageBreak => {
                self.page_break(el);
                self.skip_depth = 1;
            }
            Class::Leaf(kind) if !in_leaf => {
                self.begin(kind);
                self.leaf_depth = 1;
            }
            Class::Container if !in_leaf => self.runs.push(if el.value().name() == "li" {
                BlockKind::ListItem
            } else {
                BlockKind::Paragraph
            }),
            // Inside a leaf block, elements that would be blocks elsewhere
            // only separate the words around them.
            Class::Leaf(_) | Class::Container => {
                self.push_text(" ");
                self.leaf_depth += 1;
            }
            Class::Inline => {
                if in_leaf {
                    self.leaf_depth += 1;
                }
                if el.value().name() == "br" {
                    self.push_text(" ");
                }
            }
        }
    }

    fn leave(&mut self, el: ElementRef) {
        if self.skip_depth > 0 {
            self.skip_depth -= 1;
        } else if self.leaf_depth > 0 {
            self.leaf_depth -= 1;
            if self.leaf_depth == 0 {
                self.flush();
            } else if matches!(classify(el), Class::Leaf(_) | Class::Container) {
                self.push_text(" ");
            }
        } else if matches!(classify(el), Class::Container) {
            self.flush();
            self.runs.pop();
        }
    }

    fn text(&mut self, text: &str) {
        if self.skip_depth > 0 {
            return;
        }
        // Text between blocks that is only whitespace does not start a run,
        // so it never takes ids from the block that follows.
        if self.leaf_depth == 0 && self.open.is_none() {
            if text.trim().is_empty() {
                return;
            }
            let kind = self.runs.last().cloned().unwrap_or(BlockKind::Paragraph);
            self.begin(kind);
        }
        self.push_text(text);
    }

    fn push_text(&mut self, text: &str) {
        let Some(open) = &mut self.open else { return };
        if open.text.trim_start().is_empty() && !text.trim().is_empty() {
            open.print_page = self.print_page.clone();
        }
        open.text.push_str(text);
    }

    fn begin(&mut self, kind: BlockKind) {
        self.open = Some(Open {
            kind,
            text: String::new(),
            anchor: self.last_id.clone(),
            ids: std::mem::take(&mut self.pending_ids),
            print_page: None,
        });
    }

    /// Emits the open block. A block without text is dropped, and its ids wait
    /// for the next block.
    fn flush(&mut self) {
        let Some(open) = self.open.take() else { return };
        let text = normalize(&open.text);
        if text.is_empty() {
            self.pending_ids.extend(open.ids);
            return;
        }
        self.blocks.push(RawBlock {
            kind: open.kind,
            text,
            anchor: open.anchor,
            ids: open.ids,
            print_page: open.print_page,
        });
    }

    fn meet_id(&mut self, el: ElementRef) {
        let Some(id) = el.value().id().filter(|id| !id.is_empty()) else {
            return;
        };
        self.last_id = Some(id.to_owned());
        match &mut self.open {
            Some(open) => open.ids.push(id.to_owned()),
            None => self.pending_ids.push(id.to_owned()),
        }
    }

    /// Reads the page label from, in order, `title`, `aria-label`, the text of
    /// the marker, and the last run of digits in its `id`. A marker with none
    /// of them leaves the current page alone.
    fn page_break(&mut self, el: ElementRef) {
        let from_attr = |name: &str| el.value().attr(name).map(normalize);
        let label = from_attr("title")
            .filter(|v| !v.is_empty())
            .or_else(|| from_attr("aria-label").filter(|v| !v.is_empty()))
            .or_else(|| Some(normalize(&el.text().collect::<String>())).filter(|v| !v.is_empty()))
            .or_else(|| el.value().id().and_then(last_digits));
        if label.is_some() {
            *self.print_page = label;
        }
    }
}

fn last_digits(id: &str) -> Option<String> {
    id.split(|c: char| !c.is_ascii_digit())
        .rfind(|run| !run.is_empty())
        .map(str::to_owned)
}

/// Rewrites every self-closed element that HTML leaves open, `<x/>`, as
/// `<x></x>`. The HTML parser ignores the slash, so `<span epub:type="pagebreak"
/// title="45"/>` would otherwise contain everything after it.
fn expand_empty_elements(xhtml: &str) -> String {
    let mut out = String::with_capacity(xhtml.len());
    let mut rest = xhtml;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let after = &rest[1..];
        if rest.starts_with("<!--") {
            let end = rest.find("-->").map_or(rest.len(), |i| i + 3);
            out.push_str(&rest[..end]);
            rest = &rest[end..];
        } else if after.starts_with(['!', '?', '/']) {
            let end = rest.find('>').map_or(rest.len(), |i| i + 1);
            out.push_str(&rest[..end]);
            rest = &rest[end..];
        } else if after.starts_with(|c: char| c.is_ascii_alphabetic()) {
            let end = tag_end(rest);
            let tag = &rest[..end];
            let name = &tag[1..tag
                .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
                .unwrap_or(tag.len())];
            rest = &rest[end..];
            if let Some(open) = tag.strip_suffix("/>") {
                out.push_str(open);
                out.push('>');
                if !VOID_ELEMENTS.contains(&name.to_ascii_lowercase().as_str()) {
                    out.push_str("</");
                    out.push_str(name);
                    out.push('>');
                }
            } else {
                out.push_str(tag);
                if name.eq_ignore_ascii_case("script") || name.eq_ignore_ascii_case("style") {
                    // The content is raw text, whatever it contains.
                    let closing = format!("</{name}");
                    let end = find_ignore_case(rest, &closing).unwrap_or(rest.len());
                    out.push_str(&rest[..end]);
                    rest = &rest[end..];
                }
            }
        } else {
            out.push('<');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// The length of the start tag at the beginning of `s`, quotes respected.
fn tag_end(s: &str) -> usize {
    let mut quote = None;
    for (i, b) in s.bytes().enumerate() {
        match (quote, b) {
            (None, b'>') => return i + 1,
            (None, b'"' | b'\'') => quote = Some(b),
            (Some(q), _) if q == b => quote = None,
            _ => {}
        }
    }
    s.len()
}

fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>Chapter</title></head>
<body>{body}</body>
</html>"#
        )
    }

    fn blocks(body: &str) -> Vec<RawBlock> {
        xhtml_blocks(&document(body), &mut None)
    }

    fn pairs(blocks: &[RawBlock]) -> Vec<(BlockKind, &str)> {
        blocks
            .iter()
            .map(|b| (b.kind.clone(), b.text.as_str()))
            .collect()
    }

    fn heading(level: u8) -> BlockKind {
        BlockKind::Heading { level }
    }

    fn note(label: &str) -> BlockKind {
        BlockKind::Note {
            label: Some(label.to_string()),
        }
    }

    #[test]
    fn headings_and_paragraphs() {
        let got = blocks("<h2>Entropy</h2><p>Text one.</p>");
        assert_eq!(
            pairs(&got),
            [(heading(2), "Entropy"), (BlockKind::Paragraph, "Text one.")]
        );
    }

    #[test]
    fn role_paragraph_and_div_runs() {
        let got = blocks(r#"<div role="paragraph">A</div><div>Loose text <p>B</p> tail</div>"#);
        assert_eq!(
            pairs(&got),
            [
                (BlockKind::Paragraph, "A"),
                (BlockKind::Paragraph, "Loose text"),
                (BlockKind::Paragraph, "B"),
                (BlockKind::Paragraph, "tail"),
            ]
        );
    }

    #[test]
    fn list_items() {
        let got = blocks("<ul><li>One</li><li>Two</li></ul>");
        assert_eq!(
            pairs(&got),
            [(BlockKind::ListItem, "One"), (BlockKind::ListItem, "Two")]
        );
    }

    #[test]
    fn blockquote_paragraphs() {
        let got = blocks("<blockquote><p>Q</p></blockquote>");
        assert_eq!(pairs(&got), [(BlockKind::Paragraph, "Q")]);
    }

    #[test]
    fn noterefs_removed_notes_kept() {
        let got = blocks(
            r##"<p>Claim<a epub:type="noteref" href="#n1">1</a> continues.</p><aside epub:type="footnote" id="n1"><p>1 Source.</p></aside>"##,
        );
        assert_eq!(
            pairs(&got),
            [
                (BlockKind::Paragraph, "Claim continues."),
                (note("n1"), "1 Source.")
            ]
        );
    }

    #[test]
    fn note_and_noteref_markup_variants() {
        let noteref_forms = [r#"role="doc-noteref""#, r#"epub:type="noteref""#];
        let note_forms = [
            r#"role="doc-footnote""#,
            r#"role="doc-endnote""#,
            r#"epub:type="footnote""#,
            r#"epub:type="endnote""#,
            r#"epub:type="rearnote""#,
        ];
        for noteref in noteref_forms {
            for note_form in note_forms {
                let got = blocks(&format!(
                    r##"<p>Claim<a {noteref} href="#n1">1</a> continues.</p><aside {note_form} id="n1"><p>1 Source.</p></aside>"##
                ));
                assert_eq!(
                    pairs(&got),
                    [
                        (BlockKind::Paragraph, "Claim continues."),
                        (note("n1"), "1 Source.")
                    ],
                    "noteref {noteref}, note {note_form}"
                );
            }
        }
    }

    #[test]
    fn note_without_id_has_no_label_and_joins_its_paragraphs() {
        let got = blocks(r#"<div epub:type="footnote"><p>One.</p><p>Two.</p></div>"#);
        assert_eq!(
            pairs(&got),
            [(BlockKind::Note { label: None }, "One. Two.")]
        );
    }

    #[test]
    fn pagebreak_sets_print_page() {
        let mut page = None;
        let got = xhtml_blocks(
            &document(r#"<p>A</p><span epub:type="pagebreak" title="45"/><p>B</p>"#),
            &mut page,
        );
        assert_eq!(
            pairs(&got),
            [(BlockKind::Paragraph, "A"), (BlockKind::Paragraph, "B")]
        );
        assert_eq!(got[0].print_page, None);
        assert_eq!(got[1].print_page, Some("45".to_string()));
        assert_eq!(page, Some("45".to_string()));

        let got = xhtml_blocks(
            &document(r#"<p>C</p><span role="doc-pagebreak" aria-label="46"/><p>D</p>"#),
            &mut page,
        );
        assert_eq!(got[0].print_page, Some("45".to_string()));
        assert_eq!(got[1].print_page, Some("46".to_string()));
        assert_eq!(page, Some("46".to_string()));
    }

    #[test]
    fn pagebreak_value_precedence() {
        let value = |marker: &str| {
            let mut page = Some("old".to_string());
            xhtml_blocks(&document(&format!("{marker}<p>X</p>")), &mut page);
            page
        };
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            value(r#"<span epub:type="pagebreak" title="T" aria-label="L" id="page9">Q</span>"#),
            some("T")
        );
        assert_eq!(
            value(r#"<span epub:type="pagebreak" aria-label="L" id="page9">Q</span>"#),
            some("L")
        );
        assert_eq!(
            value(r#"<span epub:type="pagebreak" id="page9">Q</span>"#),
            some("Q")
        );
        assert_eq!(
            value(r#"<span epub:type="pagebreak" id="ch2_page_47"/>"#),
            some("47")
        );
        assert_eq!(
            value(r#"<span epub:type="pagebreak" id="p-xii"/>"#),
            some("old")
        );
    }

    #[test]
    fn pagebreak_text_stays_out_of_the_paragraph() {
        let got = blocks(r#"<p>Before <span epub:type="pagebreak">12</span>after.</p>"#);
        assert_eq!(pairs(&got), [(BlockKind::Paragraph, "Before after.")]);
        assert_eq!(got[0].print_page, None);
    }

    #[test]
    fn print_page_is_the_one_in_effect_where_the_text_starts() {
        let got = blocks(r#"<p id="a"><span epub:type="pagebreak" title="45"/>Text</p>"#);
        assert_eq!(got[0].print_page, Some("45".to_string()));
    }

    #[test]
    fn anchors_and_ids() {
        let got = blocks(r#"<section id="s1"><h1 id="h">T</h1><p>P</p></section>"#);
        assert_eq!(
            pairs(&got),
            [(heading(1), "T"), (BlockKind::Paragraph, "P")]
        );
        assert_eq!(got[0].anchor, Some("h".to_string()));
        assert_eq!(got[0].ids, ["s1", "h"]);
        assert_eq!(got[1].anchor, Some("h".to_string()));
        assert!(got[1].ids.is_empty());
    }

    #[test]
    fn descendant_ids_belong_to_the_block_and_become_the_next_anchor() {
        let got = blocks(r#"<p>Some <span id="mid">text</span></p><p>More</p>"#);
        assert_eq!(got[0].anchor, None);
        assert_eq!(got[0].ids, ["mid"]);
        assert_eq!(got[1].anchor, Some("mid".to_string()));
        assert!(got[1].ids.is_empty());
    }

    #[test]
    fn ids_of_dropped_blocks_pass_to_the_next_block() {
        let got = blocks(r#"<p id="x"> </p><a id="y"/><h2 id="z">T</h2>"#);
        assert_eq!(pairs(&got), [(heading(2), "T")]);
        assert_eq!(got[0].ids, ["x", "y", "z"]);
        assert_eq!(got[0].anchor, Some("z".to_string()));
    }

    #[test]
    fn skips_nav_script_style() {
        let got = blocks(
            r#"<nav epub:type="toc"><p>x</p></nav><script>y</script><style>p { color: red }</style><p>Z</p>"#,
        );
        assert_eq!(pairs(&got), [(BlockKind::Paragraph, "Z")]);
    }

    #[test]
    fn br_becomes_space_and_empty_blocks_drop() {
        let got = blocks("<p>a<br/>b</p><p> </p>");
        assert_eq!(pairs(&got), [(BlockKind::Paragraph, "a b")]);
    }

    #[test]
    fn table_cells_and_figure_captions_are_paragraphs() {
        let got = blocks(
            r#"<table><tr><td>A</td><td>B</td></tr></table><figure><img src="x.png" alt="pic"/><figcaption>Cap</figcaption></figure>"#,
        );
        assert_eq!(
            pairs(&got),
            [
                (BlockKind::Paragraph, "A"),
                (BlockKind::Paragraph, "B"),
                (BlockKind::Paragraph, "Cap"),
            ]
        );
    }

    #[test]
    fn self_closing_elements_do_not_swallow_what_follows() {
        let got = blocks(
            r#"<p title="a/>b">X</p><span id="s"/><div id="d"/><script src="a.js"/><p>Y</p><script>if (a<b && c) { x = "it's"; }</script><p>Z</p>"#,
        );
        assert_eq!(
            pairs(&got),
            [
                (BlockKind::Paragraph, "X"),
                (BlockKind::Paragraph, "Y"),
                (BlockKind::Paragraph, "Z")
            ]
        );
        assert_eq!(got[1].ids, ["s", "d"]);
    }

    #[test]
    fn deeply_nested_markup_does_not_overflow_the_stack() {
        let depth = 4_000;
        let body = format!("{}deep{}", "<div>".repeat(depth), "</div>".repeat(depth));
        assert_eq!(pairs(&blocks(&body)), [(BlockKind::Paragraph, "deep")]);
    }
}
