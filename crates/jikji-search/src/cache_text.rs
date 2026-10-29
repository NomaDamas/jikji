use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
pub(crate) fn generated_cache_path(storage_dir: &Path, cache_path: &str) -> Option<PathBuf> {
    if cache_path.is_empty() {
        return None;
    }
    let rel = cache_path.strip_prefix(".jikji/").unwrap_or(cache_path);
    let rel_path = Path::new(rel);
    if rel_path.has_root()
        || rel_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return None;
    }
    Some(storage_dir.join(rel_path))
}

pub(crate) fn read_cache_text(storage_dir: &Path, cache_path: &str, limit: usize) -> String {
    let Some(path) = generated_cache_path(storage_dir, cache_path) else {
        return String::new();
    };
    if path.is_file() {
        return read_source_text(path, limit);
    }
    if path.is_dir() {
        return read_cache_dir_text(&path, limit);
    }
    String::new()
}

pub(crate) fn read_source_text(path: PathBuf, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(_) => return String::new(),
    };
    let max_bytes = u64::try_from(limit.saturating_mul(4)).unwrap_or(u64::MAX);
    let mut raw = Vec::new();
    if file.take(max_bytes).read_to_end(&mut raw).is_err() {
        return String::new();
    }
    let text = if raw.starts_with(&[0xff, 0xfe]) {
        String::from_utf16(
            &raw[2..]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>(),
        )
        .ok()
    } else if raw.starts_with(&[0xfe, 0xff]) {
        String::from_utf16(
            &raw[2..]
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>(),
        )
        .ok()
    } else {
        let bytes = match std::str::from_utf8(&raw) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() && raw.len() as u64 == max_bytes => {
                match std::str::from_utf8(&raw[..error.valid_up_to()]) {
                    Ok(text) => text,
                    Err(_) => return String::new(),
                }
            }
            Err(_) => return String::new(),
        };
        Some(bytes.to_owned())
    };
    text.filter(|value| {
        !value
            .chars()
            .any(|ch| ch == '\u{fffd}' || (ch.is_control() && !matches!(ch, '\t' | '\n' | '\r')))
    })
    .map(|value| value.chars().take(limit).collect())
    .unwrap_or_default()
}

fn read_cache_dir_text(path: &Path, limit: usize) -> String {
    let mut entries = match fs::read_dir(path) {
        Ok(entries) => entries.filter_map(Result::ok).collect::<Vec<_>>(),
        Err(_) => return String::new(),
    };
    entries.sort_by_key(|entry| entry.file_name());
    let mut text = String::new();
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("chunk_") || !entry.path().is_file() {
            continue;
        }
        text.push_str(&read_source_text(
            entry.path(),
            limit.saturating_sub(text.chars().count()),
        ));
        if text.chars().count() >= limit {
            break;
        }
        text.push('\n');
    }
    text.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempTree(PathBuf);

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_tree(label: &str) -> TempTree {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "jikji-cache-text-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temp tree");
        TempTree(path)
    }

    #[test]
    fn generated_cache_path_strips_jikji_prefix_and_joins_storage() {
        let storage = Path::new("/tmp/jikji/roots/601");
        let got = generated_cache_path(storage, ".jikji/doc_text/sha256_abc.txt").unwrap();
        assert_eq!(got, storage.join("doc_text/sha256_abc.txt"));
        let already_stripped = generated_cache_path(storage, "doc_meta/sha256_abc.json").unwrap();
        assert_eq!(already_stripped, storage.join("doc_meta/sha256_abc.json"));
    }

    #[test]
    fn generated_cache_path_rejects_empty_absolute_and_parent_dir() {
        let storage = Path::new("/tmp/jikji/roots/601");
        assert!(generated_cache_path(storage, "").is_none());
        assert!(generated_cache_path(storage, "/etc/passwd").is_none());
        assert!(generated_cache_path(storage, ".jikji/doc_text/../secret.txt").is_none());
        assert!(generated_cache_path(storage, "doc_text/../../etc/passwd").is_none());
    }

    #[test]
    fn read_cache_text_uses_storage_dir_not_scan_root_jikji() {
        let tmp = temp_tree("central-not-fuse");
        let scan_root = tmp.0.join("GoogleDrive/마커");
        let storage = tmp.0.join("data/jikji/roots/601");
        let rel = ".jikji/doc_text/sha256_deadbeef.txt";
        let fuse_cache = scan_root.join(rel);
        let central_cache = storage.join("doc_text/sha256_deadbeef.txt");
        fs::create_dir_all(fuse_cache.parent().unwrap()).expect("fuse cache dir");
        fs::create_dir_all(central_cache.parent().unwrap()).expect("central cache dir");
        fs::write(&fuse_cache, "FUSE_BODY").expect("write fuse decoy");
        fs::write(&central_cache, "CENTRAL_BODY").expect("write central cache");

        assert_eq!(scan_root.join(rel), fuse_cache);
        assert_eq!(read_cache_text(&storage, rel, 64_000), "CENTRAL_BODY");
    }

    #[test]
    fn read_cache_text_reads_chunk_directory_under_storage() {
        let tmp = temp_tree("chunk-dir");
        let storage = tmp.0.join("roots/601");
        let dir = storage.join("doc_text/sha256_dir");
        fs::create_dir_all(&dir).expect("chunk dir");
        fs::write(dir.join("chunk_0001.txt"), "AAA").expect("chunk 1");
        fs::write(dir.join("chunk_0002.txt"), "BBB").expect("chunk 2");
        fs::write(dir.join("other.txt"), "NO").expect("non chunk");

        let text = read_cache_text(&storage, ".jikji/doc_text/sha256_dir", 64_000);
        assert!(text.contains("AAA"), "{text}");
        assert!(text.contains("BBB"), "{text}");
        assert!(!text.contains("NO"), "{text}");
    }

    #[test]
    fn read_source_text_does_not_need_the_whole_file_for_a_char_limit() {
        let tmp = temp_tree("bounded-read");
        let path = tmp.0.join("sensor.txt");
        fs::write(&path, "x".repeat(1_048_576)).expect("large file");
        let text = read_source_text(path, 100);
        assert_eq!(text.len(), 100);
        assert_eq!(text, "x".repeat(100));
    }
}
