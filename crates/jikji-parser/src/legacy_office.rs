//! Text extraction for legacy binary Office files (Word 97-2003 `.doc`, PowerPoint 97-2003
//! `.ppt`/`.pps`), using only the compound-file reader already in this crate (`cfb`, MIT).
//!
//! Word: the text lives in the `WordDocument` stream, located through the piece table (CLX) in
//! the `0Table`/`1Table` stream ([MS-DOC] 2.8.35, 2.9.38). PowerPoint: slide text is stored in
//! `TextCharsAtom` (UTF-16) and `TextBytesAtom` (Latin-1) records of the `PowerPoint Document`
//! stream ([MS-PPT] 2.9.2.1, 2.9.2.2). Both readers bound every offset and length so malformed
//! files fail with an error instead of panicking or allocating without limit.

use std::io::{Cursor, Error, ErrorKind, Read};

use cfb::CompoundFile;

/// Largest stream read from a legacy Office file.
const MAX_STREAM_BYTES: u64 = 64 * 1024 * 1024;

fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

fn read_stream(compound: &mut CompoundFile<Cursor<&[u8]>>, path: &str) -> std::io::Result<Vec<u8>> {
    let stream = compound.open_stream(path)?;
    let mut data = Vec::new();
    stream.take(MAX_STREAM_BYTES + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_STREAM_BYTES {
        return Err(invalid("stream is too large"));
    }
    Ok(data)
}

fn u16_at(data: &[u8], offset: usize) -> std::io::Result<u16> {
    data.get(offset..offset + 2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .ok_or_else(|| invalid("truncated structure"))
}

fn u32_at(data: &[u8], offset: usize) -> std::io::Result<u32> {
    data.get(offset..offset + 4)
        .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .ok_or_else(|| invalid("truncated structure"))
}

/// Plain text of a Word 97-2003 document, at most `max_chars` characters.
pub(crate) fn word_text(bytes: &[u8], max_chars: usize) -> std::io::Result<String> {
    let mut compound = CompoundFile::open(Cursor::new(bytes))?;
    let word = read_stream(&mut compound, "/WordDocument")?;
    if u16_at(&word, 0)? != 0xA5EC {
        return Err(invalid("not a Word 97 document"));
    }
    let flags = u16_at(&word, 0x0A)?;
    if flags & 0x0100 != 0 {
        return Err(invalid("encrypted document"));
    }
    let table_name = if flags & 0x0200 != 0 {
        "/1Table"
    } else {
        "/0Table"
    };
    let table = read_stream(&mut compound, table_name)?;

    // FibRgFcLcb97 follows FibBase (32 bytes), csw + FibRgW (2 + 28) and cslw + FibRgLw (2 + 88).
    let fc_lcb_offset = 32 + 2 + 28 + 2 + 88 + 2;
    let fc_clx = u32_at(&word, fc_lcb_offset + 33 * 8)? as usize;
    let lcb_clx = u32_at(&word, fc_lcb_offset + 33 * 8 + 4)? as usize;
    let clx = table
        .get(
            fc_clx
                ..fc_clx
                    .checked_add(lcb_clx)
                    .ok_or_else(|| invalid("bad CLX"))?,
        )
        .ok_or_else(|| invalid("CLX outside table stream"))?;

    // Skip Prc entries (0x01) until the Pcdt (0x02) holding the piece table.
    let mut offset = 0;
    while clx.get(offset) == Some(&0x01) {
        let size = u16_at(clx, offset + 1)? as usize;
        offset += 3 + size;
    }
    if clx.get(offset) != Some(&0x02) {
        return Err(invalid("piece table not found"));
    }
    let plc_len = u32_at(clx, offset + 1)? as usize;
    let plc = clx
        .get(offset + 5..offset + 5 + plc_len)
        .ok_or_else(|| invalid("truncated piece table"))?;
    // A PlcPcd holds n+1 character positions (4 bytes) and n piece descriptors (8 bytes).
    if plc_len < 4 || (plc_len - 4) % 12 != 0 {
        return Err(invalid("malformed piece table"));
    }
    let pieces = (plc_len - 4) / 12;

    let mut text = String::new();
    let mut remaining = max_chars;
    for index in 0..pieces {
        if remaining == 0 {
            break;
        }
        let cp_start = u32_at(plc, index * 4)? as usize;
        let cp_end = u32_at(plc, (index + 1) * 4)? as usize;
        let chars = cp_end
            .checked_sub(cp_start)
            .ok_or_else(|| invalid("bad piece bounds"))?;
        let fc_raw = u32_at(plc, (pieces + 1) * 4 + index * 8 + 2)?;
        let compressed = fc_raw & 0x4000_0000 != 0;
        let fc = (fc_raw & 0x3FFF_FFFF) as usize;
        let take = chars.min(remaining);
        if compressed {
            // 8-bit text stored at fc / 2 in Windows-1252.
            let start = fc / 2;
            let raw = word
                .get(start..start + take)
                .ok_or_else(|| invalid("piece outside stream"))?;
            let (decoded, _, _) = encoding_rs::WINDOWS_1252.decode(raw);
            push_word_chars(&mut text, decoded.chars(), &mut remaining);
        } else {
            let raw = word
                .get(fc..fc + take * 2)
                .ok_or_else(|| invalid("piece outside stream"))?;
            let units = raw
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]));
            let decoded = char::decode_utf16(units).map(|unit| unit.unwrap_or('\u{fffd}'));
            push_word_chars(&mut text, decoded, &mut remaining);
        }
    }
    Ok(text)
}

