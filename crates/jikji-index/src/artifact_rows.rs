use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use jikji_core::{PrepareOptions, Result, io_error};
use serde_json::{Value, json};

use crate::doc_cache::DOCUMENT_EXTENSIONS;
use crate::file_io::{dotted_ext, extension, unix_seconds_now};
use crate::scan::{ScanResult, metadata_mtime_ns, rel_path};

pub(crate) fn file_rows(scan: &ScanResult, options: &PrepareOptions) -> Result<Vec<Value>> {
    scan.files
        .iter()
        .map(|path| {
            let metadata = fs::metadata(path).map_err(|source| io_error(path, source))?;
            let ext = extension(path);
            let rel = rel_path(&scan.root, path);
            let mtime = unix_seconds(&metadata.modified().ok()).unwrap_or(0);
            let created = unix_seconds(&metadata.created().ok())
                .unwrap_or(mtime)
                .to_string();
            let modified = mtime.to_string();
            Ok(json!({
                "status": "current",
                "path": rel,
                "name": path.file_name().and_then(|name| name.to_str()).unwrap_or(""),
                "ext": dotted_ext(&ext),
                "mime": mime_for(&ext),
                "size": metadata.len(),
                "mtime": mtime,
                "mtime_ns": metadata_mtime_ns(&metadata),
                "created": created,
                "modified": modified,
                "sha256": sha256_file_if_allowed(path, metadata.len(), options.max_hash_bytes)?,
                "parser_required": false,
                "parse_status": parse_status_for(&ext),
                "text_cache_path": "",
                "doc_meta_path": "",
                "keywords": keywords_for(&rel),
                "summary": "",
                "indexed_at": unix_seconds_now()
            }))
        })
        .collect()
}

pub(crate) fn folder_rows(scan: &ScanResult) -> Vec<Value> {
    let mut folders = Vec::with_capacity(scan.dirs.len() + 1);
    folders.push(scan.root.clone());
    folders.extend(scan.dirs.iter().cloned());
    folders
        .iter()
        .map(|path| folder_row(scan, path))
        .collect::<Vec<_>>()
}

pub(crate) fn merge_document_fields(mut file_rows: Vec<Value>, doc_rows: &[Value]) -> Vec<Value> {
    let docs_by_path = doc_rows
        .iter()
        .filter_map(|row| {
            row.get("path")
                .and_then(Value::as_str)
                .map(|path| (path.to_owned(), row))
        })
        .collect::<BTreeMap<_, _>>();
    for row in &mut file_rows {
        let Some(path) = row.get("path").and_then(Value::as_str) else {
            continue;
        };
        let Some(doc) = docs_by_path.get(path) else {
            continue;
        };
        let Some(target) = row.as_object_mut() else {
            continue;
        };
        let Some(source) = doc.as_object() else {
            continue;
        };
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
        target.insert("parser_required".to_owned(), json!(true));
    }
    file_rows
}

pub(crate) fn deleted_rows(previous: &[Value], current_paths: &BTreeSet<String>) -> Vec<Value> {
    previous
        .iter()
        .filter_map(|row| {
            let path = row.get("path").and_then(Value::as_str)?;
            if current_paths.contains(path)
                || row.get("status").and_then(Value::as_str) == Some("deleted")
            {
                return None;
            }
            let mut object = row.as_object().cloned().unwrap_or_default();
            object.insert("status".to_owned(), json!("deleted"));
            object.insert("deleted_at".to_owned(), json!(unix_seconds_now()));
            Some(Value::Object(object))
        })
        .collect()
}

