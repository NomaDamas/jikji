use std::io::Write;

use jikji_parser::{ParseStatus, ParserRegistry};

#[path = "support/pdf_fixture.rs"]
mod pdf_fixture;

#[test]
fn valid_plain_and_compressed_pdf_extract_rendered_text() {
    let registry = ParserRegistry::with_defaults();
    for compressed in [false, true] {
        let bytes = pdf_fixture::minimal_pdf("pdf-quality-token", compressed);
        let parsed = registry.parse_bytes("note.pdf", &bytes, 4_000);
        assert_eq!(parsed.status, ParseStatus::Success);
        assert_eq!(parsed.text.trim(), "pdf-quality-token");
        let capped = registry.parse_bytes("note.pdf", &bytes, 5);
        assert_eq!(capped.text, "pdf-q");
    }
}

#[test]
fn malformed_random_and_compressed_bytes_never_scrape_binary() {
    let registry = ParserRegistry::with_defaults();
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(b"random metadata (must-not-index)")
        .unwrap();
    let compressed = encoder.finish().unwrap();
    let random = (0..4096)
        .map(|i| ((i * 73 + 19) % 256) as u8)
        .collect::<Vec<_>>();
    let pdf = pdf_fixture::minimal_pdf("truncated-token", false);
    let cases = [
        b"%PDF-1.7\nnot actually a document".as_slice(),
        b"%PDF-1.4\n1 0 obj << /Title (must-not-index) >> endobj\n%%EOF",
        &[0, 159, 146, 255, 0, 1, 2],
        &compressed,
        &random,
        &pdf[..pdf.len() / 2],
    ];
    for bytes in cases {
        let parsed = registry.parse_bytes("broken.pdf", bytes, 4_000);
        assert_ne!(parsed.status, ParseStatus::Success);
        assert!(parsed.text.is_empty());
    }
}

#[test]
fn valid_pdf_without_rendered_text_is_metadata_only() {
    let parsed = ParserRegistry::with_defaults().parse_bytes(
        "blank.pdf",
        &pdf_fixture::minimal_pdf("", false),
        4_000,
    );
    assert_eq!(parsed.status, ParseStatus::MetadataOnly);
    assert!(parsed.text.is_empty());
}

#[test]
fn legacy_office_garbage_yields_no_text() {
    let registry = ParserRegistry::with_defaults();
    let mut utf16 = vec![0xff, 0xfe];
    for unit in "not-a-real-office-document".encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    for extension in ["doc", "ppt", "pps"] {
        for bytes in [b"random printable bytes".as_slice(), &utf16] {
            let parsed = registry.parse_bytes(&format!("opaque.{extension}"), bytes, 4_000);
            assert_eq!(parsed.status, ParseStatus::Failed, "{extension}");
            assert!(parsed.text.is_empty(), "{extension}");
        }
    }
    let parsed = registry.parse_bytes("opaque.xls", b"random printable bytes", 4_000);
    assert_eq!(parsed.status, ParseStatus::Unsupported);
    assert!(parsed.text.is_empty());
}

#[test]
fn doc_files_that_are_really_rtf_or_docx_are_parsed_by_content() {
    let registry = ParserRegistry::with_defaults();
    let rtf = registry.parse_bytes(
        "saved-by-word.doc",
        br"{\rtf1\ansi\ansicpg949 \'c3\'aa\'ba\'bf body\par}",
        4_000,
    );
    assert_eq!(rtf.status, ParseStatus::Success);
    assert_eq!(rtf.text, "챗봇 body");
    let html = registry.parse_bytes(
        "exported.doc",
        b"<html><body><p>html marker</p></body></html>",
        4_000,
    );
    assert!(html.text.contains("html marker"), "{html:?}");
}
