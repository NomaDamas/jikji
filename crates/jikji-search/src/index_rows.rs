use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use jikji_core::cloud_content::CloudContentPolicy;

use serde_json::{Value, json};

use crate::cache_text::{read_cache_text, read_source_text};
use crate::tokenizer::{filename_lookup_keys, query_terms, token_counts};

const FIELD_WEIGHTS: &[(&str, f64)] = &[
    ("path", 5.0),
    ("name", 6.0),
    ("ext", 2.0),
    ("body", 1.0),
    ("meta", 2.2),
    ("semantic", 3.0),
];

#[derive(Debug, Clone)]
pub(crate) struct IndexRow {
    pub path: String,
    pub name: String,
    pub ext: String,
    pub duplicate_group_id: String,
    pub text_cache_path: String,
    pub summary: String,
    pub body: String,
    pub filename_keys: Vec<String>,
    pub row_json: Value,
}

pub(crate) fn rows_from_cards(
    root: &Path,
    storage_dir: &Path,
    file_cards: &[Value],
    chunk_rows: &[Value],
) -> Vec<IndexRow> {
    let mut chunks_by_path = BTreeMap::<String, Vec<&Value>>::new();
    for chunk in chunk_rows {
        if let Some(path) = value_str(chunk, "path") {
            chunks_by_path.entry(path).or_default().push(chunk);
        }
    }
    let policy = CloudContentPolicy::load_or_deny(root);
    file_cards
        .iter()
        .filter(|card| card.get("status").and_then(Value::as_str) != Some("deleted"))
        .filter_map(|card| row_from_card(root, storage_dir, card, &chunks_by_path, &policy))
        .collect()
}

fn row_from_card(
    root: &Path,
    storage_dir: &Path,
    card: &Value,
    chunks_by_path: &BTreeMap<String, Vec<&Value>>,
    policy: &CloudContentPolicy,
) -> Option<IndexRow> {
    let path = value_str(card, "path")?;
    if path.contains("/.jikji/") || path.starts_with(".jikji/") {
        return None;
    }
    let name = value_str(card, "name").unwrap_or_else(|| {
        Path::new(&path)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_owned()
    });
    let ext = value_str(card, "ext").unwrap_or_default();
    let text_cache_path = value_str(card, "text_cache_path").unwrap_or_default();
    let summary = value_str(card, "summary").unwrap_or_default();
    let body = body_for(
        root,
        storage_dir,
        &path,
        &ext,
        &text_cache_path,
        chunks_by_path,
        policy,
    );
    let chunks = chunks_by_path
        .get(&path)
        .into_iter()
        .flatten()
        .take(48)
        .map(|chunk| (*chunk).clone())
        .collect::<Vec<_>>();
    let mut filename_keys = filename_lookup_keys(&path);
    filename_keys.extend(filename_lookup_keys(&name));
    filename_keys.sort();
    filename_keys.dedup();
    let row_json = json!({
        "path": path,
        "name": name,
        "ext": ext,
        "duplicate_group_id": value_str(card, "duplicate_group_id").unwrap_or_default(),
        "filename_lookup_keys": filename_keys,
        "content_terms": array_values(card, "content_terms"),
        "rare_terms": array_values(card, "rare_terms"),
        "phrase_signatures": array_values(card, "phrase_signatures"),
        "intent_tags": array_values(card, "intent_tags"),
        "format_hints": array_values(card, "format_hints"),
        "folder_terms": array_values(card, "folder_terms"),
        "folder_roles": array_values(card, "folder_roles"),
        "path_terms": array_values(card, "path_terms"),
        "name_terms": array_values(card, "name_terms"),
        "keywords": array_values(card, "content_terms"),
        "semantic_hints": array_values(card, "semantic_hints"),
        "summary": summary,
        "text_cache_path": text_cache_path,
        "body_text": body,
        "map_chunks": chunks,
        "evidence_previews": array_values(card, "evidence_previews"),
        "evidence": evidence_for(&body, &summary, &name),
    });
    Some(IndexRow {
        path,
        name,
        ext,
        duplicate_group_id: value_str(card, "duplicate_group_id").unwrap_or_default(),
        text_cache_path,
        summary,
        body,
        filename_keys,
        row_json,
    })
}

