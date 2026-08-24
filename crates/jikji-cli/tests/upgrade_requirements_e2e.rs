#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

#[test]
fn central_db_default_policy_deep_index_refresh_and_agent_fallback_work_end_to_end() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("report.txt"),
        "document-visible-token-771",
    )
    .unwrap();
    for name in ["image.png", "audio.wav", "video.mp4"] {
        fs::write(fixture.root.join(name), format!("raw-{name}-bytes")).unwrap();
    }
    write_archive(
        &fixture.root.join("bundle.zip"),
        "configured-archive-body-token-884",
    );
    let engine = fixture.write_engine();

    let prepared = fixture.json(
        [
            "prepare",
            fixture.root_str().as_str(),
            "--no-agent-rules",
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared["files"], 5);
    assert!(fixture.database().is_file());
    assert!(!fixture.root.join(".jikji").exists());

    for filename in ["image.png", "audio.wav", "video.mp4", "bundle.zip"] {
        let found = fixture.json(
            [
                "find",
                fixture.root_str().as_str(),
                filename,
                "--no-background-refresh",
                "--json",
            ],
            &[],
        );
        assert_eq!(found["paths"][0], filename);
    }

    let media_tokens = [
        ("configured-image-body-token-881", "image.png"),
        ("configured-audio-body-token-882", "audio.wav"),
        ("configured-video-body-token-883", "video.mp4"),
        ("configured-archive-body-token-884", "bundle.zip"),
    ];
    for (token, _) in media_tokens {
        let found = fixture.json(
            [
                "find",
                fixture.root_str().as_str(),
                token,
                "--no-background-refresh",
                "--json",
            ],
            &[],
        );
        assert!(!candidate_evidence(&found).contains(token));
    }

    fixture.json(
        [
            "deep-index",
            fixture.root_str().as_str(),
            "--no-agent-rules",
            "--json",
        ],
        &[
            ("JIKJI_OCR_ENGINE", engine.as_path()),
            ("JIKJI_ASR_ENGINE", engine.as_path()),
        ],
    );
    for (token, filename) in media_tokens {
        let found = fixture.json(
            [
                "find",
                fixture.root_str().as_str(),
                token,
                "--no-background-refresh",
                "--json",
            ],
            &[],
        );
        assert!(
            candidate_evidence(&found).contains(token),
            "token={token} payload={found}"
        );
        assert_eq!(found["paths"][0], filename);
    }

    let ready = fixture.json(
        [
            "search",
            fixture.root_str().as_str(),
            "document-visible-token-771",
            "--json",
            "--no-background-refresh",
        ],
        &[],
    );
    assert_eq!(ready["index_status"], "ready");
    assert_eq!(ready["background_refresh_started"], false);

    let started = Instant::now();
    let stale = fixture.json(
        [
            "search",
            fixture.root_str().as_str(),
            "document-visible-token-771",
            "--stale-after-seconds",
            "0",
            "--json",
        ],
        &[],
    );
    assert_eq!(stale["index_status"], "stale_using_previous_index");
    assert_eq!(stale["background_refresh_started"], true);
    assert!(started.elapsed() < Duration::from_secs(2));

    let missing = tempfile::tempdir().unwrap();
    let first = fixture.json(
        [
            "find",
            path_str(missing.path()).as_str(),
            "missing-token",
            "--json",
        ],
        &[],
    );
    assert_eq!(first["handoff_action"], "jikji_retry");
    assert_eq!(first["max_jikji_retries"], 1);
    assert_eq!(first["raw_fallback_allowed"], false);
    let proof = first["retry_proof"].as_str().unwrap();
    let second = fixture.json(
        [
            "find",
            path_str(missing.path()).as_str(),
            "missing-token",
            "--after-jikji-retry",
            "--retry-proof",
            proof,
            "--json",
        ],
        &[],
    );
    assert_eq!(second["handoff_action"], "raw_fallback_after_retry");
    assert_eq!(second["max_raw_fallback_commands"], 2);
    assert_eq!(second["raw_fallback_allowed"], true);

    let skill = fixture.json(["skill-export", "--json"], &[]);
    let markdown = skill["skill_markdown"].as_str().unwrap();
    assert!(markdown.contains("Jikji Find First"));
    assert!(markdown.contains("exactly one sharper Jikji retry"));
    assert!(markdown.contains("deep-index"));
}

