//! Reads an EPUB 2 or 3 file into the [`Document`] model.

use std::collections::HashMap;
use std::path::Path;

use rbook::Epub;

use super::{assign_sections, xhtml::xhtml_blocks};
use crate::document::{
    Block, Document, Locator, Metadata, OutlineEntry, SCHEMA_VERSION, Source, Warning,
};
use crate::error::{Error, Result};
use crate::normalize::normalize;

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
        let content = match item.read_str() {
            Ok(content) => content,
            Err(error) => {
                warnings.push(Warning::UnreadableContent {
                    href,
                    reason: error.to_string(),
                });
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
    use super::relative_to;

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
