//! Reads an EPUB 2 or 3 file into the [`Document`] model.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use rbook::Epub;
use zip::ZipArchive;

use super::assign_sections;
use super::xhtml::{RawLink, xhtml_blocks};
use crate::document::{
    Apparatus, Block, BlockKind, Document, Locator, Metadata, OutlineEntry, SCHEMA_VERSION, Source,
    Warning,
};
use crate::error::{Error, Result};
use crate::normalize::normalize;

/// The most a content document may hold, in bytes, once uncompressed. A larger
/// one is skipped with a warning, so a small archive cannot expand without
/// bound in memory.
const MAX_CONTENT_BYTES: u64 = 32 * 1024 * 1024;

/// The most content, in uncompressed bytes, that is read from one book. Once
/// the content read exceeds it, the rest of the spine is skipped with one
/// warning, so a small archive cannot fill memory with many large documents.
const MAX_BOOK_CONTENT_BYTES: u64 = 256 * 1024 * 1024;

const MIB: u64 = 1024 * 1024;

/// Where the blocks of one content document start, and which of them carry
/// which element id. A table-of-contents target resolves against this.
#[derive(Default)]
struct FileBlocks {
    first: Option<u32>,
    by_id: HashMap<String, u32>,
}

/// Reads the EPUB at `path`: the spine in reading order, non-linear items
/// included, then the table of contents from the navigation document or the NCX.
///
/// A spine item that cannot be read becomes a [`Warning::UnreadableContent`]
/// and is skipped, as is a content document listed again in the spine. A book
/// with no text in any spine item is an error.
pub fn read_epub(path: &Path, source: Source) -> Result<Document> {
    read_epub_within(path, source, MAX_BOOK_CONTENT_BYTES)
}

/// [`read_epub`], reading content documents only until `budget` uncompressed
/// bytes have been exceeded.
fn read_epub_within(path: &Path, source: Source, budget: u64) -> Result<Document> {
    let unreadable = |reason: String| Error::Unreadable {
        path: path.to_path_buf(),
        reason,
    };
    let epub = Epub::open(path).map_err(|e| unreadable(e.to_string()))?;
    let package_dir = epub.package().directory().decode().into_owned();
    // Content documents are read here, not through rbook, which would copy an
    // entry into memory whatever its size.
    let mut archive = File::open(path)
        .map_err(|e| e.to_string())
        .and_then(|file| ZipArchive::new(file).map_err(|e| e.to_string()))
        .map_err(unreadable)?;

    let mut blocks: Vec<Block> = Vec::new();
    let mut warnings = Vec::new();
    let mut files: HashMap<String, FileBlocks> = HashMap::new();
    // The path of each content document read, and for each block the
    // document it is in and the links it holds.
    let mut paths: Vec<String> = Vec::new();
    let mut file_of: Vec<usize> = Vec::new();
    let mut links: Vec<Vec<RawLink>> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut content_read = 0;
    let mut print_page = None;
    for entry in epub.spine().iter() {
        let Some(item) = entry.manifest_entry() else {
            warnings.push(Warning::UnreadableContent {
                href: entry.idref().to_string(),
                reason: "the spine item is not in the manifest".to_string(),
            });
            continue;
        };
        let full_path = item.href().path().decode().into_owned();
        let href = without_controls(&relative_to(&package_dir, &full_path));
        if content_read > budget {
            warnings.push(Warning::UnreadableContent {
                href,
                reason: format!(
                    "book exceeds {} MiB of content; the rest is not read",
                    MAX_BOOK_CONTENT_BYTES / MIB
                ),
            });
            break;
        }
        if !seen.insert(full_path.clone()) {
            warnings.push(Warning::UnreadableContent {
                href,
                reason: "listed again in the spine; read once".to_string(),
            });
            continue;
        }
        let entry_name = full_path.trim_start_matches('/');
        let read = read_entry(
            &mut archive,
            entry_name,
            MAX_CONTENT_BYTES,
            &mut content_read,
        );
        let content = match read {
            Ok(content) => content,
            Err(error) => {
                let reason = match error {
                    EntryError::TooLarge => {
                        format!("content document exceeds {} MiB", MAX_CONTENT_BYTES / MIB)
                    }
                    EntryError::Unreadable(reason) => reason,
                };
                warnings.push(Warning::UnreadableContent { href, reason });
                continue;
            }
        };
        paths.push(full_path.clone());
        let file = files.entry(full_path).or_default();
        for (index, raw) in xhtml_blocks(&content, &mut print_page)
            .into_iter()
            .enumerate()
        {
            let id = blocks.len() as u32;
            file.first.get_or_insert(id);
            for element_id in raw.ids {
                file.by_id.entry(element_id).or_insert(id);
            }
            file_of.push(paths.len() - 1);
            links.push(raw.links);
            blocks.push(Block {
                id,
                kind: raw.kind,
                text: raw.text,
                orig: None,
                spans: Vec::new(),
                section: Vec::new(),
                apparatus: raw.apparatus,
                locator: Locator::Epub {
                    href: href.clone(),
                    anchor: raw.anchor,
                    para: index as u32 + 1,
                    print_page: raw.print_page,
                },
            });
        }
    }
    if blocks.is_empty() {
        return Err(unreadable("no readable text".to_string()));
    }
    assign_sections(&mut blocks);
    mark_linked_notes(&mut blocks, &links, &file_of, &paths, &files);

    let metadata = epub.metadata();
    Ok(Document {
        schema_version: SCHEMA_VERSION,
        source,
        metadata: Metadata {
            title: metadata
                .title()
                .map(|title| normalize(title.value()))
                .filter(|title| !title.is_empty()),
            authors: metadata
                .creators()
                .map(|creator| normalize(creator.value()))
                .filter(|author| !author.is_empty())
                .collect(),
            language: metadata
                .language()
                .map(|language| language.value().trim().to_string())
                .filter(|language| !language.is_empty()),
            page_count: None,
            spine_count: Some(epub.spine().len() as u32),
        },
        outline: outline(&epub, &files),
        blocks,
        furniture: Vec::new(),
        warnings,
    })
}

