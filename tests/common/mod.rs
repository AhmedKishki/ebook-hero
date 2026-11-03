//! Fixture books for the integration tests.

#![allow(dead_code)]

use std::fs::File;
use std::io::Write;
use std::path::Path;

use zip::CompressionMethod;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

/// A temporary directory that only this user can write. The cache refuses a
/// root that others can write, and `TempDir::new` honours the umask, which is
/// 002 on many systems.
pub fn private_tempdir() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

const IDENTIFIER: &str = "urn:uuid:5f3c8a1e-7b2d-4c6e-9a10-3d2f4b5c6d7e";
const FONT_PATH: &str = "fonts/book.otf";
const FONT_LEN: usize = 2048;
/// The IDPF algorithm obfuscates only this many leading bytes.
const OBFUSCATED_LEN: usize = 1040;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EpubVersion {
    V2,
    V3,
}

enum Item {
    Chapter {
        href: String,
        body: String,
        linear: bool,
    },
    Missing {
        href: String,
    },
}

impl Item {
    fn href(&self) -> &str {
        match self {
            Item::Chapter { href, .. } | Item::Missing { href } => href,
        }
    }

    fn linear(&self) -> bool {
        match self {
            Item::Chapter { linear, .. } => *linear,
            Item::Missing { .. } => true,
        }
    }
}

struct TocEntry {
    title: String,
    target: String,
    level: u8,
}

/// Writes a small, valid EPUB 2 or 3 file. Chapter bodies are the inner XHTML
/// of `<body>`, and every `href` and TOC target is relative to `OEBPS/`.
pub struct EpubBuilder {
    version: EpubVersion,
    title: String,
    authors: Vec<String>,
    items: Vec<Item>,
    toc: Vec<TocEntry>,
    obfuscated_font: bool,
}

impl EpubBuilder {
    pub fn new(version: EpubVersion) -> Self {
        EpubBuilder {
            version,
            title: "Test Book".to_string(),
            authors: Vec::new(),
            items: Vec::new(),
            toc: Vec::new(),
            obfuscated_font: false,
        }
    }

    pub fn title(mut self, title: &str) -> Self {
        self.title = title.to_string();
        self
    }

    pub fn author(mut self, author: &str) -> Self {
        self.authors.push(author.to_string());
        self
    }

    pub fn chapter(mut self, href: &str, body: &str) -> Self {
        self.items.push(Item::Chapter {
            href: href.to_string(),
            body: body.to_string(),
            linear: true,
        });
        self
    }

    pub fn nonlinear_chapter(mut self, href: &str, body: &str) -> Self {
        self.items.push(Item::Chapter {
            href: href.to_string(),
            body: body.to_string(),
            linear: false,
        });
        self
    }

    /// Adds a TOC entry. `level` 1 is a top-level entry; an entry nests under
    /// the closest earlier entry with a lower level.
    pub fn toc(mut self, title: &str, target: &str, level: u8) -> Self {
        self.toc.push(TocEntry {
            title: title.to_string(),
            target: target.to_string(),
            level,
        });
        self
    }

    /// Lists a content document in the manifest and the spine without storing it.
    pub fn missing_chapter(mut self, href: &str) -> Self {
        self.items.push(Item::Missing {
            href: href.to_string(),
        });
        self
    }

    /// Embeds a font obfuscated with the IDPF algorithm and declares it in
    /// `META-INF/encryption.xml`.
    pub fn obfuscated_font(mut self) -> Self {
        self.obfuscated_font = true;
        self
    }

