use regex::Regex;

use crate::utils::{cap_chars, collapse_whitespace, decode_text_strict};
use crate::{DocumentParser, ParseStatus, ParsedDocument, ParserInput};

const PLAIN_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "csv", "tsv", "log", "json", "jsonl", "yaml", "yml", "xml", "ini",
    "cfg", "toml",
];

#[derive(Debug, Default)]
pub struct PlainTextParser;

impl DocumentParser for PlainTextParser {
    fn name(&self) -> &'static str {
        "plain-text"
    }

    fn supports_extension(&self, extension: &str) -> bool {
        PLAIN_EXTENSIONS.contains(&extension)
    }

    fn parse(&self, input: ParserInput<'_>) -> ParsedDocument {
        let Some(text) = decode_text_strict(input.bytes, input.max_chars) else {
            return ParsedDocument::failed(self.name());
        };
        ParsedDocument::new(text, ParseStatus::Success, self.name())
    }
}

#[derive(Debug, Default)]
pub struct SubtitleParser;

impl DocumentParser for SubtitleParser {
    fn name(&self) -> &'static str {
        "subtitles"
    }

    fn supports_extension(&self, extension: &str) -> bool {
        matches!(extension, "srt" | "vtt")
    }

    fn parse(&self, input: ParserInput<'_>) -> ParsedDocument {
        let Some(raw) = decode_text_strict(input.bytes, input.max_chars.saturating_mul(4)) else {
            return ParsedDocument::failed(self.name());
        };
        let timing = Regex::new(
            r"^\d{1,2}:\d{2}:\d{2}[.,]\d{1,3}\s*-->\s*\d{1,2}:\d{2}:\d{2}[.,]\d{1,3}.*$",
        );
        let mut parts = Vec::new();
        let mut previous_blank = true;
        for line in raw.lines() {
            let stripped = line.trim();
            if stripped.is_empty() {
                previous_blank = true;
                continue;
            }
            if stripped == "WEBVTT"
                || stripped.starts_with("NOTE")
                || stripped.starts_with("STYLE")
                || stripped.starts_with("REGION")
            {
                previous_blank = false;
                continue;
            }
            if timing.as_ref().is_ok_and(|regex| regex.is_match(stripped)) {
                previous_blank = false;
                continue;
            }
            if previous_blank && stripped.chars().all(|ch| ch.is_ascii_digit()) {
                previous_blank = false;
                continue;
            }
            previous_blank = false;
            parts.push(stripped.to_owned());
        }
        ParsedDocument::new(parts.join("\n"), ParseStatus::Success, self.name())
    }
}

#[derive(Debug, Default)]
pub struct HtmlParser;

impl DocumentParser for HtmlParser {
    fn name(&self) -> &'static str {
        "html"
    }
    fn supports_extension(&self, extension: &str) -> bool {
        matches!(extension, "html" | "htm")
    }
    fn parse(&self, input: ParserInput<'_>) -> ParsedDocument {
        let Some(raw) = decode_text_strict(input.bytes, input.max_chars.saturating_mul(6)) else {
            return ParsedDocument::failed(self.name());
        };
        ParsedDocument::new(
            strip_html(raw.as_str(), input.max_chars),
            ParseStatus::Success,
            self.name(),
        )
    }
}

#[derive(Debug, Default)]
pub struct RtfParser;

impl DocumentParser for RtfParser {
    fn name(&self) -> &'static str {
        "rtf"
    }
    fn supports_extension(&self, extension: &str) -> bool {
        extension == "rtf"
    }
    fn parse(&self, input: ParserInput<'_>) -> ParsedDocument {
        match crate::rtf::rtf_text(input.bytes, input.max_chars) {
            Some(text) if !text.trim().is_empty() => {
                ParsedDocument::new(text, ParseStatus::Success, self.name())
            }
            _ => ParsedDocument::failed(self.name()),
        }
    }
}

fn strip_html(html: &str, max_chars: usize) -> String {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut in_tag = false;
    let mut tag = String::new();
    let mut skip_depth = 0usize;
    for ch in html.chars() {
        match ch {
            '<' => {
                if skip_depth == 0 && !text.trim().is_empty() {
                    parts.push(collapse_whitespace(text.as_str()));
                }
                text.clear();
                tag.clear();
                in_tag = true;
            }
            '>' if in_tag => {
                let tag_name = tag_name(tag.as_str());
                if matches!(tag_name.as_str(), "script" | "style" | "head") {
                    if tag.trim_start().starts_with('/') {
                        skip_depth = skip_depth.saturating_sub(1);
                    } else {
                        skip_depth = skip_depth.saturating_add(1);
                    }
                }
                in_tag = false;
            }
            _ if in_tag => tag.push(ch),
            _ if skip_depth == 0 => text.push(ch),
            _ => {}
        }
    }
    if skip_depth == 0 && !text.trim().is_empty() {
        parts.push(collapse_whitespace(text.as_str()));
    }
    cap_chars(parts.join("\n").as_str(), max_chars)
}

fn tag_name(tag: &str) -> String {
    tag.trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}
