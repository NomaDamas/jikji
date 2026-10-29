use std::io::{Cursor, Read};

use zip::ZipArchive;

use crate::utils::{cap_chars, decode_xml_strict, strip_xml_tags};
use crate::{DocumentParser, ParseStatus, ParsedDocument, ParserInput};

#[derive(Debug, Default)]
pub struct EpubParser;

impl DocumentParser for EpubParser {
    fn name(&self) -> &'static str {
        "epub"
    }

    fn supports_extension(&self, extension: &str) -> bool {
        extension == "epub"
    }

    fn parse(&self, input: ParserInput<'_>) -> ParsedDocument {
        let Some(text) = zip_text(
            input.bytes,
            &|name| matches!(suffix(name).as_str(), "xhtml" | "html" | "htm" | "xml"),
            input.max_chars,
        ) else {
            return ParsedDocument::failed(self.name());
        };
        ParsedDocument::new(text, ParseStatus::Success, self.name())
    }
}

#[derive(Debug, Default)]
pub struct OfficeOpenXmlParser;

impl DocumentParser for OfficeOpenXmlParser {
    fn name(&self) -> &'static str {
        "office-openxml"
    }

    fn supports_extension(&self, extension: &str) -> bool {
        matches!(extension, "docx" | "pptx" | "ppsx" | "xlsx")
    }

    fn parse(&self, input: ParserInput<'_>) -> ParsedDocument {
        let ext = suffix(input.path.to_string_lossy().as_ref());
        let structured = match ext.as_str() {
            "docx" => Some(docx_text(input.bytes, input.max_chars)),
            "pptx" | "ppsx" => Some(pptx_text(input.bytes, input.max_chars)),
            _ => None,
        };
        if let Some(text) = structured {
            let Some(text) = text else {
                return ParsedDocument::failed(self.name());
            };
            return ParsedDocument::new(text, ParseStatus::Success, self.name());
        }
        let matcher = |name: &str| {
            let lower = name.to_ascii_lowercase();
            (lower.starts_with("word/") && lower.ends_with(".xml"))
                || (lower.starts_with("ppt/slides/") && lower.ends_with(".xml"))
                || lower.ends_with("sharedstrings.xml")
                || (lower.starts_with("xl/worksheets/") && lower.ends_with(".xml"))
        };
        let Some(text) = zip_text(input.bytes, &matcher, input.max_chars) else {
            return ParsedDocument::failed(self.name());
        };
        ParsedDocument::new(text, ParseStatus::Success, self.name())
    }
}

#[derive(Debug, Default)]
pub struct OpenDocumentParser;

impl DocumentParser for OpenDocumentParser {
    fn name(&self) -> &'static str {
        "opendocument"
    }

    fn supports_extension(&self, extension: &str) -> bool {
        matches!(extension, "odt" | "ods" | "odp")
    }

    fn parse(&self, input: ParserInput<'_>) -> ParsedDocument {
        let Some(text) = zip_text(input.bytes, &|name| name == "content.xml", input.max_chars)
        else {
            return ParsedDocument::failed(self.name());
        };
        ParsedDocument::new(text, ParseStatus::Success, self.name())
    }
}

#[derive(Debug, Default)]
pub struct HwpxParser;

impl DocumentParser for HwpxParser {
    fn name(&self) -> &'static str {
        "hwpx"
    }

    fn supports_extension(&self, extension: &str) -> bool {
        extension == "hwpx"
    }

    fn parse(&self, input: ParserInput<'_>) -> ParsedDocument {
        let matcher = |name: &str| {
            let lower = name.to_ascii_lowercase();
            lower.starts_with("contents/section") && lower.ends_with(".xml")
                || lower.contains("section") && lower.ends_with(".xml")
        };
        let Some(text) = zip_text(input.bytes, &matcher, input.max_chars) else {
            return ParsedDocument::failed(self.name());
        };
        ParsedDocument::new(text, ParseStatus::Success, self.name())
    }
}

// Bound decompression independently of the compressed file and requested text length.
const MAX_XML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ZIP_ENTRIES: usize = 10_000;

fn docx_text(bytes: &[u8], max_chars: usize) -> Option<String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).ok()?;
    if archive.len() > MAX_ZIP_ENTRIES {
        return None;
    }
    let file = archive.by_name("word/document.xml").ok()?;
    if file.size() > MAX_XML_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_XML_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_XML_BYTES {
        return None;
    }
    word_xml_text(&decode_xml_strict(&bytes)?, "w", max_chars)
}