    pub fn write(&self, path: &Path) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let mut add = |name: &str, data: &[u8]| {
            zip.start_file(name, stored).unwrap();
            zip.write_all(data).unwrap();
        };
        add("mimetype", b"application/epub+zip");
        add("META-INF/container.xml", CONTAINER.as_bytes());
        if self.obfuscated_font {
            add("META-INF/encryption.xml", encryption_xml().as_bytes());
            add(&format!("OEBPS/{FONT_PATH}"), &obfuscated_font_bytes());
        }
        add("OEBPS/content.opf", self.opf().as_bytes());
        match self.version {
            EpubVersion::V2 => add("OEBPS/toc.ncx", self.ncx().as_bytes()),
            EpubVersion::V3 => add("OEBPS/nav.xhtml", self.nav().as_bytes()),
        }
        for item in &self.items {
            if let Item::Chapter { href, body, .. } = item {
                add(&format!("OEBPS/{href}"), chapter_xhtml(body).as_bytes());
            }
        }
        zip.finish().unwrap();
    }

    fn opf(&self) -> String {
        let (version, modified) = match self.version {
            EpubVersion::V2 => ("2.0", ""),
            EpubVersion::V3 => (
                "3.0",
                r#"<meta property="dcterms:modified">2024-01-01T00:00:00Z</meta>"#,
            ),
        };
        let creators: String = self
            .authors
            .iter()
            .map(|a| format!("<dc:creator>{}</dc:creator>", escape(a)))
            .collect();
        let mut manifest = match self.version {
            EpubVersion::V2 => r#"<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>"#,
            EpubVersion::V3 => r#"<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>"#,
        }
        .to_string();
        let mut spine = String::new();
        for (i, item) in self.items.iter().enumerate() {
            manifest.push_str(&format!(
                r#"<item id="item{i}" href="{}" media-type="application/xhtml+xml"/>"#,
                escape(item.href())
            ));
            let linear = if item.linear() { "" } else { r#" linear="no""# };
            spine.push_str(&format!(r#"<itemref idref="item{i}"{linear}/>"#));
        }
        if self.obfuscated_font {
            manifest.push_str(&format!(
                r#"<item id="font" href="{FONT_PATH}" media-type="application/vnd.ms-opentype"/>"#
            ));
        }
        let toc_attr = match self.version {
            EpubVersion::V2 => r#" toc="ncx""#,
            EpubVersion::V3 => "",
        };
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="{version}" unique-identifier="uid">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:identifier id="uid">{IDENTIFIER}</dc:identifier>
<dc:title>{title}</dc:title>
{creators}
<dc:language>en</dc:language>
{modified}
</metadata>
<manifest>{manifest}</manifest>
<spine{toc_attr}>{spine}</spine>
</package>
"#,
            title = escape(&self.title)
        )
    }

    fn nav(&self) -> String {
        let mut next = 0;
        let mut list = self.nav_list(&mut next, 1);
        if list.is_empty() {
            list = "<ol></ol>".to_string();
        }
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>Contents</title></head>
<body><nav epub:type="toc"><h1>Contents</h1>{list}</nav></body>
</html>
"#
        )
    }

    /// Renders the entries from `*next` on whose level is at least `level`,
    /// or nothing when there are none.
    fn nav_list(&self, next: &mut usize, level: u8) -> String {
        let mut items = String::new();
        while let Some(entry) = self.toc.get(*next) {
            if entry.level < level {
                break;
            }
            *next += 1;
            let children = self.nav_list(next, entry.level + 1);
            items.push_str(&format!(
                r#"<li><a href="{}">{}</a>{children}</li>"#,
                escape(&entry.target),
                escape(&entry.title)
            ));
        }
        if items.is_empty() {
            items
        } else {
            format!("<ol>{items}</ol>")
        }
    }

    fn ncx(&self) -> String {
        let mut next = 0;
        let mut order = 0;
        let points = self.ncx_points(&mut next, &mut order, 1);
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
<head><meta name="dtb:uid" content="{IDENTIFIER}"/></head>
<docTitle><text>{title}</text></docTitle>
<navMap>{points}</navMap>
</ncx>
"#,
            title = escape(&self.title)
        )
    }

    fn ncx_points(&self, next: &mut usize, order: &mut usize, level: u8) -> String {
        let mut out = String::new();
        while let Some(entry) = self.toc.get(*next) {
            if entry.level < level {
                break;
            }
            *next += 1;
            *order += 1;
            let play_order = *order;
            let children = self.ncx_points(next, order, entry.level + 1);
            out.push_str(&format!(
                r#"<navPoint id="np{play_order}" playOrder="{play_order}"><navLabel><text>{}</text></navLabel><content src="{}"/>{children}</navPoint>"#,
                escape(&entry.title),
                escape(&entry.target)
            ));
        }
        out
    }
}

const CONTAINER: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>
"#;

fn chapter_xhtml(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>Chapter</title></head>
<body>{body}</body>
</html>
"#
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn encryption_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
<enc:EncryptedData>
<enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/>
<enc:CipherData><enc:CipherReference URI="OEBPS/{FONT_PATH}"/></enc:CipherData>
</enc:EncryptedData>
</encryption>
"#
    )
}

/// A stand-in font whose first 1040 bytes are XORed with the SHA-1 of the
/// package's unique identifier, as the IDPF font-obfuscation algorithm says.
fn obfuscated_font_bytes() -> Vec<u8> {
    let key = sha1(IDENTIFIER.split_whitespace().collect::<String>().as_bytes());
    let mut bytes: Vec<u8> = (0..FONT_LEN).map(|i| (i % 251) as u8).collect();
    for (byte, key_byte) in bytes[..OBFUSCATED_LEN].iter_mut().zip(key.iter().cycle()) {
        *byte ^= key_byte;
    }
    bytes
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d, e]) {
            *s = s.wrapping_add(v);
        }
    }
    let mut digest = [0u8; 20];
    for (chunk, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        *chunk = word.to_be_bytes();
    }
    digest
}
