use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

#[test]
fn default_prepare_indexes_media_filename_without_root_sidecar() {
    let root = temp_root("native-media");
    let data_dir = temp_root("native-media-db");
    let mut png = vec![0_u8; 24];
    png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    png[12..16].copy_from_slice(b"IHDR");
    png[16..20].copy_from_slice(&320_u32.to_be_bytes());
    png[20..24].copy_from_slice(&200_u32.to_be_bytes());
    fs::write(root.join("photo.png"), png).expect("write media");

    let prepared = json_cmd(&data_dir, &["prepare", root_str(&root).as_str(), "--json"]);
    assert_eq!(prepared["files"], 1);
    assert!(!root.join(".jikji").exists());

    let found = json_cmd(
        &data_dir,
        &["find", root_str(&root).as_str(), "photo.png", "--json"],
    );
    assert_eq!(found["paths"][0], "photo.png", "{found}");
    assert!(data_dir.join("jikji/index.sqlite").is_file());
}

fn json_cmd(data_dir: &Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_jikji"))
        .env("JIKJI_DATA_DIR", data_dir)
        .args(args)
        .output()
        .expect("run jikji");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("json stdout")
}

fn root_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn temp_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("jikji-{label}-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&path).expect("temp root");
    path
}