fn body_for(
    root: &Path,
    storage_dir: &Path,
    path: &str,
    ext: &str,
    text_cache_path: &str,
    chunks_by_path: &BTreeMap<String, Vec<&Value>>,
    policy: &CloudContentPolicy,
) -> String {
    let mut body_parts = Vec::new();
    let cached = read_cache_text(storage_dir, text_cache_path, 64_000);
    let cache_empty = cached.is_empty();
    body_parts.push(cached);
    if cache_empty && is_native_text_ext(ext) && policy.allows(&root.join(path)) {
        body_parts.push(read_source_text(root.join(path), 24_000));
    }
    for chunk in chunks_by_path.get(path).into_iter().flatten().take(48) {
        body_parts.push(value_str(chunk, "preview").unwrap_or_default());
        body_parts.push(array_text(chunk, "content_terms"));
        body_parts.push(array_text(chunk, "rare_terms"));
        body_parts.push(array_text(chunk, "phrase_signatures"));
        body_parts.push(array_text(chunk, "intent_tags"));
    }
    body_parts.join("\n")
}

pub(crate) fn row_terms(row: &IndexRow) -> BTreeSet<String> {
    let mut out = query_terms(&format!(
        "{} {} {} {} {}",
        row.path, row.name, row.ext, row.summary, row.body
    ));
    for key in &row.filename_keys {
        out.insert(key.clone());
    }
    out
}

pub(crate) fn fielded_terms(row: &IndexRow) -> BTreeMap<&'static str, BTreeSet<(String, usize)>> {
    let mut fields = BTreeMap::new();
    fields.insert(
        "path",
        token_counts(
            &format!("{} {}", row.path, row.filename_keys.join(" ")),
            4096,
        ),
    );
    fields.insert("name", token_counts(&row.name, 4096));
    fields.insert("ext", token_counts(row.ext.trim_start_matches('.'), 128));
    fields.insert("body", token_counts(&row.body, 4096));
    fields.insert("meta", token_counts(&row.summary, 1024));
    fields.insert(
        "semantic",
        token_counts(&format!("{} {}", row.summary, row.body), 4096),
    );
    fields
}

pub(crate) fn field_weight(field: &str) -> f64 {
    FIELD_WEIGHTS
        .iter()
        .find_map(|(name, weight)| (*name == field).then_some(*weight))
        .unwrap_or(1.0)
}

pub(crate) fn evidence_for(body: &str, summary: &str, fallback: &str) -> Vec<String> {
    let joined = if body.trim().is_empty() {
        summary
    } else {
        body
    };
    let mut out = Vec::new();
    for line in joined.split(['\n', '.', '!', '?']) {
        let compact = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if compact.starts_with('#') {
            continue;
        }
        if compact.chars().count() >= 12 {
            out.push(compact.chars().take(240).collect());
        }
        if out.len() >= 3 {
            break;
        }
    }
    if out.is_empty() && !fallback.is_empty() {
        out.push(fallback.to_owned());
    }
    out
}

fn value_str(row: &Value, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
}

fn array_text(row: &Value, key: &str) -> String {
    array_values(row, key).join(" ")
}

