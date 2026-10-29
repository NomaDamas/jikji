#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

mod archive;
mod binary;
mod container;
mod legacy_office;
mod media;
mod rtf;
mod structured;
mod text;
mod utils;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveLimits {
    pub max_entries: usize,
    pub max_entry_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_entries: 1_000,
            max_entry_bytes: 16 * 1024 * 1024,
            max_total_bytes: 128 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ParserInput<'a> {
    pub path: &'a Path,
    pub bytes: &'a [u8],
    pub max_chars: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseStatus {
    Success,
    ArchiveListing,
    MetadataOnly,
    Failed,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDocument {
    pub text: String,
    pub metadata: BTreeMap<String, String>,
    pub status: ParseStatus,
    pub parser_name: &'static str,
}

impl ParsedDocument {
    pub fn new(text: String, status: ParseStatus, parser_name: &'static str) -> Self {
        Self {
            text,
            metadata: BTreeMap::new(),
            status,
            parser_name,
        }
    }

    pub fn unsupported() -> Self {
        Self::new(String::new(), ParseStatus::Unsupported, "unsupported")
    }

    pub fn failed(parser_name: &'static str) -> Self {
        Self::new(String::new(), ParseStatus::Failed, parser_name)
    }
}

pub trait DocumentParser: Send + Sync {
    fn name(&self) -> &'static str;
    fn supports_extension(&self, extension: &str) -> bool;
    fn parse(&self, input: ParserInput<'_>) -> ParsedDocument;
}

#[derive(Default)]
pub struct ParserRegistry {
    parsers: Vec<Box<dyn DocumentParser>>,
}

impl ParserRegistry {
    pub fn with_defaults() -> Self {
        Self {
            parsers: vec![
                Box::<text::PlainTextParser>::default(),
                Box::<text::SubtitleParser>::default(),
                Box::<text::HtmlParser>::default(),
                Box::<text::RtfParser>::default(),
                Box::<archive::ArchiveParser>::default(),
                Box::<structured::EmailParser>::default(),
                Box::<structured::CalendarParser>::default(),
                Box::<structured::SqliteParser>::default(),
                Box::<container::EpubParser>::default(),
                Box::<container::OfficeOpenXmlParser>::default(),
                Box::<container::OpenDocumentParser>::default(),
                Box::<container::HwpxParser>::default(),
                Box::<binary::PdfParser>::default(),
                Box::<binary::LegacyOfficeParser>::default(),
                Box::<binary::BinaryHwpParser>::default(),
                Box::<media::MediaMetadataParser>::default(),
            ],
        }
    }

    pub fn with_default_stubs() -> Self {
        Self::with_defaults()
    }

    pub fn parser_for_extension(&self, extension: &str) -> Option<&dyn DocumentParser> {
        let normalized = normalize_extension(extension);
        self.parsers
            .iter()
            .map(Box::as_ref)
            .find(|parser| parser.supports_extension(normalized.as_str()))
    }

    pub fn parse_path(&self, path: &Path, max_chars: usize) -> ParsedDocument {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(_) => return ParsedDocument::failed("filesystem"),
        };
        self.parse_path_with_bytes(path, &bytes, max_chars)
    }

    pub fn parse_bytes(&self, name: &str, bytes: &[u8], max_chars: usize) -> ParsedDocument {
        let path = PathBuf::from(name);
        self.parse_path_with_bytes(path.as_path(), bytes, max_chars)
    }

    pub fn parse_path_deep(
        &self,
        path: &Path,
        max_chars: usize,
        limits: ArchiveLimits,
    ) -> ParsedDocument {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(_) => return ParsedDocument::failed("filesystem"),
        };
        archive::parse_deep(self, path, &bytes, max_chars, limits)
    }

    pub fn parse_bytes_deep(
        &self,
        name: &str,
        bytes: &[u8],
        max_chars: usize,
        limits: ArchiveLimits,
    ) -> ParsedDocument {
        let path = PathBuf::from(name);
        archive::parse_deep(self, &path, bytes, max_chars, limits)
    }

    fn parse_path_with_bytes(&self, path: &Path, bytes: &[u8], max_chars: usize) -> ParsedDocument {
        let extension = extension_for_path(path);
        // Legacy Office names are often reused for other formats: Word saves RTF, HTML or a
        // renamed .docx as ".doc". Pick the parser from the content in that case.
        let effective = match (extension.as_str(), sniff_document_format(bytes)) {
            ("doc" | "dot" | "xls" | "ppt" | "pps", Some(sniffed)) => sniffed,
            _ => extension.as_str(),
        };
        let Some(parser) = self.parser_for_extension(effective) else {
            return ParsedDocument::unsupported();
        };
        parser.parse(ParserInput {
            path,
            bytes,
            max_chars,
        })
    }
}

/// Document format recognised from the leading bytes, for files whose extension lies.
/// `None` for compound (OLE) files and anything unrecognised, which keep their extension.
fn sniff_document_format(bytes: &[u8]) -> Option<&'static str> {
    let head = bytes.get(..bytes.len().min(512)).unwrap_or_default();
    if head.starts_with(b"{\\rtf") {
        return Some("rtf");
    }
    if head.starts_with(b"PK\x03\x04") {
        return Some("docx");
    }
    let text = String::from_utf8_lossy(head)
        .trim_start_matches('\u{feff}')
        .trim_start()
        .to_ascii_lowercase();
    if text.starts_with("<html")
        || text.starts_with("<!doctype html")
        || (text.starts_with("<?xml") && text.contains("<html"))
    {
        return Some("html");
    }
    if text.starts_with("mime-version:") {
        return Some("eml");
    }
    None
}

fn normalize_extension(extension: &str) -> String {
    extension.trim_start_matches('.').to_ascii_lowercase()
}

fn extension_for_path(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|part| part.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    for compound in ["tar.gz", "tar.bz2", "tar.xz"] {
        if name.ends_with(compound) {
            return compound.to_owned();
        }
    }
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(normalize_extension)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{ParseStatus, ParserRegistry};

    #[test]
    fn registry_resolves_plain_text_when_extension_is_text() {
        let registry = ParserRegistry::with_defaults();

        let parsed = registry.parse_bytes("note.txt", b"hello", 1_000);

        assert_eq!(parsed.text, "hello");
        assert_eq!(parsed.status, ParseStatus::Success);
    }
}