/// Appends Word characters, turning paragraph/cell marks into line breaks and dropping field
/// codes and other control characters.
fn push_word_chars(text: &mut String, chars: impl Iterator<Item = char>, remaining: &mut usize) {
    for ch in chars {
        if *remaining == 0 {
            return;
        }
        let mapped = match ch {
            '\r' | '\u{0b}' | '\u{0c}' | '\u{07}' => '\n',
            '\t' => '\t',
            ch if ch.is_control() => continue,
            ch => ch,
        };
        if mapped == '\n' && text.ends_with("\n\n") {
            continue;
        }
        text.push(mapped);
        *remaining -= 1;
    }
}

/// Slide text of a PowerPoint 97-2003 presentation, at most `max_chars` characters.
pub(crate) fn powerpoint_text(bytes: &[u8], max_chars: usize) -> std::io::Result<String> {
    const TEXT_CHARS_ATOM: u16 = 0x0FA0;
    const TEXT_BYTES_ATOM: u16 = 0x0FA8;
    let mut compound = CompoundFile::open(Cursor::new(bytes))?;
    let document = read_stream(&mut compound, "/PowerPoint Document")?;
    let mut text = String::new();
    let mut remaining = max_chars;
    let mut offset = 0usize;
    while offset + 8 <= document.len() && remaining > 0 {
        let version_instance = u16_at(&document, offset)?;
        let record_type = u16_at(&document, offset + 2)?;
        let length = u32_at(&document, offset + 4)? as usize;
        offset += 8;
        // Containers (version 0xF) hold child records: step into them.
        if version_instance & 0x000F == 0x000F {
            continue;
        }
        let end = offset
            .checked_add(length)
            .ok_or_else(|| invalid("bad record length"))?;
        let Some(payload) = document.get(offset..end) else {
            break;
        };
        offset = end;
        let decoded: String = match record_type {
            TEXT_CHARS_ATOM => {
                let units = payload
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]));
                char::decode_utf16(units)
                    .map(|unit| unit.unwrap_or('\u{fffd}'))
                    .collect()
            }
            TEXT_BYTES_ATOM => encoding_rs::WINDOWS_1252.decode(payload).0.into_owned(),
            _ => continue,
        };
        for ch in decoded.chars() {
            if remaining == 0 {
                break;
            }
            let mapped = match ch {
                '\r' | '\u{0b}' => '\n',
                ch if ch.is_control() && ch != '\t' => continue,
                ch => ch,
            };
            text.push(mapped);
            remaining -= 1;
        }
        if remaining > 0 && !text.ends_with('\n') {
            text.push('\n');
            remaining -= 1;
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::{powerpoint_text, push_word_chars, word_text};

    #[test]
    fn word_marks_become_line_breaks_and_controls_are_dropped() {
        let mut text = String::new();
        let mut remaining = 100;
        push_word_chars(
            &mut text,
            "제목\r본문\u{07}셀\u{13}FIELD\u{15}\r\r\r끝".chars(),
            &mut remaining,
        );
        assert_eq!(text, "제목\n본문\n셀FIELD\n\n끝");
    }

    #[test]
    fn non_compound_and_garbage_input_fail_without_panicking() {
        assert!(word_text(b"not an office file", 100).is_err());
        assert!(powerpoint_text(&[0xD0, 0xCF, 0x11, 0xE0, 0, 0], 100).is_err());
        assert!(word_text(&[], 100).is_err());
    }
}
