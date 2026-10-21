use std::io::{Cursor, Write};

use encoding_rs::EUC_KR;
use jikji_parser::{ParseStatus, ParserRegistry};
use zip::write::SimpleFileOptions;

fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little {
        vec![0xff, 0xfe]
    } else {
        vec![0xfe, 0xff]
    };
    for word in text.encode_utf16() {
        bytes.extend_from_slice(&if little {
            word.to_le_bytes()
        } else {
            word.to_be_bytes()
        });
    }
    bytes
}

fn zip_xml(name: &str, xml: &[u8]) -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    archive
        .start_file(name, SimpleFileOptions::default())
        .unwrap();
    archive.write_all(xml).unwrap();
    archive.finish().unwrap().into_inner()
}

#[test]
fn korean_encodings_preserve_text_and_character_limits() {
    let registry = ParserRegistry::with_defaults();
    let text = "한글 문서 내용을 보존합니다";
    let (euc, _, errors) = EUC_KR.encode(text);
    assert!(!errors);
    for bytes in [
        text.as_bytes().to_vec(),
        utf16(text, true),
        utf16(text, false),
        euc.into_owned(),
    ] {
        for limit in [0, 1, 2, 7, 100] {
            let result = registry.parse_bytes("text.txt", &bytes, limit);
            assert_eq!(result.status, ParseStatus::Success);
            assert_eq!(result.text, text.chars().take(limit).collect::<String>());
        }
    }
}

#[test]
fn utf8_byte_boundary_does_not_trigger_legacy_fallback() {
    let text = "가나다라마바사아자차카타파하";
    let result = ParserRegistry::with_defaults().parse_bytes("text.txt", text.as_bytes(), 1);
    assert_eq!(result.status, ParseStatus::Success);
    assert_eq!(result.text, "가");
}

#[test]
fn invalid_and_null_binary_fail_in_text_parsers() {
    let registry = ParserRegistry::with_defaults();
    for extension in ["txt", "md", "html", "rtf", "srt", "vtt"] {
        for bytes in [
            vec![0xff, 0xfd, 0x80],
            b"printable\0binary".to_vec(),
            vec![0xff, 0xfe, 0x00, 0xd8],
            vec![0xff, 0xfe, 0x41],
        ] {
            let result = registry.parse_bytes(&format!("text.{extension}"), &bytes, 100);
            assert_eq!(result.status, ParseStatus::Failed, "{extension}: {bytes:?}");
            assert!(result.text.is_empty());
        }
    }
}

#[test]
fn empty_and_non_latin_text_are_preserved() {
    let registry = ParserRegistry::with_defaults();
    for text in ["", "   \n\t", "العربية עברית 日本語 中文 Ελληνικά हिन्दी 😀"]
    {
        let result = registry.parse_bytes("text.txt", text.as_bytes(), 100);
        assert_eq!(result.status, ParseStatus::Success);
        assert_eq!(result.text, text);
    }
}

#[test]
fn docx_and_hwpx_use_xml_encoding_and_fail_on_wrong_bytes() {
    let registry = ParserRegistry::with_defaults();
    for (extension, member) in [
        ("docx", "word/document.xml"),
        ("hwpx", "Contents/section0.xml"),
    ] {
        let name = format!("document.{extension}");
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><p>한글 본문</p>";
        let valid = zip_xml(member, xml.as_bytes());
        let result = registry.parse_bytes(&name, &valid, 100);
        assert_eq!(result.status, ParseStatus::Success);
        assert_eq!(result.text, "한글 본문");
        let utf16_xml = xml.replace("UTF-8", "UTF-16");
        for little in [true, false] {
            let result =
                registry.parse_bytes(&name, &zip_xml(member, &utf16(&utf16_xml, little)), 100);
            assert_eq!(result.status, ParseStatus::Success);
            assert_eq!(result.text, "한글 본문");
        }
        let euc_xml = xml.replace("UTF-8", "EUC-KR");
        let (encoded, _, errors) = EUC_KR.encode(&euc_xml);
        assert!(!errors);
        let result = registry.parse_bytes(&name, &zip_xml(member, &encoded), 100);
        assert_eq!(result.status, ParseStatus::Success);
        assert_eq!(result.text, "한글 본문");
        let (wrong, _, _) = EUC_KR.encode(xml);
        for invalid in [
            wrong.into_owned(),
            utf16(xml, true),
            b"<p>\0binary</p>".to_vec(),
            b"<?xml version=\"1.0\" encoding=\"unknown\"?><p>text</p>".to_vec(),
        ] {
            let result = registry.parse_bytes(&name, &zip_xml(member, &invalid), 100);
            assert_eq!(result.status, ParseStatus::Failed);
            assert!(result.text.is_empty());
        }
        let result = registry.parse_bytes(&name, b"not a zip", 100);
        assert_eq!(result.status, ParseStatus::Failed);
    }
}
