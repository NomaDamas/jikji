use std::io::{Cursor, Read};

use cfb::CompoundFile;
use flate2::read::DeflateDecoder;

use crate::utils::cap_chars;
use crate::{DocumentParser, ParseStatus, ParsedDocument, ParserInput};

#[derive(Debug, Default)]
pub struct PdfParser;

impl DocumentParser for PdfParser {
    fn name(&self) -> &'static str {
        "pdf-best-effort"
    }

    fn supports_extension(&self, extension: &str) -> bool {
        extension == "pdf"
    }

    fn parse(&self, input: ParserInput<'_>) -> ParsedDocument {
        // The PDF library still has panic paths for malformed font/page structures.
        // Keep these within the parser's failure boundary, never scrape raw bytes.
        match std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(input.bytes)) {
            Ok(Ok(text)) => {
                let status = if text.trim().is_empty() {
                    ParseStatus::MetadataOnly
                } else {
                    ParseStatus::Success
                };
                ParsedDocument::new(cap_chars(text.trim(), input.max_chars), status, self.name())
            }
            _ => ParsedDocument::failed(self.name()),
        }
    }
}

#[derive(Debug, Default)]
pub struct LegacyOfficeParser;

impl DocumentParser for LegacyOfficeParser {
    fn name(&self) -> &'static str {
        "legacy-office-best-effort"
    }

    fn supports_extension(&self, extension: &str) -> bool {
        matches!(extension, "doc" | "ppt" | "pps" | "xls")
    }

    fn parse(&self, _input: ParserInput<'_>) -> ParsedDocument {
        ParsedDocument::new(String::new(), ParseStatus::Unsupported, self.name())
    }
}

#[derive(Debug, Default)]
pub struct BinaryHwpParser;

impl DocumentParser for BinaryHwpParser {
    fn name(&self) -> &'static str {
        "hwp-best-effort"
    }

    fn supports_extension(&self, extension: &str) -> bool {
        extension == "hwp"
    }

    fn parse(&self, input: ParserInput<'_>) -> ParsedDocument {
        let text = hwp_text(input.bytes, input.max_chars);
        if text.trim().is_empty() {
            return ParsedDocument::failed(self.name());
        }
        ParsedDocument::new(text, ParseStatus::Success, self.name())
    }
}

fn hwp_text(bytes: &[u8], max_chars: usize) -> String {
    parse_hwp(bytes, max_chars).unwrap_or_default()
}

fn parse_hwp(bytes: &[u8], max_chars: usize) -> std::io::Result<String> {
    use std::io::{Error, ErrorKind};
    let invalid = || {
        Error::new(
            ErrorKind::InvalidData,
            "invalid or unsupported HWP5 document",
        )
    };
    let mut compound = CompoundFile::open(Cursor::new(bytes))?;
    let mut header = [0u8; 40];
    compound
        .open_stream("/FileHeader")?
        .read_exact(&mut header)?;
    if !header.starts_with(b"HWP Document File\0") || header[35] != 5 {
        return Err(invalid());
    }
    let flags = u32::from_le_bytes(header[36..40].try_into().unwrap());
    if flags & 6 != 0 {
        return Err(invalid());
    }
    let mut sections = compound
        .walk()
        .filter_map(|entry| {
            let path = entry.path();
            if !entry.is_stream() || path.parent() != Some(std::path::Path::new("/BodyText")) {
                return None;
            }
            let number = path
                .file_name()?
                .to_str()?
                .strip_prefix("Section")?
                .parse::<u32>()
                .ok()?;
            Some((number, path.to_path_buf()))
        })
        .collect::<Vec<_>>();
    sections.sort_by_key(|(number, _)| *number);
    if sections.is_empty() {
        return Err(invalid());
    }
    let mut text = String::new();
    let mut remaining = max_chars;
    let mut budget = 64 * 1024 * 1024usize;
    for (_, path) in sections {
        if remaining == 0 {
            break;
        }
        let stream = compound.open_stream(path)?;
        let reader: Box<dyn Read + '_> = if flags & 1 != 0 {
            Box::new(DeflateDecoder::new(stream))
        } else {
            Box::new(stream)
        };
        let mut data = Vec::new();
        reader.take(budget as u64 + 1).read_to_end(&mut data)?;
        if data.len() > budget {
            return Err(invalid());
        }
        budget -= data.len();
        let mut offset = 0usize;
        while offset < data.len() && remaining > 0 {
            let record = data.get(offset..offset + 4).ok_or_else(invalid)?;
            let header = u32::from_le_bytes(record.try_into().unwrap());
            offset += 4;
            let mut size = (header >> 20) as usize;
            if size == 0xfff {
                size = u32::from_le_bytes(
                    data.get(offset..offset + 4)
                        .ok_or_else(invalid)?
                        .try_into()
                        .unwrap(),
                ) as usize;
                offset += 4;
            }
            let end = offset.checked_add(size).ok_or_else(invalid)?;
            let payload = data.get(offset..end).ok_or_else(invalid)?;
            offset = end;
            if header & 0x3ff != 67 {
                continue;
            }
            if payload.len() % 2 != 0 {
                return Err(invalid());
            }
            let units = payload
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>();
            let mut plain = Vec::new();
            let mut i = 0;
            while i < units.len() {
                let u = units[i];
                if matches!(u, 1..=9 | 11..=12 | 14..=23) {
                    if i + 8 > units.len() {
                        return Err(invalid());
                    }
                    if u == 9 {
                        plain.push(9);
                    }
                    i += 8;
                } else {
                    if u >= 32 || matches!(u, 10 | 13) {
                        plain.push(if u == 13 { 10 } else { u });
                    }
                    i += 1;
                }
            }
            let decoded = String::from_utf16(&plain).map_err(|_| invalid())?;
            for ch in decoded.chars().take(remaining) {
                text.push(ch);
                remaining -= 1;
            }
            if remaining > 0 && !text.ends_with('\n') {
                text.push('\n');
                remaining -= 1;
            }
        }
    }
    Ok(text)
}