/// Slide text in slide order (`ppt/slides/slideN.xml` sorted by N, not archive order), one line
/// per paragraph and a blank line between slides. Layouts, masters and notes are not slides.
fn pptx_text(bytes: &[u8], max_chars: usize) -> Option<String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).ok()?;
    if archive.len() > MAX_ZIP_ENTRIES {
        return None;
    }
    let mut slides: Vec<(u32, String)> = archive
        .file_names()
        .filter_map(|name| {
            let number = name
                .strip_prefix("ppt/slides/slide")?
                .strip_suffix(".xml")?;
            Some((number.parse().ok()?, name.to_owned()))
        })
        .collect();
    slides.sort_unstable();
    let mut remaining_bytes = MAX_XML_BYTES;
    let mut text = String::new();
    for (_, name) in slides {
        let file = archive.by_name(&name).ok()?;
        if file.size() > remaining_bytes {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(remaining_bytes + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        remaining_bytes = remaining_bytes.checked_sub(bytes.len() as u64)?;
        let slide = word_xml_text(&decode_xml_strict(&bytes)?, "a", max_chars)?;
        if slide.trim().is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(slide.trim());
        if text.chars().count() >= max_chars {
            break;
        }
    }
    Some(cap_chars(&text, max_chars))
}

/// Text of a WordprocessingML (`w`) or DrawingML (`a`) part: only `t` elements, runs joined
/// within a paragraph, a line break per paragraph or table row.
fn word_xml_text(xml: &str, ns: &str, max_chars: usize) -> Option<String> {
    let mut rest = xml;
    let mut text = String::new();
    let mut stack: Vec<&str> = Vec::new();
    let mut skipped = 0usize;
    while !rest.is_empty() {
        if let Some(comment) = rest.strip_prefix("<!--") {
            rest = comment.split_once("-->")?.1;
            continue;
        }
        if let Some(cdata) = rest.strip_prefix("<![CDATA[") {
            let (body, tail) = cdata.split_once("]]>")?;
            if skipped == 0
                && stack
                    .last()
                    .is_some_and(|name| is_word_element(name, ns, "t"))
            {
                text.push_str(body);
            }
            rest = tail;
            continue;
        }
        if let Some(tag) = rest.strip_prefix('<') {
            let mut quote = None;
            let end = tag.char_indices().find_map(|(index, ch)| {
                if let Some(q) = quote {
                    if ch == q {
                        quote = None;
                    }
                } else if matches!(ch, '\'' | '"') {
                    quote = Some(ch);
                } else if ch == '>' {
                    return Some(index);
                }
                None
            })?;
            let tag_body = tag[..end].trim();
            rest = &tag[end + 1..];
            if tag_body.starts_with('?') {
                continue;
            }
            // No DTD or external entity expansion on the request path.
            if tag_body.starts_with('!') {
                return None;
            }
            let closing = tag_body.starts_with('/');
            let empty = tag_body.ends_with('/');
            let name = tag_body
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()?;
            let ignore = is_word_element(name, ns, "del")
                || is_word_element(name, ns, "delText")
                || is_word_element(name, ns, "instrText");
            if closing {
                if stack.pop()? != name {
                    return None;
                }
                if ignore {
                    skipped = skipped.checked_sub(1)?;
                }
                if skipped == 0
                    && (is_word_element(name, ns, "p") || is_word_element(name, ns, "tr"))
                    && !text.ends_with('\n')
                {
                    text.push('\n');
                }
            } else {
                if skipped == 0 {
                    if is_word_element(name, ns, "tab") {
                        text.push('\t');
                    } else if is_word_element(name, ns, "br") || is_word_element(name, ns, "cr") {
                        text.push('\n');
                    }
                }
                if !empty {
                    if stack.len() >= 256 {
                        return None;
                    }
                    stack.push(name);
                    if ignore {
                        skipped += 1;
                    }
                }
            }
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            let body = &rest[..end];
            if skipped == 0
                && stack
                    .last()
                    .is_some_and(|name| is_word_element(name, ns, "t"))
            {
                let mut remaining = body;
                while let Some((before, entity)) = remaining.split_once('&') {
                    text.push_str(before);
                    let (entity, tail) = entity.split_once(';')?;
                    let ch = match entity {
                        "amp" => '&',
                        "lt" => '<',
                        "gt" => '>',
                        "quot" => '"',
                        "apos" => '\'',
                        _ => {
                            let value = if let Some(hex) = entity.strip_prefix("#x") {
                                u32::from_str_radix(hex, 16).ok()?
                            } else {
                                entity.strip_prefix('#')?.parse().ok()?
                            };
                            char::from_u32(value)?
                        }
                    };
                    if ch.is_control() && !matches!(ch, '\n' | '\r' | '\t') {
                        return None;
                    }
                    text.push(ch);
                    remaining = tail;
                }
                text.push_str(remaining);
            }
            rest = &rest[end..];
        }
    }
    if !stack.is_empty() {
        return None;
    }
    Some(cap_chars(text.trim_end_matches('\n'), max_chars))
}

fn is_word_element(name: &str, ns: &str, local: &str) -> bool {
    name == local
        || name
            .strip_prefix(ns)
            .and_then(|rest| rest.strip_prefix(':'))
            == Some(local)
}

fn zip_text(bytes: &[u8], predicate: &dyn Fn(&str) -> bool, max_chars: usize) -> Option<String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).ok()?;
    if archive.len() > MAX_ZIP_ENTRIES {
        return None;
    }
    let mut remaining_bytes = MAX_XML_BYTES;
    let mut parts = Vec::new();
    let mut total = 0usize;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).ok()?;
        let name = file.name().to_owned();
        if !predicate(name.as_str()) {
            continue;
        }
        if file.size() > remaining_bytes {
            return None;
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(remaining_bytes + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        remaining_bytes = remaining_bytes.checked_sub(bytes.len() as u64)?;
        let xml = decode_xml_strict(bytes.as_slice())?;
        let xml = strip_xml_tags(xml.as_str(), max_chars);
        if !xml.trim().is_empty() {
            total = total.saturating_add(xml.len());
            parts.push(xml);
            if total >= max_chars {
                break;
            }
        }
    }
    Some(cap_chars(parts.join("\n").as_str(), max_chars))
}

fn suffix(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, suffix)| suffix.to_ascii_lowercase())
        .unwrap_or_default()
}
