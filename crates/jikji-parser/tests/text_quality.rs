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
        let xml = if extension == "docx" {
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document><w:body><w:p><w:r><w:t>한글 본문</w:t></w:r></w:p></w:body></w:document>"
        } else {
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><p>한글 본문</p>"
        };
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

#[test]
fn docx_preserves_split_runs_paragraphs_and_entities() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <w:document xmlns:w="urn:w"><w:body>
        <w:p><w:r><w:t>계</w:t></w:r><w:r><w:t>약</w:t></w:r><w:r><w:t>서</w:t></w:r></w:p>
        <w:p><w:r><w:t>A &amp; B &lt; C &#xAC00; &#44033;</w:t></w:r><w:del><w:r><w:delText>삭제됨</w:delText></w:r></w:del><w:instrText>필드 명령</w:instrText></w:p>
        </w:body></w:document>"#;
    let result = ParserRegistry::with_defaults().parse_bytes(
        "contract.docx",
        &zip_xml("word/document.xml", xml.as_bytes()),
        1_000,
    );
    assert_eq!(result.status, ParseStatus::Success);
    assert_eq!(result.text, "계약서\nA & B < C 가 각");
}

#[test]
fn docx_excludes_drawing_coordinates_but_keeps_textboxes() {
    let xml = r#"<w:document xmlns:w="urn:w" xmlns:a="urn:a"><w:body><w:p>
        <w:r><w:t>앞</w:t></w:r>
        <w:drawing><a:posOffset>7620</a:posOffset><a:ext cx="456565" cy="2376214"/><a:t>DrawingML theme text</a:t></w:drawing>
        <w:txbxContent><w:p><w:r><w:t>텍스트 상자</w:t></w:r></w:p></w:txbxContent>
        <w:r><w:t>뒤</w:t></w:r>
        </w:p></w:body></w:document>"#;
    let result = ParserRegistry::with_defaults().parse_bytes(
        "drawing.docx",
        &zip_xml("word/document.xml", xml.as_bytes()),
        1_000,
    );
    assert_eq!(result.status, ParseStatus::Success);
    assert_eq!(result.text, "앞텍스트 상자\n뒤");
    assert!(!result.text.contains("7620"));
    assert!(!result.text.contains("DrawingML"));
}

#[test]
fn docx_uses_only_the_document_body_and_rejects_malformed_xml() {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    archive
        .start_file("docProps/core.xml", SimpleFileOptions::default())
        .unwrap();
    archive
        .write_all(b"<cp:core><dc:title>metadata leak</dc:title></cp:core>")
        .unwrap();
    archive
        .start_file("word/document.xml", SimpleFileOptions::default())
        .unwrap();
    archive
        .write_all(
            b"<w:document><w:body><w:p><w:r><w:t>body only</w:t></w:r></w:p></w:body></w:document>",
        )
        .unwrap();
    let bytes = archive.finish().unwrap().into_inner();
    let result = ParserRegistry::with_defaults().parse_bytes("body.docx", &bytes, 1_000);
    assert_eq!(result.text, "body only");
    let malformed = zip_xml(
        "word/document.xml",
        b"<w:document><w:p><w:t>broken</w:p></w:document>",
    );
    assert_eq!(
        ParserRegistry::with_defaults()
            .parse_bytes("broken.docx", &malformed, 1_000)
            .status,
        ParseStatus::Failed
    );
}

#[test]
fn docx_rejects_oversized_expansion_without_panicking() {
    let oversized = format!(
        "<w:document><w:body><w:p><w:t>{}</w:t></w:p></w:body></w:document>",
        "x".repeat(16 * 1024 * 1024)
    );
    let result = ParserRegistry::with_defaults().parse_bytes(
        "large.docx",
        &zip_xml("word/document.xml", oversized.as_bytes()),
        100,
    );
    assert_eq!(result.status, ParseStatus::Failed);
    assert!(result.text.is_empty());
}

#[test]
fn pptx_runs_join_and_slide_parts_are_ordered_without_layout_noise() {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, body) in [
        (
            "ppt/slides/slide10.xml",
            "<a:p><a:r><a:t>열 번째</a:t></a:r></a:p>",
        ),
        (
            "ppt/slides/slide2.xml",
            "<a:p><a:r><a:t>계</a:t></a:r><a:r><a:t>약서</a:t></a:r></a:p><a:p><a:r><a:t>A &amp; B</a:t></a:r></a:p>",
        ),
        (
            "ppt/slides/slide1.xml",
            "<a:p><a:r><a:t>첫 슬라이드</a:t></a:r></a:p><a:posOffset>7620</a:posOffset>",
        ),
        (
            "ppt/slideLayouts/slideLayout1.xml",
            "<a:p><a:r><a:t>자리표시자</a:t></a:r></a:p>",
        ),
        (
            "ppt/notesSlides/notesSlide1.xml",
            "<a:p><a:r><a:t>발표자 노트</a:t></a:r></a:p>",
        ),
    ] {
        archive
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        write!(
            archive,
            "<p:sld xmlns:p='urn:p' xmlns:a='urn:a'>{body}</p:sld>"
        )
        .unwrap();
    }
    let bytes = archive.finish().unwrap().into_inner();
    let registry = ParserRegistry::with_defaults();
    for ext in ["pptx", "ppsx"] {
        let parsed = registry.parse_bytes(&format!("deck.{ext}"), &bytes, 4000);
        assert_eq!(parsed.status, ParseStatus::Success);
        assert_eq!(parsed.text, "첫 슬라이드\n\n계약서\nA & B\n\n열 번째");
    }
    let malformed = zip_xml("ppt/slides/slide1.xml", b"<p:sld><a:t>broken</p:sld>");
    assert_eq!(
        registry.parse_bytes("bad.pptx", &malformed, 4000).status,
        ParseStatus::Failed
    );
}
