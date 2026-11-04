//! Turns one EPUB content document into raw blocks.

use scraper::{ElementRef, Html, Node};

use crate::document::{Apparatus, BlockKind};
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
    /// The apparatus that the `epub:type` or `role` of the block or of an
    /// element around it names.
    pub apparatus: Option<Apparatus>,
    /// The links in the block, in document order, and those met since the
    /// previous emitted block.
    pub links: Vec<RawLink>,
}

/// An `<a href>` element.
#[derive(Debug, Clone, PartialEq)]
pub struct RawLink {
    pub href: String,
    /// The element's own id.
    pub id: Option<String>,
    /// Whether its text has at most `MAX_MARKER_CHARS` characters that are not
    /// whitespace, as the marker of a note has.
    pub short: bool,
}

/// The most characters that are not whitespace a note marker such as `12`,
/// `[12]` or `*` has.
pub const MAX_MARKER_CHARS: usize = 8;

/// The longest element id, in bytes, that is kept. Every block copies the last
/// id it follows into its anchor, so a hostile file could otherwise multiply
/// one huge id by its number of blocks.
const MAX_ID_BYTES: usize = 256;

/// The longest printed-page label, in characters, that is kept. Every block
/// copies the page in effect, so the same reasoning applies.
const MAX_PAGE_LABEL_CHARS: usize = 32;

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
        pending_links: Vec::new(),
        last_id: None,
        open: None,
        runs: vec![Run {
            kind: BlockKind::Paragraph,
            apparatus: None,
        }],
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
            label: element_id(el).map(str::to_owned),
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

/// The apparatus that the element's `epub:type` or `role` names, in the EPUB
/// structural semantics and DPUB-ARIA vocabularies.
fn apparatus_of(el: ElementRef) -> Option<Apparatus> {
    let attr = |name| el.value().attr(name).unwrap_or_default();
    attr("epub:type")
        .split_whitespace()
        .chain(attr("role").split_whitespace())
        .find_map(|token| match token {
            "toc" | "toc-brief" | "doc-toc" => Some(Apparatus::Contents),
            "loi" | "lot" | "loa" | "lov" => Some(Apparatus::Lists),
            "endnotes" | "footnotes" | "rearnotes" | "doc-endnotes" => Some(Apparatus::Notes),
            "bibliography" | "doc-bibliography" => Some(Apparatus::Bibliography),
            "index" | "doc-index" => Some(Apparatus::Index),
            _ => None,
        })
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
    /// Whether `text` holds anything but whitespace.
    started: bool,
    anchor: Option<String>,
    ids: Vec<String>,
    print_page: Option<String>,
    apparatus: Option<Apparatus>,
    links: Vec<RawLink>,
}

/// What the loose text of a container becomes.
struct Run {
    kind: BlockKind,
    /// The apparatus the container or an element around it names.
    apparatus: Option<Apparatus>,
}