/// The table of contents in reading order, each entry pointing at the first
/// block of its target: the block carrying the fragment's element id, or the
/// first block of the file when the fragment is missing or matches none.
fn outline(epub: &Epub, files: &HashMap<String, FileBlocks>) -> Vec<OutlineEntry> {
    let Some(contents) = epub.toc().contents() else {
        return Vec::new();
    };
    let mut outline = Vec::new();
    for entry in contents.flatten() {
        let title = normalize(entry.label());
        if title.is_empty() {
            continue;
        }
        let block_id = entry.href().and_then(|href| {
            let path = href.path().decode();
            let file = files.get(&*path)?;
            let decoded = href.decode();
            let fragment = decoded
                .get(path.len()..)
                .and_then(|rest| rest.split_once('#'))
                .map(|(_, fragment)| fragment);
            fragment
                .and_then(|fragment| file.by_id.get(fragment))
                .or(file.first.as_ref())
                .copied()
        });
        outline.push(OutlineEntry {
            id: outline.len() as u32,
            title,
            level: u8::try_from(entry.depth()).unwrap_or(u8::MAX),
            block_id,
        });
    }
    outline
}

/// Makes a note of each paragraph or list item that a marker outside it links
/// to and that links back to the marker: how an EPUB without `epub:type`, such
/// as one converted from another format, ties a note to its place in the text.
/// A marker is a link with an id and short text ([`RawLink::short`]) outside
/// headings and outside contents, lists, bibliography and index, whose entries
/// link to captions and headings that may link back. The note's label is the
/// id the marker links to. Runs after [`assign_sections`], which gives blocks
/// their apparatus.
fn mark_linked_notes(
    blocks: &mut [Block],
    links: &[Vec<RawLink>],
    file_of: &[usize],
    paths: &[String],
    files: &HashMap<String, FileBlocks>,
) {
    // Each marker by its document and id, with the document and id it links
    // to and the block it is in.
    let mut markers: HashMap<(&str, &str), (String, String, u32)> = HashMap::new();
    for (block, held) in blocks.iter().zip(links) {
        let may_mark = matches!(block.apparatus, None | Some(Apparatus::Notes));
        if matches!(block.kind, BlockKind::Heading { .. }) || !may_mark {
            continue;
        }
        let path = paths[file_of[block.id as usize]].as_str();
        for link in held.iter().filter(|link| link.short) {
            let Some(id) = link.id.as_deref() else {
                continue;
            };
            if let Some((to_path, to_id)) = resolve_link(path, &link.href) {
                markers
                    .entry((path, id))
                    .or_insert((to_path, to_id, block.id));
            }
        }
    }
    for (block, held) in blocks.iter_mut().zip(links) {
        if !matches!(block.kind, BlockKind::Paragraph | BlockKind::ListItem) {
            continue;
        }
        let path = paths[file_of[block.id as usize]].as_str();
        let label = held.iter().find_map(|link| {
            let (to_path, to_id) = resolve_link(path, &link.href)?;
            let (back_path, back_id, marker_block) =
                markers.get(&(to_path.as_str(), to_id.as_str()))?;
            let carries_back_id = files.get(back_path)?.by_id.get(back_id) == Some(&block.id);
            (*marker_block != block.id && back_path == path && carries_back_id)
                .then(|| back_id.clone())
        });
        if let Some(label) = label {
            block.kind = BlockKind::Note { label: Some(label) };
        }
    }
}

