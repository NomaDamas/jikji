use encoding_rs::{EUC_KR, Encoding, UTF_8, UTF_16BE, UTF_16LE};
use regex::Regex;

pub fn decode_text(bytes: &[u8], max_chars: usize) -> String {
    decode_text_strict(bytes, max_chars).unwrap_or_default()
}

pub fn decode_text_strict(bytes: &[u8], max_chars: usize) -> Option<String> {
    let raw = bytes;
    let text = if raw.starts_with(&[0xff, 0xfe]) {
        decode_with_strict(UTF_16LE, &raw[2..])?
    } else if raw.starts_with(&[0xfe, 0xff]) {
        decode_with_strict(UTF_16BE, &raw[2..])?
    } else if raw.starts_with(&[0xef, 0xbb, 0xbf]) {
        std::str::from_utf8(&raw[3..]).ok()?.to_owned()
    } else if let Ok(text) = std::str::from_utf8(raw) {
        text.to_owned()
    } else {
        let (text, _, had_errors) = EUC_KR.decode(raw);
        if had_errors {
            return None;
        }
        text.into_owned()
    };
    if !is_text(text.as_str()) {
        return None;
    }
    Some(cap_chars(text.as_str(), max_chars))
}

pub fn decode_xml_strict(bytes: &[u8]) -> Option<String> {
    let (detected, skip) = if bytes.starts_with(&[0xff, 0xfe]) {
        (Some(UTF_16LE), 2)
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        (Some(UTF_16BE), 2)
    } else if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        (Some(UTF_8), 3)
    } else if bytes.starts_with(b"\0<\0?") {
        (Some(UTF_16BE), 0)
    } else if bytes.starts_with(b"<\0?\0") {
        (Some(UTF_16LE), 0)
    } else {
        (None, 0)
    };
    let prefix: String = bytes
        .iter()
        .take_while(|byte| byte.is_ascii())
        .map(|byte| char::from(*byte))
        .collect();
    let declared = xml_declared_encoding(prefix.as_str());
    let encoding = match (detected, declared) {
        (Some(encoding), _) => encoding,
        (None, Some(label)) => Encoding::for_label(label.as_bytes())?,
        (None, None) => UTF_8,
    };
    let text = decode_with_strict(encoding, &bytes[skip..])?;
    if let Some(label) = xml_declared_encoding(text.as_str()) {
        let declared = Encoding::for_label(label.as_bytes())?;
        let generic_utf16 =
            label.eq_ignore_ascii_case("utf-16") && (encoding == UTF_16LE || encoding == UTF_16BE);
        if declared != encoding && !generic_utf16 {
            return None;
        }
    }
    is_text(text.as_str()).then_some(text)
}

fn decode_with_strict(encoding: &'static encoding_rs::Encoding, bytes: &[u8]) -> Option<String> {
    if bytes.len() % 2 != 0 && (encoding == UTF_16LE || encoding == UTF_16BE) {
        return None;
    }
    let (text, had_errors) = encoding.decode_without_bom_handling(bytes);
    (!had_errors).then(|| text.into_owned())
}

fn is_text(text: &str) -> bool {
    !text.chars().any(|ch| {
        ch == '\u{fffd}' || (ch.is_control() && !matches!(ch, '\n' | '\r' | '\t' | '\u{c}'))
    })
}

fn xml_declared_encoding(text: &str) -> Option<&str> {
    let head = text.strip_prefix("<?xml")?.split_once("?>")?.0;
    Regex::new(r#"\bencoding\s*=\s*["']([^"']+)["']"#)
        .ok()?
        .captures(head)
        .and_then(|captures| captures.get(1).map(|match_| match_.as_str()))
}

pub fn cap_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

pub fn normalize_lines(parts: &[String], max_chars: usize) -> String {
    let text = parts
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    cap_chars(text.as_str(), max_chars)
}

pub fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out.trim().to_owned()
}

pub fn strip_xml_tags(xml: &str, max_chars: usize) -> String {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut in_tag = false;
    let mut in_entity = false;
    let mut entity = String::new();
    for ch in xml.chars() {
        match ch {
            '<' => {
                if !text.trim().is_empty() {
                    parts.push(collapse_whitespace(text.as_str()));
                }
                text.clear();
                in_tag = true;
                in_entity = false;
                entity.clear();
            }
            '>' => in_tag = false,
            '&' if !in_tag => {
                in_entity = true;
                entity.clear();
            }
            ';' if in_entity && !in_tag => {
                text.push_str(decode_entity(entity.as_str()));
                in_entity = false;
                entity.clear();
            }
            _ if in_entity && !in_tag => entity.push(ch),
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    if !text.trim().is_empty() {
        parts.push(collapse_whitespace(text.as_str()));
    }
    normalize_lines(&parts, max_chars)
}

fn decode_entity(entity: &str) -> &'static str {
    match entity {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        _ => " ",
    }
}