struct Walker<'a> {
    blocks: Vec<RawBlock>,
    print_page: &'a mut Option<String>,
    /// Ids met since the last emitted block while no block was open. Empty
    /// while a block is open, which takes them.
    pending_ids: Vec<String>,
    /// Links met since the last emitted block while no block was open, kept
    /// like `pending_ids`.
    pending_links: Vec<RawLink>,
    /// The last id met, inside a block or outside.
    last_id: Option<String>,
    open: Option<Open>,
    /// For each container being walked, what its loose text becomes.
    runs: Vec<Run>,
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
        self.meet_link(el);
        match class {
            Class::Skipped => self.skip_depth = 1,
            Class::PageBreak => {
                self.page_break(el);
                self.skip_depth = 1;
            }
            Class::Leaf(kind) if !in_leaf => {
                self.begin(kind, apparatus_of(el).or(self.apparatus()));
                self.leaf_depth = 1;
            }
            Class::Container if !in_leaf => self.runs.push(Run {
                kind: if el.value().name() == "li" {
                    BlockKind::ListItem
                } else {
                    BlockKind::Paragraph
                },
                apparatus: apparatus_of(el).or(self.apparatus()),
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
            let kind = self
                .runs
                .last()
                .map_or(BlockKind::Paragraph, |run| run.kind.clone());
            self.begin(kind, self.apparatus());
        }
        self.push_text(text);
    }

    /// The apparatus of the innermost container being walked.
    fn apparatus(&self) -> Option<Apparatus> {
        self.runs.last().and_then(|run| run.apparatus)
    }

    fn push_text(&mut self, text: &str) {
        let Some(open) = &mut self.open else { return };
        if !open.started && !text.trim().is_empty() {
            open.started = true;
            open.print_page = self.print_page.clone();
        }
        open.text.push_str(text);
    }

    fn begin(&mut self, kind: BlockKind, apparatus: Option<Apparatus>) {
        self.open = Some(Open {
            kind,
            text: String::new(),
            started: false,
            anchor: self.last_id.clone(),
            ids: std::mem::take(&mut self.pending_ids),
            print_page: None,
            apparatus,
            links: std::mem::take(&mut self.pending_links),
        });
    }

    /// Emits the open block. A block without text is dropped, and its ids wait
    /// for the next block: they are all the ids met since the last emitted
    /// block, since the open block took the pending ones.
    fn flush(&mut self) {
        let Some(open) = self.open.take() else { return };
        let text = normalize(&open.text);
        if text.is_empty() {
            debug_assert!(self.pending_ids.is_empty() && self.pending_links.is_empty());
            self.pending_ids = open.ids;
            self.pending_links = open.links;
            return;
        }
        self.blocks.push(RawBlock {
            kind: open.kind,
            text,
            anchor: open.anchor,
            ids: open.ids,
            print_page: open.print_page,
            apparatus: open.apparatus,
            links: open.links,
        });
    }

    fn meet_link(&mut self, el: ElementRef) {
        if el.value().name() != "a" {
            return;
        }
        let Some(href) = el.value().attr("href") else {
            return;
        };
        let link = RawLink {
            href: href.to_owned(),
            id: element_id(el).map(str::to_owned),
            short: short_text(el, MAX_MARKER_CHARS).is_some(),
        };
        match &mut self.open {
            Some(open) => open.links.push(link),
            None => self.pending_links.push(link),
        }
    }

    fn meet_id(&mut self, el: ElementRef) {
        let Some(id) = element_id(el) else { return };
        self.last_id = Some(id.to_owned());
        match &mut self.open {
            Some(open) => open.ids.push(id.to_owned()),
            None => self.pending_ids.push(id.to_owned()),
        }
    }

    /// Reads the page label from, in order, `title`, `aria-label`, the text of
    /// the marker, and the last run of digits in its `id`. A candidate that is
    /// empty or longer than `MAX_PAGE_LABEL_CHARS` is passed over, and a
    /// marker with no usable candidate leaves the current page alone.
    fn page_break(&mut self, el: ElementRef) {
        let label = page_label(el.value().attr("title"))
            .or_else(|| page_label(el.value().attr("aria-label")))
            .or_else(|| {
                short_text(el, MAX_PAGE_LABEL_CHARS).and_then(|text| page_label(Some(&text)))
            })
            .or_else(|| page_label(element_id(el).and_then(last_digits)));
        if label.is_some() {
            *self.print_page = label;
        }
    }
}

/// The element's id, unless it is empty or longer than `MAX_ID_BYTES`.
fn element_id<'a>(el: ElementRef<'a>) -> Option<&'a str> {
    el.value()
        .id()
        .filter(|id| !id.is_empty() && id.len() <= MAX_ID_BYTES)
}

/// The normalized candidate, unless it is empty or, once trimmed, longer than
/// `MAX_PAGE_LABEL_CHARS`. The length is checked before anything is copied.
fn page_label(candidate: Option<&str>) -> Option<String> {
    let trimmed = candidate?.trim();
    if trimmed.chars().take(MAX_PAGE_LABEL_CHARS + 1).count() > MAX_PAGE_LABEL_CHARS {
        return None;
    }
    Some(normalize(trimmed)).filter(|label| !label.is_empty())
}