/// The content document and fragment that `href`, met in the document at
/// `base`, links to. `None` for a link without a fragment, and for one with a
/// scheme such as `https:` or `mailto:`, which leaves the book.
fn resolve_link(base: &str, href: &str) -> Option<(String, String)> {
    let (path, fragment) = href.split_once('#')?;
    if fragment.is_empty() || path.contains(':') {
        return None;
    }
    let path = percent_decode(path);
    let target = if path.is_empty() {
        base.to_string()
    } else {
        join_path(base, &path)
    };
    Some((target, percent_decode(fragment)))
}

/// `path` read from the directory of the document at `base`, or from the root
/// of the book when it starts with `/`, with `.` and `..` resolved. The result
/// starts with `/` when `base` does.
fn join_path(base: &str, path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    if !path.starts_with('/') {
        segments.extend(base.split('/').filter(|s| !s.is_empty()));
        segments.pop();
    }
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    let joined = segments.join("/");
    if base.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}

/// `text` with each `%` and two hex digits replaced by the byte they encode.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug)]
enum EntryError {
    /// The entry holds more than the cap once uncompressed.
    TooLarge,
    Unreadable(String),
}

/// Reads the archive entry `name` as text, refusing one that holds more than
/// `cap` bytes, and adds the bytes it uncompressed to `read`. The count is of
/// the bytes that actually arrive, because the size an archive declares for an
/// entry can be false.
fn read_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    cap: u64,
    read: &mut u64,
) -> std::result::Result<String, EntryError> {
    let entry = archive
        .by_name(name)
        .map_err(|e| EntryError::Unreadable(e.to_string()))?;
    let mut bytes = Vec::new();
    let result = entry.take(cap + 1).read_to_end(&mut bytes);
    *read += bytes.len() as u64;
    result.map_err(|e| EntryError::Unreadable(e.to_string()))?;
    if bytes.len() as u64 > cap {
        return Err(EntryError::TooLarge);
    }
    decode_text(&bytes).map_err(EntryError::Unreadable)
}

/// UTF-8, or UTF-16 when the bytes start with a byte order mark.
fn decode_text(bytes: &[u8]) -> std::result::Result<String, String> {
    let (little_endian, units) = match bytes {
        [0xFF, 0xFE, rest @ ..] => (true, rest),
        [0xFE, 0xFF, rest @ ..] => (false, rest),
        _ => {
            return String::from_utf8(bytes.to_vec())
                .map_err(|_| "content is not valid UTF-8".to_string());
        }
    };
    let (pairs, rest) = units.as_chunks::<2>();
    if !rest.is_empty() {
        return Err("content is not valid UTF-16".to_string());
    }
    let units = pairs.iter().map(|&pair| match little_endian {
        true => u16::from_le_bytes(pair),
        false => u16::from_be_bytes(pair),
    });
    char::decode_utf16(units)
        .collect::<std::result::Result<String, _>>()
        .map_err(|_| "content is not valid UTF-16".to_string())
}

/// `href` without control characters, which a terminal would act on when the
/// locator is printed.
fn without_controls(href: &str) -> String {
    href.chars().filter(|c| !c.is_control()).collect()
}

