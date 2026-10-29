use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use jikji_core::{JikjiError, Result, io_error};
use serde_json::{Value, json};

use crate::file_io::unix_seconds_now;

pub(crate) struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    pub(crate) fn acquire(index_dir: &Path) -> Result<Self> {
        let path = index_dir.join(".lock");
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let payload =
                    json!({"pid": std::process::id(), "started_at_unix": unix_seconds_now()});
                file.write_all(payload.to_string().as_bytes())
                    .map_err(|source| io_error(&path, source))?;
                Ok(Self { path })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if remove_stale_lock(&path)? {
                    return Self::acquire(index_dir);
                }
                Err(JikjiError::Locked(path))
            }
            Err(source) => Err(io_error(path, source)),
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn remove_stale_lock(path: &Path) -> Result<bool> {
    let metadata = fs::metadata(path).map_err(|source| io_error(path, source))?;
    let now = unix_seconds_now();
    let payload = fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<BTreeMap<String, Value>>(&text).ok());
    if payload.as_ref().is_some_and(lock_holder_is_alive) {
        return Ok(false);
    }
    let age_from_mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| now.saturating_sub(duration.as_secs()));
    let age_from_payload = payload
        .as_ref()
        .and_then(|payload| payload.get("started_at_unix").and_then(Value::as_u64))
        .map_or(age_from_mtime, |started| now.saturating_sub(started));
    if age_from_mtime.max(age_from_payload) < 3600 {
        return Ok(false);
    }
    fs::remove_file(path).map_err(|source| io_error(path, source))?;
    Ok(true)
}

fn lock_holder_is_alive(payload: &BTreeMap<String, Value>) -> bool {
    payload
        .get("pid")
        .and_then(Value::as_u64)
        .is_some_and(process_looks_like_live_jikji)
}

fn process_looks_like_live_jikji(pid: u64) -> bool {
    #[cfg(target_os = "linux")]
    {
        let proc_dir = Path::new("/proc").join(pid.to_string());
        if !proc_dir.exists() {
            return false;
        }
        match fs::read_to_string(proc_dir.join("comm")) {
            Ok(comm) => comm.contains("jikji"),
            Err(_) => true,
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .is_some_and(|output| String::from_utf8_lossy(&output.stdout).contains("jikji"))
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .is_some_and(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .to_ascii_lowercase()
                    .contains("jikji")
            })
    }
}

#[cfg(test)]
mod tests {
    use super::LockGuard;
    use jikji_core::JikjiError;
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_index(label: &str) -> TempDir {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("jikji-lock-{label}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("temp index");
        TempDir(path)
    }

    fn write_lock(dir: &std::path::Path, pid: u32, started_at_unix: u64) {
        fs::write(
            dir.join(".lock"),
            json!({"pid": pid, "started_at_unix": started_at_unix}).to_string(),
        )
        .expect("write lock");
    }

    #[test]
    fn acquire_does_not_steal_lock_held_by_live_jikji_process() {
        let dir = temp_index("live");
        write_lock(&dir.0, std::process::id(), 1);
        match LockGuard::acquire(&dir.0) {
            Err(err) => assert!(matches!(err, JikjiError::Locked(_)), "{err:?}"),
            Ok(_) => panic!("live lock must remain"),
        }
        assert!(dir.0.join(".lock").exists());
    }

    #[test]
    fn acquire_steals_old_lock_from_dead_pid() {
        let dir = temp_index("dead");
        write_lock(&dir.0, u32::MAX, 1);
        let guard = LockGuard::acquire(&dir.0).expect("steal dead lock");
        drop(guard);
        assert!(!dir.0.join(".lock").exists());
    }
}
