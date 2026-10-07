use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use jikji_core::PrepareOptions;
use jikji_core::storage::root_storage_dir;

const REFRESH_LOCK: &str = ".refresh.lock";
const REFRESH_LOCK_ENV: &str = "JIKJI_BACKGROUND_REFRESH_LOCK";
const REFRESH_ROOT_ENV: &str = "JIKJI_BACKGROUND_REFRESH_ROOT";

pub(crate) struct BackgroundRefreshGuard(Option<PathBuf>);

impl BackgroundRefreshGuard {
    pub(crate) fn from_env() -> Self {
        let root = std::env::var_os(REFRESH_ROOT_ENV).map(PathBuf::from);
        let path = std::env::var_os(REFRESH_LOCK_ENV).map(PathBuf::from);
        let verified = root.and_then(|root| {
            let expected = root_storage_dir(&root).ok()?.join(REFRESH_LOCK);
            path.filter(|candidate| candidate == &expected)
        });
        Self(verified)
    }
}

impl Drop for BackgroundRefreshGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

pub(crate) fn start_background_refresh(root: &Path, options: &PrepareOptions) -> bool {
    let Ok(index_dir) = root_storage_dir(root) else {
        return false;
    };
    if !safe_existing_index_dir(&index_dir) || index_dir.join(".lock").exists() {
        return false;
    }
    let Some(refresh_lock) = reserve_refresh(&index_dir) else {
        return false;
    };
    if spawn_background_prepare(root, options, &refresh_lock).is_ok() {
        true
    } else {
        let _ = fs::remove_file(refresh_lock);
        false
    }
}

fn reserve_refresh(index_dir: &Path) -> Option<PathBuf> {
    let refresh_lock = index_dir.join(REFRESH_LOCK);
    let mut reservation = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&refresh_lock)
        .ok()?;
    let _ = writeln!(reservation, "{}", std::process::id());
    Some(refresh_lock)
}

fn safe_existing_index_dir(index_dir: &Path) -> bool {
    match fs::symlink_metadata(index_dir) {
        Ok(metadata) => metadata.is_dir() && !metadata.file_type().is_symlink(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => false,
    }
}

fn spawn_background_prepare(
    root: &Path,
    options: &PrepareOptions,
    refresh_lock: &Path,
) -> std::io::Result<std::process::Child> {
    let executable = std::env::current_exe()?;
    let mut command = Command::new(executable);
    command
        .arg("prepare")
        .arg(root)
        .arg("--parse-timeout")
        .arg(options.parse_timeout_seconds.to_string())
        .arg("--max-hash-bytes")
        .arg(options.max_hash_bytes.to_string())
        .arg("--doc-text-max-chars")
        .arg(options.doc_text_max_chars.to_string())
        .arg("--doc-text-chunk-chars")
        .arg(options.doc_text_chunk_chars.to_string())
        .arg("--json")
        // A refresh triggered by a search must not edit files under ROOT.
        .arg("--no-agent-rules")
        .arg("--no-root-map")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env(REFRESH_LOCK_ENV, refresh_lock)
        .env(REFRESH_ROOT_ENV, root);
    append_optional_prepare_args(&mut command, options);
    command.spawn()
}

fn append_optional_prepare_args(command: &mut Command, options: &PrepareOptions) {
    if options.include_hidden {
        command.arg("--include-hidden");
    }
    if options.include_sensitive {
        command.arg("--include-sensitive");
    }
    if let Some(max_files) = options.max_files {
        command.arg("--max-files").arg(max_files.to_string());
    }
    for pattern in &options.exclude_patterns {
        command.arg("--exclude").arg(pattern);
    }
    if options.enable_media_index {
        command.arg("--enable-media-index");
        command
            .arg("--media-index-max-mb")
            .arg(options.media_index_max_mb.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::{REFRESH_LOCK, reserve_refresh};

    #[test]
    fn refresh_reservation_is_atomic_and_reusable_after_cleanup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = reserve_refresh(temp.path()).expect("first reservation");
        assert!(reserve_refresh(temp.path()).is_none());
        std::fs::remove_file(&first).expect("release reservation");
        assert_eq!(
            reserve_refresh(temp.path()).expect("second reservation"),
            temp.path().join(REFRESH_LOCK)
        );
    }
}