/// The path of `path` below `dir`, both absolute. A path outside `dir` keeps
/// all its components.
fn relative_to(dir: &str, path: &str) -> String {
    path.strip_prefix(dir.trim_end_matches('/'))
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or_else(|| path.trim_start_matches('/'))
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipArchive, ZipWriter};

    use super::*;
    use crate::document::Format;

    /// A content document: its manifest href, its entry name below `OEBPS/`,
    /// and its body.
    type Item<'a> = (&'a str, &'a str, &'a str);

    fn chapter(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>C</title></head><body>{body}</body></html>"#
        )
    }

    /// Writes an EPUB 3 with `items` in its manifest and the items at the
    /// indexes `spine` in its spine.
    fn write_epub(path: &Path, items: &[Item], spine: &[usize]) {
        let manifest: String = items
            .iter()
            .enumerate()
            .map(|(i, (href, _, _))| {
                format!(r#"<item id="c{i}" href="{href}" media-type="application/xhtml+xml"/>"#)
            })
            .collect();
        let itemrefs: String = spine
            .iter()
            .map(|i| format!(r#"<itemref idref="c{i}"/>"#))
            .collect();
        let opf = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:identifier id="uid">urn:uuid:0</dc:identifier><dc:title>T</dc:title><dc:language>en</dc:language>
<meta property="dcterms:modified">2024-01-01T00:00:00Z</meta>
</metadata>
<manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>{manifest}</manifest>
<spine>{itemrefs}</spine>
</package>"#
        );
        let nav = r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>Contents</title></head><body><nav epub:type="toc"><ol></ol></nav></body></html>"#;
        let container = r#"<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;
        let mut zip = ZipWriter::new(std::fs::File::create(path).unwrap());
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let mut add = |name: &str, data: &[u8]| {
            zip.start_file(name, stored).unwrap();
            zip.write_all(data).unwrap();
        };
        add("mimetype", b"application/epub+zip");
        add("META-INF/container.xml", container.as_bytes());
        add("OEBPS/content.opf", opf.as_bytes());
        add("OEBPS/nav.xhtml", nav.as_bytes());
        for (_, entry, body) in items {
            add(&format!("OEBPS/{entry}"), chapter(body).as_bytes());
        }
        zip.finish().unwrap();
    }

    fn read_with_budget(items: &[Item], spine: &[usize], budget: u64) -> Document {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("book.epub");
        write_epub(&path, items, spine);
        let source = Source {
            sha256: "abc".to_string(),
            file_name: "book.epub".to_string(),
            format: Format::Epub,
        };
        read_epub_within(&path, source, budget).unwrap()
    }

    fn texts(doc: &Document) -> Vec<&str> {
        doc.blocks.iter().map(|b| b.text.as_str()).collect()
    }

    fn unreadable(href: &str, reason: &str) -> Warning {
        Warning::UnreadableContent {
            href: href.to_string(),
            reason: reason.to_string(),
        }
    }

    #[test]
    fn a_document_listed_again_in_the_spine_is_read_once() {
        let items = [
            ("ch1.xhtml", "ch1.xhtml", "<p>One.</p>"),
            ("ch2.xhtml", "ch2.xhtml", "<p>Two.</p>"),
        ];

        let doc = read_with_budget(&items, &[0, 1, 0, 0], MAX_BOOK_CONTENT_BYTES);

        assert_eq!(texts(&doc), ["One.", "Two."]);
        assert_eq!(
            doc.warnings,
            [
                unreadable("ch1.xhtml", "listed again in the spine; read once"),
                unreadable("ch1.xhtml", "listed again in the spine; read once"),
            ]
        );
    }

    #[test]
    fn reading_stops_once_the_book_exceeds_its_content_budget() {
        let items = [
            ("ch1.xhtml", "ch1.xhtml", "<p>One.</p>"),
            ("ch2.xhtml", "ch2.xhtml", "<p>Two.</p>"),
            ("ch3.xhtml", "ch3.xhtml", "<p>Three.</p>"),
            ("ch4.xhtml", "ch4.xhtml", "<p>Four.</p>"),
        ];
        // The first document fits the budget exactly; the second takes the
        // book past it.
        let budget = chapter("<p>One.</p>").len() as u64;

        let doc = read_with_budget(&items, &[0, 1, 2, 3], budget);

        assert_eq!(texts(&doc), ["One.", "Two."]);
        assert_eq!(
            doc.warnings,
            [unreadable(
                "ch3.xhtml",
                "book exceeds 256 MiB of content; the rest is not read"
            )]
        );
    }

    #[test]
    fn control_characters_are_dropped_from_hrefs() {
        let items = [("ch%1B1.xhtml", "ch\u{1b}1.xhtml", "<p>One.</p>")];

        let doc = read_with_budget(&items, &[0], MAX_BOOK_CONTENT_BYTES);

        assert_eq!(texts(&doc), ["One."]);
        assert!(
            matches!(&doc.blocks[0].locator, Locator::Epub { href, .. } if href == "ch1.xhtml"),
            "{:?}",
            doc.blocks[0].locator
        );
    }

    fn archive(entries: &[(&str, &[u8], CompressionMethod)]) -> ZipArchive<Cursor<Vec<u8>>> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data, method) in entries {
            zip.start_file(
                *name,
                SimpleFileOptions::default().compression_method(*method),
            )
            .unwrap();
            zip.write_all(data).unwrap();
        }
        ZipArchive::new(zip.finish().unwrap()).unwrap()
    }

    fn stored(data: &[u8]) -> ZipArchive<Cursor<Vec<u8>>> {
        archive(&[("a.xhtml", data, CompressionMethod::Stored)])
    }

    #[test]
    fn an_entry_at_the_cap_is_read_and_one_byte_more_is_refused() {
        assert_eq!(
            read_entry(&mut stored(b"0123456789"), "a.xhtml", 10, &mut 0).ok(),
            Some("0123456789".to_string())
        );
        assert!(matches!(
            read_entry(&mut stored(b"0123456789A"), "a.xhtml", 10, &mut 0),
            Err(EntryError::TooLarge)
        ));
    }

    #[test]
    fn an_entry_that_expands_past_the_cap_is_refused_whatever_it_declares() {
        let bomb = vec![b' '; 4 * 1024 * 1024];
        let mut zip = archive(&[("a.xhtml", &bomb, CompressionMethod::Deflated)]);

        assert!(matches!(
            read_entry(&mut zip, "a.xhtml", 1024, &mut 0),
            Err(EntryError::TooLarge)
        ));
        assert_eq!(
            read_entry(&mut zip, "a.xhtml", bomb.len() as u64, &mut 0)
                .unwrap()
                .len(),
            bomb.len()
        );
    }

    #[test]
    fn the_bytes_uncompressed_are_counted_whether_or_not_the_entry_is_read() {
        let mut read = 0;
        read_entry(&mut stored(b"0123456789"), "a.xhtml", 10, &mut read).unwrap();
        assert_eq!(read, 10);
        let refused = read_entry(&mut stored(b"0123456789AB"), "a.xhtml", 10, &mut read);
        assert!(matches!(refused, Err(EntryError::TooLarge)));
        assert_eq!(read, 21);
        let invalid = read_entry(&mut stored(b"caf\xE9"), "a.xhtml", 10, &mut read);
        assert!(matches!(invalid, Err(EntryError::Unreadable(_))));
        assert_eq!(read, 25);
    }

    #[test]
    fn utf16_with_a_byte_order_mark_is_decoded() {
        let mut le = vec![0xFF, 0xFE];
        let mut be = vec![0xFE, 0xFF];
        for unit in "<p>é</p>".encode_utf16() {
            le.extend_from_slice(&unit.to_le_bytes());
            be.extend_from_slice(&unit.to_be_bytes());
        }

        assert_eq!(
            read_entry(&mut stored(&le), "a.xhtml", 100, &mut 0).ok(),
            Some("<p>é</p>".to_string())
        );
        assert_eq!(
            read_entry(&mut stored(&be), "a.xhtml", 100, &mut 0).ok(),
            Some("<p>é</p>".to_string())
        );
        assert!(matches!(
            read_entry(&mut stored(&[0xFF, 0xFE, 0x41]), "a.xhtml", 100, &mut 0),
            Err(EntryError::Unreadable(_))
        ));
    }

    #[test]
    fn a_missing_entry_and_invalid_utf8_are_unreadable() {
        assert!(matches!(
            read_entry(&mut stored(b"x"), "b.xhtml", 100, &mut 0),
            Err(EntryError::Unreadable(_))
        ));
        assert!(matches!(
            read_entry(&mut stored(b"caf\xE9"), "a.xhtml", 100, &mut 0),
            Err(EntryError::Unreadable(_))
        ));
    }

    #[test]
    fn paths_are_relative_to_the_package_directory() {
        assert_eq!(relative_to("/OEBPS", "/OEBPS/ch1.xhtml"), "ch1.xhtml");
        assert_eq!(
            relative_to("/OEBPS", "/OEBPS/text/ch 1.xhtml"),
            "text/ch 1.xhtml"
        );
        assert_eq!(relative_to("/", "/ch1.xhtml"), "ch1.xhtml");
        assert_eq!(relative_to("", "/ch1.xhtml"), "ch1.xhtml");
        assert_eq!(relative_to("/OEBPS", "/shared/a.xhtml"), "shared/a.xhtml");
        assert_eq!(relative_to("/OEBPS", "/OEBPS2/a.xhtml"), "OEBPS2/a.xhtml");
    }
}
