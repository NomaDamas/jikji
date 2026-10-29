//! Plain text from RTF ([RTF 1.9.1 spec]). Skips destination groups (font table, styles,
//! pictures, `\*` extensions), decodes `\'hh` bytes with the document code page (`\ansicpg`,
//! e.g. 949 for Korean) and `\uN` Unicode escapes honouring `\ucN` fallback counts.

const SKIPPED_DESTINATIONS: &[&str] = &[
    "fonttbl",
    "colortbl",
    "stylesheet",
    "listtable",
    "listoverridetable",
    "info",
    "pict",
    "object",
    "objdata",
    "themedata",
    "colorschememapping",
    "datastore",
    "latentstyles",
    "rsidtbl",
    "generator",
    "xmlnstbl",
    "mmathPr",
    "header",
    "footer",
    "headerl",
    "headerr",
    "footerl",
    "footerr",
    "headerf",
    "footerf",
    "fldinst",
    "filetbl",
    "revtbl",
    "pgdsctbl",
];

fn encoding_for_codepage(codepage: u32) -> &'static encoding_rs::Encoding {
    match codepage {
        949 => encoding_rs::EUC_KR,
        932 => encoding_rs::SHIFT_JIS,
        936 => encoding_rs::GBK,
        950 => encoding_rs::BIG5,
        1250 => encoding_rs::WINDOWS_1250,
        1251 => encoding_rs::WINDOWS_1251,
        1253 => encoding_rs::WINDOWS_1253,
        1254 => encoding_rs::WINDOWS_1254,
        1255 => encoding_rs::WINDOWS_1255,
        1256 => encoding_rs::WINDOWS_1256,
        65001 => encoding_rs::UTF_8,
        _ => encoding_rs::WINDOWS_1252,
    }
}

#[derive(Clone, Copy)]
struct Group {
    skip: bool,
    uc: usize,
}

