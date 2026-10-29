use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use jikji_core::{PrepareOptions, Result, io_error, json_error};
use jikji_media_bridge::{MediaBridgeOutcome, MediaBridgeStatus};
use jikji_parser::ParseStatus;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::doc_chunks::chunk_rows;
use crate::doc_media::{CacheEntry, DocumentCacheRuntime, MEDIA_EXTENSIONS, SourceDocument};
use crate::doc_text_cache::{
    text_cache_path_for, write_doc_text_cache, write_generated_cache_file,
};
use crate::file_io::{dotted_ext, extension};
use crate::scan::{ScanResult, rel_path};

pub(crate) const DOCUMENT_EXTENSIONS: &[&str] = &[
    "rtf", "pdf", "epub", "eml", "ics", "sqlite", "sqlite3", "db", "doc", "docx", "ppt", "pptx",
    "pps", "ppsx", "xls", "xlsx", "hwp", "hwpx", "odt", "ods", "odp", "png", "jpg", "jpeg", "tif",
    "tiff", "webp", "bmp", "gif", "mp3", "wav", "m4a", "flac", "ogg", "aac", "opus", "wma", "mp4",
    "mov", "mkv", "avi", "webm", "m4v", "wmv", "flv", "mpg", "mpeg", "zip", "jar", "war", "tar",
    "tgz", "tbz", "txz", "7z", "rar",
];

pub(crate) struct DocumentBuild {
    pub(crate) rows: Vec<Value>,
    pub(crate) chunk_rows: Vec<Value>,
    pub(crate) live_digests: BTreeSet<String>,
    pub(crate) parsed: usize,
    pub(crate) failed: usize,
}

pub(crate) struct CacheDirs<'a> {
    pub(crate) text: &'a Path,
    pub(crate) meta: &'a Path,
}

struct DocumentRecord<'a> {
    ext: &'a str,
    digest: &'a str,
    rel: &'a str,
    entry: &'a CacheEntry,
    text_cache_path: String,
}

pub(crate) fn document_rows(
    scan: &ScanResult,
    dirs: CacheDirs<'_>,
    options: &PrepareOptions,
) -> Result<DocumentBuild> {
    let runtime = DocumentCacheRuntime::new();
    let mut rows = Vec::new();
    let mut chunks = Vec::new();
    let mut live_digests = BTreeSet::new();
    let mut parsed = 0usize;
    let mut failed = 0usize;
    for path in &scan.files {
        let ext = extension(path);
        if !DOCUMENT_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        if !options.enable_media_index && MEDIA_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        let rel = rel_path(&scan.root, path);
        let metadata = fs::metadata(path).map_err(|source| io_error(path, source))?;
        if hash_oversize(metadata.len(), options.max_hash_bytes) {
            rows.push(skipped_document_row(&rel, ext.as_str(), "hash_oversize"));
            failed += 1;
            continue;
        }
        let digest = match sha256_file_with_timeout(path, options.parse_timeout_seconds) {
            Ok(digest) => digest,
            Err(status) => {
                rows.push(skipped_document_row(&rel, ext.as_str(), status));
                failed += 1;
                continue;
            }
        };
        live_digests.insert(digest.clone());
        let source = SourceDocument {
            path,
            ext: ext.as_str(),
            byte_len: metadata.len(),
        };
        let entry = runtime.cache_entry(source, options);
        let body_text = bounded_body_text(&entry, options.doc_text_max_chars);
        let record = DocumentRecord {
            ext: ext.as_str(),
            digest: digest.as_str(),
            rel: rel.as_str(),
            entry: &entry,
            text_cache_path: text_cache_path_for(
                digest.as_str(),
                &body_text,
                options.doc_text_chunk_chars,
            ),
        };
        let row = document_row(&record);
        write_doc_text_cache(
            dirs.text,
            record.rel,
            record.digest,
            &body_text,
            options.doc_text_chunk_chars,
        )?;
        write_doc_meta(dirs.meta, &record)?;
        chunks.extend(chunk_rows(&row, record.digest, &body_text));
        if entry.parsed.status == ParseStatus::Failed {
            failed += 1;
        } else {
            parsed += 1;
        }
        rows.push(row);
    }
    Ok(DocumentBuild {
        rows,
        chunk_rows: chunks,
        live_digests,
        parsed,
        failed,
    })
}

fn document_row(record: &DocumentRecord<'_>) -> Value {
    let summary = summary_for(record.entry);
    json!({
        "path": record.rel,
        "file_id": format!("sha256:{}", record.digest),
        "name": Path::new(record.rel).file_name().and_then(|name| name.to_str()).unwrap_or(""),
        "ext": dotted_ext(record.ext),
        "sha256": record.digest,
        "parse_status": parse_status(record.entry.parsed.status),
        "parser": record.entry.parsed.parser_name,
        "media_bridge_status": record.entry.bridge.as_ref().map(|outcome| media_bridge_status(outcome.status)),
        "text_cache_path": record.text_cache_path,
        "doc_meta_path": format!(".jikji/doc_meta/sha256_{}.json", record.digest),
        "summary": summary
    })
}