#[test]
fn coding_file_bodies_are_indexed_by_default() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("main.rs"),
        "fn code_body_qz991unique() -> &'static str { \"indexed\" }",
    )
    .unwrap();
    fixture.json(
        [
            "prepare",
            fixture.root_str().as_str(),
            "--no-agent-rules",
            "--json",
        ],
        &[],
    );
    let found = fixture.json(
        [
            "find",
            fixture.root_str().as_str(),
            "code_body_qz991unique",
            "--no-background-refresh",
            "--json",
        ],
        &[],
    );
    assert_eq!(found["paths"][0], "main.rs");
    assert!(candidate_evidence(&found).contains("code_body_qz991unique"));
}

#[test]
fn empty_results_reindex_once_and_retry_same_query() {
    for command in ["search", "brief", "find", "discover"] {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("old.txt"), "old-token").unwrap();
        fixture.json(
            [
                "prepare",
                fixture.root_str().as_str(),
                "--no-agent-rules",
                "--json",
            ],
            &[],
        );
        fs::write(fixture.root.join("new.txt"), "qznew997unique").unwrap();
        let payload = fixture.json(
            [
                command,
                fixture.root_str().as_str(),
                "qznew997unique",
                "--json",
                "--no-background-refresh",
            ],
            &[],
        );
        assert_eq!(
            payload["empty_result_reindexed"], true,
            "command={command} payload={payload}"
        );
        let paths = payload.get("paths").or_else(|| payload.get("answer_paths"));
        if let Some(paths) = paths.and_then(Value::as_array) {
            assert!(
                paths.iter().any(|path| path == "new.txt"),
                "command={command} payload={payload}"
            );
        } else {
            assert!(
                payload["candidates"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|candidate| {
                        candidate
                            .get("path")
                            .or_else(|| candidate.get("p"))
                            .and_then(Value::as_str)
                            == Some("new.txt")
                    }),
                "command={command} payload={payload}"
            );
        }
    }
}

fn candidate_evidence(value: &Value) -> String {
    value["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|candidate| candidate.get("ev").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn write_archive(path: &Path, body: &str) {
    let file = fs::File::create(path).unwrap();
    let mut archive = zip::ZipWriter::new(file);
    archive
        .start_file("inside.txt", SimpleFileOptions::default())
        .unwrap();
    archive.write_all(body.as_bytes()).unwrap();
    archive.finish().unwrap();
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    data: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let data = temp.path().join("data");
        fs::create_dir_all(&root).unwrap();
        Self {
            _temp: temp,
            root,
            data,
        }
    }
    fn root_str(&self) -> String {
        path_str(&self.root)
    }
    fn database(&self) -> PathBuf {
        self.data.join("jikji/index.sqlite")
    }
    fn write_engine(&self) -> PathBuf {
        let path = self._temp.path().join("media-engine.sh");
        fs::write(&path, "#!/bin/sh\ncase \"$JIKJI_MEDIA_ENGINE_KIND\" in\n image) echo configured-image-body-token-881;;\n audio) echo configured-audio-body-token-882;;\n video) echo configured-video-body-token-883;;\nesac\n").unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }
    fn run<I, S>(&self, args: I, extra: &[(&str, &Path)]) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jikji"));
        command.env("JIKJI_DATA_DIR", &self.data).args(args);
        for (key, value) in extra {
            command.env(key, value);
        }
        command.output().unwrap()
    }
    fn json<I, S>(&self, args: I, extra: &[(&str, &Path)]) -> Value
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = self.run(args, extra);
        assert!(
            output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