/// Text of an RTF document, at most `max_chars` characters. `None` when it is not RTF.
pub(crate) fn rtf_text(bytes: &[u8], max_chars: usize) -> Option<String> {
    if !bytes.starts_with(b"{\\rtf") {
        return None;
    }
    let mut encoding = encoding_rs::WINDOWS_1252;
    let mut out = String::new();
    let mut pending: Vec<u8> = Vec::new();
    let mut stack: Vec<Group> = Vec::new();
    let mut current = Group { skip: false, uc: 1 };
    let mut skip_fallback = 0usize;
    let mut chars = 0usize;
    let mut i = 0usize;

    let flush = |pending: &mut Vec<u8>,
                 out: &mut String,
                 encoding: &'static encoding_rs::Encoding,
                 chars: &mut usize| {
        if pending.is_empty() {
            return;
        }
        let (decoded, _, _) = encoding.decode(pending);
        for ch in decoded.chars() {
            if *chars >= max_chars {
                break;
            }
            out.push(ch);
            *chars += 1;
        }
        pending.clear();
    };

    while i < bytes.len() && chars < max_chars {
        let byte = bytes[i];
        match byte {
            b'{' => {
                stack.push(current);
                i += 1;
                // `{\*\dest ...}` marks an ignorable destination.
                if bytes.get(i..i + 2) == Some(b"\\*") {
                    current.skip = true;
                }
            }
            b'}' => {
                if !current.skip {
                    flush(&mut pending, &mut out, encoding, &mut chars);
                }
                current = stack.pop().unwrap_or(Group { skip: false, uc: 1 });
                i += 1;
            }
            b'\\' => {
                i += 1;
                let Some(&next) = bytes.get(i) else { break };
                if next == b'\'' {
                    let hex = bytes
                        .get(i + 1..i + 3)
                        .and_then(|pair| std::str::from_utf8(pair).ok());
                    i += 3;
                    if skip_fallback > 0 {
                        skip_fallback -= 1;
                        continue;
                    }
                    if let Some(value) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                        if !current.skip {
                            pending.push(value);
                        }
                    }
                    continue;
                }
                if !next.is_ascii_alphabetic() {
                    // Control symbols: escaped braces/backslash, non-breaking space, etc.
                    i += 1;
                    if current.skip {
                        continue;
                    }
                    let symbol = match next {
                        b'\\' | b'{' | b'}' => Some(next as char),
                        b'~' => Some(' '),
                        b'\n' | b'\r' => Some('\n'),
                        _ => None,
                    };
                    if let Some(symbol) = symbol {
                        flush(&mut pending, &mut out, encoding, &mut chars);
                        if chars < max_chars {
                            out.push(symbol);
                            chars += 1;
                        }
                    }
                    continue;
                }
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let word = std::str::from_utf8(&bytes[start..i]).unwrap_or_default();
                let num_start = i;
                if bytes.get(i) == Some(&b'-') {
                    i += 1;
                }
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let param = std::str::from_utf8(&bytes[num_start..i])
                    .ok()
                    .and_then(|value| value.parse::<i64>().ok());
                if bytes.get(i) == Some(&b' ') {
                    i += 1;
                }
                if SKIPPED_DESTINATIONS.contains(&word) {
                    current.skip = true;
                    continue;
                }
                match word {
                    "ansicpg" => {
                        if let Some(page) = param.and_then(|value| u32::try_from(value).ok()) {
                            encoding = encoding_for_codepage(page);
                        }
                    }
                    "uc" => {
                        current.uc = param
                            .and_then(|value| usize::try_from(value).ok())
                            .unwrap_or(1)
                    }
                    "u" => {
                        if let Some(value) = param {
                            let code = if value < 0 { value + 65_536 } else { value };
                            if !current.skip {
                                flush(&mut pending, &mut out, encoding, &mut chars);
                                if let Some(ch) = u32::try_from(code).ok().and_then(char::from_u32)
                                {
                                    if chars < max_chars {
                                        out.push(ch);
                                        chars += 1;
                                    }
                                }
                            }
                            skip_fallback = current.uc;
                        }
                    }
                    "par" | "line" | "sect" | "page" | "row" if !current.skip => {
                        flush(&mut pending, &mut out, encoding, &mut chars);
                        if chars < max_chars && !out.ends_with("\n\n") {
                            out.push('\n');
                            chars += 1;
                        }
                    }
                    "tab" | "cell" if !current.skip => {
                        flush(&mut pending, &mut out, encoding, &mut chars);
                        if chars < max_chars {
                            out.push(if word == "tab" { '\t' } else { ' ' });
                            chars += 1;
                        }
                    }
                    _ => {}
                }
            }
            b'\r' | b'\n' => i += 1,
            _ => {
                i += 1;
                if skip_fallback > 0 {
                    skip_fallback -= 1;
                    continue;
                }
                if !current.skip {
                    pending.push(byte);
                }
            }
        }
    }
    flush(&mut pending, &mut out, encoding, &mut chars);
    Some(out.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::rtf_text;

    #[test]
    fn korean_codepage_text_is_decoded_and_tables_are_skipped() {
        let rtf = br"{\rtf1\ansi\ansicpg949{\fonttbl{\f0\fcharset129 \'b8\'bc\'c0\'ba \'b0\'ed\'b5\'f1;}}{\stylesheet{\s1 Heading 1;}}\pard \'c3\'aa\'ba\'bf \'b0\'b3\'b9\'df\par Second line\par}";
        assert_eq!(rtf_text(rtf, 100).unwrap(), "챗봇 개발\nSecond line");
    }

    #[test]
    fn unicode_escapes_skip_their_fallback_and_ignorable_groups_are_dropped() {
        let rtf = br"{\rtf1\ansi\uc1\u54620?\u44544?{\*\generator Riched20;} body \{x\}}";
        assert_eq!(rtf_text(rtf, 100).unwrap(), "한글 body {x}");
        assert_eq!(rtf_text(br"{\rtf1 a\par b}", 1).unwrap(), "a");
        assert!(rtf_text(b"not rtf", 10).is_none());
    }
}