pub(crate) fn native_text_extensions() -> &'static [&'static str] {
    &[
        ".cfg",
        ".conf",
        ".csv",
        ".htm",
        ".html",
        ".ini",
        ".json",
        ".jsonl",
        ".log",
        ".markdown",
        ".md",
        ".org",
        ".rst",
        ".tex",
        ".text",
        ".tsv",
        ".txt",
        ".xml",
        ".yaml",
        ".yml",
        ".toml",
        ".srt",
        ".vtt",
        ".rs",
        ".py",
        ".pyi",
        ".js",
        ".jsx",
        ".mjs",
        ".cjs",
        ".ts",
        ".tsx",
        ".java",
        ".kt",
        ".kts",
        ".go",
        ".c",
        ".h",
        ".cc",
        ".cpp",
        ".cxx",
        ".hpp",
        ".cs",
        ".swift",
        ".rb",
        ".php",
        ".sh",
        ".bash",
        ".zsh",
        ".fish",
        ".ps1",
        ".sql",
        ".scala",
        ".lua",
        ".r",
        ".dart",
        ".ex",
        ".exs",
        ".erl",
        ".hrl",
        ".vue",
        ".svelte",
        ".css",
        ".scss",
        ".sass",
        ".less",
    ]
}

fn folder_row(scan: &ScanResult, path: &Path) -> Value {
    let rel = if path == scan.root {
        ".".to_owned()
    } else {
        rel_path(&scan.root, path)
    };
    let child_folders = scan
        .dirs
        .iter()
        .filter(|dir| dir.parent() == Some(path))
        .filter_map(|dir| {
            dir.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    let direct_files = scan
        .files
        .iter()
        .filter(|file| file.parent() == Some(path))
        .collect::<Vec<_>>();
    let top_extensions = direct_files
        .iter()
        .map(|file| dotted_ext(&extension(file)))
        .filter(|ext| !ext.is_empty())
        .fold(BTreeMap::<String, usize>::new(), |mut counts, ext| {
            *counts.entry(ext).or_insert(0) += 1;
            counts
        });
    let total_size = direct_files
        .iter()
        .filter_map(|file| fs::metadata(file).ok().map(|metadata| metadata.len()))
        .sum::<u64>();
    json!({
        "folder_id": folder_id(&rel),
        "path": rel,
        "name": path.file_name().and_then(|name| name.to_str()).unwrap_or("."),
        "status": "current",
        "depth": folder_depth(&rel),
        "file_count_direct": direct_files.len(),
        "subfolder_count_direct": child_folders.len(),
        "total_size_direct": total_size,
        "top_extensions_direct": top_extensions,
        "child_folders": child_folders,
        "keywords": keywords_for(&rel),
        "summary": format!("{rel} - {} files, {} subfolders", direct_files.len(), child_folders.len())
    })
}

fn unix_seconds(time: &Option<SystemTime>) -> Option<u64> {
    time.and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
}

fn keywords_for(text: &str) -> Vec<String> {
    text.split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '-'))
        .map(|token| token.trim_matches(['.', '_', '-']).to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .take(16)
        .collect()
}

fn folder_id(rel: &str) -> String {
    if rel == "." {
        "root".to_owned()
    } else {
        format!("folder:{}", rel.replace('/', ":"))
    }
}

fn folder_depth(rel: &str) -> usize {
    if rel == "." {
        0
    } else {
        Path::new(rel).components().count()
    }
}

fn parse_status_for(ext: &str) -> &'static str {
    if DOCUMENT_EXTENSIONS.contains(&ext) {
        "not_required"
    } else if native_text_extensions().contains(&dotted_ext(ext).as_str()) {
        "native_text"
    } else {
        "not_required"
    }
}

fn sha256_file_if_allowed(path: &Path, byte_len: u64, max_hash_bytes: u64) -> Result<String> {
    if max_hash_bytes > 0 && byte_len > max_hash_bytes {
        return Ok(String::new());
    }
    sha256_file(path)
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "txt" | "text" | "log" | "md" | "markdown" | "rst" => "text/plain",
        "csv" => "text/csv",
        "json" | "jsonl" => "application/json",
        "html" | "htm" => "text/html",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read as _;

    use sha2::{Digest as _, Sha256};

    let mut file = fs::File::open(path).map_err(|source| io_error(path, source))?;
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