fn skipped_document_row(rel: &str, ext: &str, parse_status: &str) -> Value {
    json!({
        "path": rel,
        "file_id": "",
        "name": Path::new(rel).file_name().and_then(|name| name.to_str()).unwrap_or(""),
        "ext": dotted_ext(ext),
        "sha256": "",
        "parse_status": parse_status,
        "parser": "",
        "media_bridge_status": null,
        "text_cache_path": "",
        "doc_meta_path": "",
        "summary": ""
    })
}

fn write_doc_meta(doc_meta_dir: &Path, record: &DocumentRecord<'_>) -> Result<()> {
    let path = doc_meta_dir.join(format!("sha256_{}.json", record.digest));
    let value = json!({
            "schema_version": 1,
            "source": "jikji",
            "file_id": format!("sha256:{}", record.digest),
            "path": record.rel,
            "parser": record.entry.parsed.parser_name,
            "parse_status": parse_status(record.entry.parsed.status),
            "parser_metadata": record.entry.parsed.metadata,
            "media_bridge": record.entry.bridge.as_ref().map(media_bridge_meta)
    });
    let text = serde_json::to_string_pretty(&value).map_err(|source| json_error(&path, source))?;
    write_generated_cache_file(&path, text.as_bytes())
}

fn sha256_file_with_timeout(
    path: &Path,
    timeout_seconds: f64,
) -> std::result::Result<String, &'static str> {
    if !timeout_seconds.is_finite() || timeout_seconds <= 0.0 {
        return sha256_file(path).map_err(|_| "failed");
    }
    let path = path.to_path_buf();
    let timeout = Duration::from_secs_f64(timeout_seconds);
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(sha256_file(&path));
    });
    match receiver.recv_timeout(timeout) {
        Ok(Ok(digest)) => Ok(digest),
        Ok(Err(_)) => Err("failed"),
        Err(_) => Err("timeout"),
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|source| io_error(path, source))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| io_error(path, source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_oversize(byte_len: u64, max_hash_bytes: u64) -> bool {
    max_hash_bytes > 0 && byte_len > max_hash_bytes
}

fn text_for_cache(entry: &CacheEntry) -> &str {
    if let Some(outcome) = &entry.bridge {
        if outcome.status == MediaBridgeStatus::Success && !outcome.text.is_empty() {
            return outcome.text.as_str();
        }
    }
    entry.parsed.text.as_str()
}

fn bounded_body_text(entry: &CacheEntry, max_source_chars: usize) -> String {
    text_for_cache(entry)
        .chars()
        .take(max_source_chars.max(1))
        .collect()
}

fn summary_for(entry: &CacheEntry) -> String {
    text_for_cache(entry)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}

fn parse_status(status: ParseStatus) -> &'static str {
    match status {
        ParseStatus::Success => "success",
        ParseStatus::ArchiveListing => "archive_listing",
        ParseStatus::MetadataOnly => "metadata_only",
        ParseStatus::Failed => "failed",
        ParseStatus::Unsupported => "unsupported",
    }
}

fn media_bridge_status(status: MediaBridgeStatus) -> &'static str {
    match status {
        MediaBridgeStatus::MetadataOnly => "metadata_only",
        MediaBridgeStatus::Success => "success",
        MediaBridgeStatus::Unavailable => "unavailable",
        MediaBridgeStatus::Failed => "failed",
        MediaBridgeStatus::Timeout => "timeout",
    }
}

fn media_bridge_meta(outcome: &MediaBridgeOutcome) -> Value {
    json!({
        "status": media_bridge_status(outcome.status),
        "metadata": outcome.metadata,
        "error": outcome.error
    })
}

#[cfg(test)]
mod tests {
    use super::{CacheDirs, document_rows};
    use crate::scan::scan_root;
    use jikji_core::PrepareOptions;
    use std::fs;
    use std::time::{Duration, Instant};

    #[cfg(unix)]
    #[test]
    fn document_hash_timeout_skips_blocked_file_and_keeps_others() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("ok.pdf"), b"%PDF-1.4\n%").expect("ok pdf");
        let fifo = dir.path().join("stuck.pdf");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(status.success(), "mkfifo {fifo:?}");

        let mut scan = scan_root(dir.path(), &PrepareOptions::default()).expect("scan");
        scan.files.push(scan.root.join("stuck.pdf"));

        let cache = dir.path().join("cache");
        let text = cache.join("doc_text");
        let meta = cache.join("doc_meta");
        fs::create_dir_all(&text).expect("text cache");
        fs::create_dir_all(&meta).expect("meta cache");

        let options = PrepareOptions {
            parse_timeout_seconds: 0.3,
            ..PrepareOptions::default()
        };
        let started = Instant::now();
        let docs = document_rows(
            &scan,
            CacheDirs {
                text: &text,
                meta: &meta,
            },
            &options,
        )
        .expect("document rows");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "blocked FIFO hash must not stall document indexing"
        );

        let stuck = docs
            .rows
            .iter()
            .find(|row| row["path"] == "stuck.pdf")
            .expect("stuck row");
        assert_eq!(stuck["parse_status"], "timeout");
        assert_eq!(stuck["sha256"], "");
        assert!(
            docs.rows.iter().any(|row| row["path"] == "ok.pdf"),
            "other documents should still be indexed: {:?}",
            docs.rows
        );
    }
}
