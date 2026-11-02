//! Reads an EPUB 2 or 3 file into the [`Document`] model.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use rbook::Epub;
use zip::ZipArchive;

use super::{assign_sections, xhtml::xhtml_blocks};
use crate::document::{
    Block, Document, Locator, Metadata, OutlineEntry, SCHEMA_VERSION, Source, Warning,
};
use crate::error::{Error, Result};
use crate::normalize::normalize;

/// The most a content document may hold, in bytes, once uncompressed. A larger
/// one is skipped with a warning, so a small archive cannot expand without
/// bound in memory.
const MAX_CONTENT_BYTES: u64 = 32 * 1024 * 1024;

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
/// and is skipped. A book with no text in any spine item is an error.
pub fn read_epub(path: &Path, source: Source) -> Result<Document> {
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
        let href = relative_to(&package_dir, &full_path);
        let entry_name = full_path.trim_start_matches('/');
        let content = match read_entry(&mut archive, entry_name, MAX_CONTENT_BYTES) {
            Ok(content) => content,
            Err(error) => {
                let reason = match error {
                    EntryError::TooLarge => format!(
                        "content document exceeds {} MiB",
                        MAX_CONTENT_BYTES / (1024 * 1024)
                    ),
                    EntryError::Unreadable(reason) => reason,
                };
                warnings.push(Warning::UnreadableContent { href, reason });
                continue;
            }
        };
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
            blocks.push(Block {
                id,
                kind: raw.kind,
                text: raw.text,
                orig: None,
                spans: Vec::new(),
                section: Vec::new(),
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

#[derive(Debug)]
enum EntryError {
    /// The entry holds more than the cap once uncompressed.
    TooLarge,
    Unreadable(String),
}

/// Reads the archive entry `name` as text, refusing one that holds more than
/// `cap` bytes. The count is of the bytes that actually arrive, because the
/// size an archive declares for an entry can be false.
fn read_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    cap: u64,
) -> std::result::Result<String, EntryError> {
    let entry = archive
        .by_name(name)
        .map_err(|e| EntryError::Unreadable(e.to_string()))?;
    let mut bytes = Vec::new();
    entry
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| EntryError::Unreadable(e.to_string()))?;
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

    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipArchive, ZipWriter};

    use super::{EntryError, read_entry, relative_to};

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
            read_entry(&mut stored(b"0123456789"), "a.xhtml", 10).ok(),
            Some("0123456789".to_string())
        );
        assert!(matches!(
            read_entry(&mut stored(b"0123456789A"), "a.xhtml", 10),
            Err(EntryError::TooLarge)
        ));
    }

    #[test]
    fn an_entry_that_expands_past_the_cap_is_refused_whatever_it_declares() {
        let bomb = vec![b' '; 4 * 1024 * 1024];
        let mut zip = archive(&[("a.xhtml", &bomb, CompressionMethod::Deflated)]);

        assert!(matches!(
            read_entry(&mut zip, "a.xhtml", 1024),
            Err(EntryError::TooLarge)
        ));
        assert_eq!(
            read_entry(&mut zip, "a.xhtml", bomb.len() as u64)
                .unwrap()
                .len(),
            bomb.len()
        );
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
            read_entry(&mut stored(&le), "a.xhtml", 100).ok(),
            Some("<p>é</p>".to_string())
        );
        assert_eq!(
            read_entry(&mut stored(&be), "a.xhtml", 100).ok(),
            Some("<p>é</p>".to_string())
        );
        assert!(matches!(
            read_entry(&mut stored(&[0xFF, 0xFE, 0x41]), "a.xhtml", 100),
            Err(EntryError::Unreadable(_))
        ));
    }

    #[test]
    fn a_missing_entry_and_invalid_utf8_are_unreadable() {
        assert!(matches!(
            read_entry(&mut stored(b"x"), "b.xhtml", 100),
            Err(EntryError::Unreadable(_))
        ));
        assert!(matches!(
            read_entry(&mut stored(b"caf\xE9"), "a.xhtml", 100),
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