fn array_values(row: &Value, key: &str) -> Vec<String> {
    row.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn is_native_text_ext(ext: &str) -> bool {
    matches!(
        ext.trim_start_matches('.'),
        "md" | "markdown"
            | "txt"
            | "text"
            | "rst"
            | "log"
            | "csv"
            | "tsv"
            | "json"
            | "jsonl"
            | "yaml"
            | "yml"
            | "xml"
            | "html"
            | "htm"
            | "ini"
            | "cfg"
            | "conf"
            | "org"
            | "tex"
            | "toml"
            | "srt"
            | "vtt"
            | "rs"
            | "py"
            | "pyi"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "ts"
            | "tsx"
            | "java"
            | "kt"
            | "kts"
            | "go"
            | "c"
            | "h"
            | "cc"
            | "cpp"
            | "cxx"
            | "hpp"
            | "cs"
            | "swift"
            | "rb"
            | "php"
            | "sh"
            | "bash"
            | "zsh"
            | "fish"
            | "ps1"
            | "sql"
            | "scala"
            | "lua"
            | "r"
            | "dart"
            | "ex"
            | "exs"
            | "erl"
            | "hrl"
            | "vue"
            | "svelte"
            | "css"
            | "scss"
            | "sass"
            | "less"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempTree(std::path::PathBuf);

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn rows_from_cards_reads_cache_from_storage_not_scan_root() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let tmp = TempTree(
            std::env::temp_dir().join(format!("jikji-index-rows-{}-{nonce}", std::process::id())),
        );
        let scan_root = tmp.0.join("GoogleDrive/마커");
        let storage = tmp.0.join("data/jikji/roots/601");
        let rel = ".jikji/doc_text/sha256_deadbeef.txt";
        fs::create_dir_all(scan_root.join(".jikji/doc_text")).expect("scan cache dir");
        fs::create_dir_all(storage.join("doc_text")).expect("storage cache dir");
        fs::write(scan_root.join(rel), "FUSE_BODY").expect("fuse decoy");
        fs::write(storage.join("doc_text/sha256_deadbeef.txt"), "CENTRAL_BODY").expect("central");

        let cards = [json!({
            "path": "paper.pdf",
            "name": "paper.pdf",
            "ext": "pdf",
            "text_cache_path": rel,
            "summary": "paper",
        })];
        let rows = rows_from_cards(&scan_root, &storage, &cards, &[]);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].body.contains("CENTRAL_BODY"), "{}", rows[0].body);
        assert!(!rows[0].body.contains("FUSE_BODY"), "{}", rows[0].body);
    }

    #[test]
    fn rows_from_cards_skips_native_source_when_storage_cache_exists() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let tmp = TempTree(std::env::temp_dir().join(format!(
            "jikji-index-rows-cache-{}-{nonce}",
            std::process::id()
        )));
        let scan_root = tmp.0.join("GoogleDrive/마커");
        let storage = tmp.0.join("data/jikji/roots/601");
        fs::create_dir_all(&scan_root).expect("scan root");
        fs::create_dir_all(storage.join("doc_text")).expect("storage cache dir");
        fs::write(scan_root.join("sensor.txt"), "NATIVE_FUSE_BODY").expect("native source");
        fs::write(
            storage.join("doc_text/sha256_cached.txt"),
            "CENTRAL_CACHE_BODY",
        )
        .expect("central cache");

        let cards = [json!({
            "path": "sensor.txt",
            "name": "sensor.txt",
            "ext": "txt",
            "text_cache_path": ".jikji/doc_text/sha256_cached.txt",
            "summary": "sensor",
        })];
        let rows = rows_from_cards(&scan_root, &storage, &cards, &[]);
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].body.contains("CENTRAL_CACHE_BODY"),
            "{}",
            rows[0].body
        );
        assert!(
            !rows[0].body.contains("NATIVE_FUSE_BODY"),
            "{}",
            rows[0].body
        );
    }

    #[test]
    fn rows_from_cards_reads_native_source_when_cache_is_empty() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let tmp = TempTree(std::env::temp_dir().join(format!(
            "jikji-index-rows-native-{}-{nonce}",
            std::process::id()
        )));
        let scan_root = tmp.0.join("Documents/notes");
        let storage = tmp.0.join("data/jikji/roots/601");
        fs::create_dir_all(&scan_root).expect("scan root");
        fs::create_dir_all(&storage).expect("storage");
        fs::write(scan_root.join("note.txt"), "NATIVE_ONLY_BODY").expect("native source");

        let cards = [json!({
            "path": "note.txt",
            "name": "note.txt",
            "ext": "txt",
            "text_cache_path": "",
            "summary": "note",
        })];
        let rows = rows_from_cards(&scan_root, &storage, &cards, &[]);
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].body.contains("NATIVE_ONLY_BODY"),
            "{}",
            rows[0].body
        );
    }

    #[test]
    fn rows_from_cards_skips_native_source_on_google_drive_cache_miss() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let tmp = TempTree(std::env::temp_dir().join(format!(
            "jikji-index-rows-drive-skip-{}-{nonce}",
            std::process::id()
        )));
        let scan_root = tmp.0.join("GoogleDrive/마커");
        let storage = tmp.0.join("data/jikji/roots/601");
        fs::create_dir_all(&scan_root).expect("scan root");
        fs::create_dir_all(&storage).expect("storage");
        fs::write(scan_root.join("note.txt"), "NATIVE_FUSE_BODY").expect("native source");

        let cards = [json!({
            "path": "note.txt",
            "name": "note.txt",
            "ext": "txt",
            "text_cache_path": "",
            "summary": "note",
        })];
        let rows = rows_from_cards(&scan_root, &storage, &cards, &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "note.txt");
        assert!(
            !rows[0].body.contains("NATIVE_FUSE_BODY"),
            "{}",
            rows[0].body
        );
    }

    #[cfg(unix)]
    #[test]
    fn rows_from_cards_skips_native_source_through_symlink_into_google_drive() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let tmp = TempTree(std::env::temp_dir().join(format!(
            "jikji-index-rows-drive-link-{}-{nonce}",
            std::process::id()
        )));
        let real = tmp.0.join("GoogleDrive/마커");
        let link = tmp.0.join("Drive");
        let storage = tmp.0.join("data/jikji/roots/601");
        fs::create_dir_all(&real).expect("real drive root");
        fs::create_dir_all(&storage).expect("storage");
        std::os::unix::fs::symlink(&real, &link).expect("drive symlink");
        fs::write(real.join("note.txt"), "NATIVE_FUSE_BODY").expect("native source");

        let cards = [json!({
            "path": "note.txt",
            "name": "note.txt",
            "ext": "txt",
            "text_cache_path": "",
            "summary": "note",
        })];
        let rows = rows_from_cards(&link, &storage, &cards, &[]);
        assert_eq!(rows.len(), 1);
        assert!(
            !rows[0].body.contains("NATIVE_FUSE_BODY"),
            "{}",
            rows[0].body
        );
    }

    #[cfg(unix)]
    #[test]
    fn rows_from_cards_skips_native_source_through_parent_drive_symlink() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let tmp = TempTree(std::env::temp_dir().join(format!(
            "jikji-index-rows-drive-parent-{}-{nonce}",
            std::process::id()
        )));
        let real = tmp.0.join("GoogleDrive/마커");
        let link_parent = tmp.0.join("Drive");
        let scan_root = link_parent.join("마커");
        let storage = tmp.0.join("data/jikji/roots/601");
        fs::create_dir_all(&real).expect("real drive root");
        fs::create_dir_all(&storage).expect("storage");
        std::os::unix::fs::symlink(tmp.0.join("GoogleDrive"), &link_parent)
            .expect("parent drive symlink");
        fs::write(real.join("note.txt"), "NATIVE_FUSE_BODY").expect("native source");

        let cards = [json!({
            "path": "note.txt",
            "name": "note.txt",
            "ext": "txt",
            "text_cache_path": "",
            "summary": "note",
        })];
        let rows = rows_from_cards(&scan_root, &storage, &cards, &[]);
        assert_eq!(rows.len(), 1);
        assert!(
            !rows[0].body.contains("NATIVE_FUSE_BODY"),
            "{}",
            rows[0].body
        );
    }

    #[cfg(unix)]
    #[test]
    fn rows_from_cards_skips_native_source_when_file_is_symlink_into_google_drive() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let tmp = TempTree(std::env::temp_dir().join(format!(
            "jikji-index-rows-file-link-{}-{nonce}",
            std::process::id()
        )));
        let scan_root = tmp.0.join("Documents/notes");
        let real = tmp.0.join("GoogleDrive/마커/secret.txt");
        let storage = tmp.0.join("data/jikji/roots/601");
        fs::create_dir_all(&scan_root).expect("scan root");
        fs::create_dir_all(real.parent().expect("parent")).expect("drive file parent");
        fs::create_dir_all(&storage).expect("storage");
        fs::write(&real, "NATIVE_FUSE_BODY").expect("native source");
        std::os::unix::fs::symlink(&real, scan_root.join("note.txt")).expect("file symlink");

        let cards = [json!({
            "path": "note.txt",
            "name": "note.txt",
            "ext": "txt",
            "text_cache_path": "",
            "summary": "note",
        })];
        let rows = rows_from_cards(&scan_root, &storage, &cards, &[]);
        assert_eq!(rows.len(), 1);
        assert!(
            !rows[0].body.contains("NATIVE_FUSE_BODY"),
            "{}",
            rows[0].body
        );
    }

    #[test]
    fn rows_from_cards_skips_native_source_on_rclone_drive_mount_names() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        for (idx, mount) in ["gdrive", "google-drive", "GDrive"].iter().enumerate() {
            let tmp = TempTree(std::env::temp_dir().join(format!(
                "jikji-index-rows-rclone-{idx}-{}-{nonce}",
                std::process::id()
            )));
            let scan_root = tmp.0.join(mount).join("마커");
            let storage = tmp.0.join("data/jikji/roots/601");
            fs::create_dir_all(&scan_root).expect("scan root");
            fs::create_dir_all(&storage).expect("storage");
            fs::write(scan_root.join("note.txt"), "NATIVE_FUSE_BODY").expect("native source");
            let cards = [json!({
                "path": "note.txt",
                "name": "note.txt",
                "ext": "txt",
                "text_cache_path": "",
                "summary": "note",
            })];
            let rows = rows_from_cards(&scan_root, &storage, &cards, &[]);
            assert_eq!(rows.len(), 1, "{mount}");
            assert!(
                !rows[0].body.contains("NATIVE_FUSE_BODY"),
                "{mount}: {}",
                rows[0].body
            );
        }
    }
}