/// The text of the element, unless it holds more than `max` characters that
/// are not whitespace. A marker wrapped around a chapter stops being read as
/// soon as it is clear that it is no label.
fn short_text(el: ElementRef, max: usize) -> Option<String> {
    let mut text = String::new();
    let mut visible = 0;
    for fragment in el.text() {
        visible += fragment.chars().filter(|c| !c.is_whitespace()).count();
        if visible > max {
            return None;
        }
        text.push_str(fragment);
    }
    Some(text)
}

fn last_digits(id: &str) -> Option<&str> {
    id.split(|c: char| !c.is_ascii_digit())
        .rfind(|run| !run.is_empty())
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
    fn apparatus_comes_from_the_epub_type_or_role_of_an_element_around_the_block() {
        let got = blocks(
            r#"<p>Text.</p><section epub:type="bibliography"><h1>Works</h1><div><p>Book one.</p></div></section><section role="doc-index">loose <p>apple, 3</p></section><p>After.</p>"#,
        );
        let apparatus: Vec<_> = got.iter().map(|b| (b.text.as_str(), b.apparatus)).collect();
        assert_eq!(
            apparatus,
            [
                ("Text.", None),
                ("Works", Some(Apparatus::Bibliography)),
                ("Book one.", Some(Apparatus::Bibliography)),
                ("loose", Some(Apparatus::Index)),
                ("apple, 3", Some(Apparatus::Index)),
                ("After.", None),
            ]
        );
    }

    #[test]
    fn a_body_typed_as_an_index_makes_its_blocks_index() {
        let html = r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>Index</title></head>
<body epub:type="backmatter index"><p>apple, 3</p><p>pear, 5</p></body>
</html>"#;
        let got = xhtml_blocks(html, &mut None);
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|b| b.apparatus == Some(Apparatus::Index)));
    }

    #[test]
    fn links_are_kept_with_their_id_and_whether_their_text_is_short() {
        let got = blocks(
            r##"<p>Claim<a id="r1" href="#n1"><sup>12</sup></a> and <a href="other.xhtml#x">a longer link</a>.</p><div><a id="r2" href="#n2">*</a> loose</div>"##,
        );
        let link = |href: &str, id: Option<&str>, short| RawLink {
            href: href.to_string(),
            id: id.map(str::to_owned),
            short,
        };
        assert_eq!(
            got[0].links,
            [
                link("#n1", Some("r1"), true),
                link("other.xhtml#x", None, false)
            ]
        );
        assert_eq!(got[1].links, [link("#n2", Some("r2"), true)]);
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

    #[test]
    fn overlong_id_is_ignored() {
        let long = "i".repeat(300);
        let got = blocks(&format!(r#"<span id="{long}"></span><p>x</p>"#));
        assert_eq!(pairs(&got), [(BlockKind::Paragraph, "x")]);
        assert_eq!(got[0].anchor, None);
        assert!(got[0].ids.is_empty());

        let at_the_bound = "i".repeat(256);
        let got = blocks(&format!(r#"<span id="{at_the_bound}"></span><p>x</p>"#));
        assert_eq!(got[0].anchor, Some(at_the_bound.clone()));
        assert_eq!(got[0].ids, [at_the_bound]);
    }

    #[test]
    fn overlong_id_is_not_a_note_label() {
        let long = "n".repeat(300);
        let got = blocks(&format!(
            r#"<aside epub:type="footnote" id="{long}">N</aside>"#
        ));
        assert_eq!(pairs(&got), [(BlockKind::Note { label: None }, "N")]);
    }

    #[test]
    fn overlong_page_label_is_skipped() {
        let page_after = |marker: &str| {
            let mut page = Some("old".to_string());
            xhtml_blocks(&document(&format!("{marker}<p>X</p>")), &mut page);
            page
        };
        let some = |s: &str| Some(s.to_string());
        let forty = "4".repeat(40);
        let thirty_two = "3".repeat(32);

        assert_eq!(
            page_after(&format!(
                r#"<span epub:type="pagebreak" title="{forty}" aria-label="12"/>"#
            )),
            some("12")
        );
        assert_eq!(
            page_after(&format!(
                r#"<span epub:type="pagebreak" title="  {thirty_two}  " aria-label="12"/>"#
            )),
            some(&thirty_two)
        );
        // Every candidate is too long: the page stays as it was.
        assert_eq!(
            page_after(&format!(
                r#"<span epub:type="pagebreak" title="{forty}" aria-label="{forty}" id="page_{forty}">{forty}</span>"#
            )),
            some("old")
        );
        // The id digits are the last candidate and are bounded too.
        assert_eq!(
            page_after(&format!(r#"<span epub:type="pagebreak" id="p{forty}"/>"#)),
            some("old")
        );
    }

    #[test]
    fn pagebreak_wrapping_long_text_does_not_become_label() {
        let chapter = "word ".repeat(2_000);
        let page_after = |marker: String| {
            let mut page = Some("old".to_string());
            let got = xhtml_blocks(&document(&format!("{marker}<p>X</p>")), &mut page);
            assert_eq!(pairs(&got), [(BlockKind::Paragraph, "X")]);
            page
        };
        let some = |s: &str| Some(s.to_string());

        assert_eq!(
            page_after(format!(
                r#"<div epub:type="pagebreak"><p>{chapter}</p></div>"#
            )),
            some("old")
        );
        assert_eq!(
            page_after(format!(
                r#"<div epub:type="pagebreak" id="page_12"><p>{chapter}</p></div>"#
            )),
            some("12")
        );
    }

    #[test]
    fn hostile_id_and_label_are_not_copied_into_every_block() {
        let long_id = "a".repeat(100_000);
        let long_label = "9".repeat(100_000);
        let body = format!(
            r#"<span id="{long_id}"></span><span epub:type="pagebreak" title="{long_label}"/>{}"#,
            "<p>x</p>".repeat(1_000)
        );
        let got = blocks(&body);
        assert_eq!(got.len(), 1_000);
        assert!(
            got.iter()
                .all(|b| b.anchor.is_none() && b.print_page.is_none())
        );
    }

    /// Linear code reads these documents in a few seconds at most in a debug
    /// build; quadratic code takes from twenty seconds to minutes.
    const HOSTILE_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

    #[test]
    fn many_empty_blocks_with_ids_take_linear_time() {
        let body: String = (0..150_000)
            .map(|i| format!(r#"<p id="p{i}"></p>"#))
            .collect();
        let started = std::time::Instant::now();

        let got = blocks(&format!("{body}<p>end</p>"));

        assert!(started.elapsed() < HOSTILE_LIMIT, "{:?}", started.elapsed());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].ids.len(), 150_000);
        assert_eq!(got[0].ids[0], "p0");
    }

    #[test]
    fn a_block_of_many_blank_runs_takes_linear_time() {
        let body = format!("<p>{}end</p>", "<b> </b>".repeat(80_000));
        let started = std::time::Instant::now();

        let got = blocks(&body);

        assert!(started.elapsed() < HOSTILE_LIMIT, "{:?}", started.elapsed());
        assert_eq!(pairs(&got), [(BlockKind::Paragraph, "end")]);
    }

    #[test]
    fn the_printed_page_is_the_one_where_the_text_starts() {
        let mut page = Some("1".to_string());
        let html = document(
            r#"<p> <b> </b><span epub:type="pagebreak" title="2"/> text <span epub:type="pagebreak" title="3"/>more</p>"#,
        );

        let got = xhtml_blocks(&html, &mut page);

        assert_eq!(got[0].print_page.as_deref(), Some("2"));
        assert_eq!(page.as_deref(), Some("3"));
    }
}
