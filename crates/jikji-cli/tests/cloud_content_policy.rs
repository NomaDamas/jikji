//! Cloud-mounted drives: file names only by default, contents only for folders the user approved.
//! Runs the real binary against an isolated data dir on a folder named like a cloud mount.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    drive: PathBuf,
    data: PathBuf,
}

impl Fixture {
    /// `GoogleDrive/{docs,other}`, each with a plain-text file and an RTF document.
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let drive = temp.path().join("GoogleDrive");
        let data = temp.path().join("data");
        for (dir, word) in [("docs", "kelvorin"), ("other", "zyxwvut")] {
            fs::create_dir_all(drive.join(dir)).unwrap();
            fs::write(drive.join(dir).join("memo.txt"), format!("{word} budget")).unwrap();
            fs::write(
                drive.join(dir).join("contract.rtf"),
                format!("{{\\rtf1\\ansi {word}rtf clause}}"),
            )
            .unwrap();
        }
        Self {
            _temp: temp,
            drive,
            data,
        }
    }

    fn output(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_jikji"))
            .env("JIKJI_DATA_DIR", &self.data)
            .args(args)
            .output()
            .unwrap()
    }

    fn run(&self, args: &[&str]) -> Value {
        let output = self.output(args);
        assert!(
            output.status.success(),
            "args={args:?} stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn prepare(&self, folder: &Path, flags: &[&str]) -> Value {
        let folder = folder.to_string_lossy().into_owned();
        let mut args = vec!["prepare", folder.as_str(), "--no-agent-rules", "--json"];
        args.extend_from_slice(flags);
        self.run(&args)
    }

    fn find(&self, folder: &Path, query: &str) -> Value {
        let folder = folder.to_string_lossy().into_owned();
        self.run(&["find", &folder, query, "--json", "--no-background-refresh"])
    }
}

fn paths(payload: &Value) -> Vec<String> {
    payload["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| path.as_str().unwrap().to_owned())
        .collect()
}

fn codes(payload: &Value) -> Vec<String> {
    payload["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|note| note["code"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_cloud_drive_is_indexed_by_file_name_only_and_says_so() {
    let fixture = Fixture::new();
    let prepared = fixture.prepare(&fixture.drive, &[]);
    assert_eq!(prepared["cloud_content"]["mode"], "filename_only");
    assert!(prepared["cloud_content"]["note"].is_string());

    // Names are searchable; words inside txt and rtf files are not.
    let by_name = fixture.find(&fixture.drive, "contract");
    assert!(paths(&by_name).contains(&"docs/contract.rtf".to_owned()));
    assert_eq!(by_name["cloud_content"]["mode"], "filename_only");
    assert!(codes(&by_name).contains(&"cloud_filename_only".to_owned()));
    for word in ["kelvorin", "zyxwvut", "kelvorinrtf"] {
        let by_text = fixture.find(&fixture.drive, word);
        assert!(paths(&by_text).is_empty(), "{word}: {by_text}");
        assert!(codes(&by_text).contains(&"cloud_filename_only".to_owned()));
        assert!(by_text["cloud_content"]["how_to_search_contents"].is_string());
    }
    // Running it again changes nothing.
    let again = fixture.find(&fixture.drive, "kelvorin");
    assert!(paths(&again).is_empty());
}

#[test]
fn approving_one_folder_indexes_its_contents_and_a_parent_refresh_keeps_them() {
    let fixture = Fixture::new();
    fixture.prepare(&fixture.drive, &[]);
    let docs = fixture.drive.join("docs");

    let approved = fixture.prepare(&docs, &["--approve-cloud-content"]);
    assert_eq!(approved["cloud_content"]["mode"], "approved_folder");
    for word in ["kelvorin", "kelvorinrtf"] {
        let found = fixture.find(&docs, word);
        assert_eq!(paths(&found).len(), 1, "{word}: {found}");
        assert_eq!(found["results"][0]["text_indexed"], true);
        assert!(!codes(&found).contains(&"cloud_filename_only".to_owned()));
    }
    // The folder that was not approved stays name-only.
    assert!(paths(&fixture.find(&fixture.drive.join("other"), "zyxwvut")).is_empty());

    // Refreshing the parent mount reads the approved folder and only that folder.
    let parent = fixture.prepare(&fixture.drive, &[]);
    assert_eq!(parent["cloud_content"]["mode"], "filename_only");
    assert_eq!(
        parent["cloud_content"]["approved_below"][0]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next(),
        Some("docs")
    );
    let in_parent = fixture.find(&fixture.drive, "kelvorin");
    assert_eq!(paths(&in_parent), ["docs/memo.txt"], "{in_parent}");
    let message = in_parent["notes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|note| note["code"] == "cloud_filename_only")
        .map(|note| note["message"].as_str().unwrap().to_owned())
        .unwrap();
    assert!(
        message.contains("approved folders below") && message.contains("docs"),
        "{message}"
    );
    assert!(paths(&fixture.find(&fixture.drive, "zyxwvut")).is_empty());
    assert_eq!(
        fixture.find(&fixture.drive, "zyxwvut")["cloud_content"]["approved_folders_below"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn revoking_an_approval_returns_the_folder_to_file_names_only() {
    let fixture = Fixture::new();
    let docs = fixture.drive.join("docs");
    fixture.prepare(&docs, &["--approve-cloud-content"]);
    assert_eq!(paths(&fixture.find(&docs, "kelvorin")).len(), 1);

    let revoked = fixture.prepare(&docs, &["--revoke-cloud-content"]);
    assert_eq!(revoked["cloud_content"]["mode"], "filename_only");
    assert!(paths(&fixture.find(&docs, "kelvorin")).is_empty());
    // A later plain prepare must not bring the contents back.
    fixture.prepare(&docs, &[]);
    assert!(paths(&fixture.find(&docs, "kelvorin")).is_empty());
}

#[test]
fn approving_an_ordinary_folder_is_refused_and_ordinary_folders_keep_full_text() {
    let fixture = Fixture::new();
    let local = fixture.data.parent().unwrap().join("local");
    fs::create_dir_all(&local).unwrap();
    fs::write(local.join("a.txt"), "plainword budget").unwrap();

    let local_arg = local.to_string_lossy().into_owned();
    let refused = fixture.output(&["prepare", &local_arg, "--approve-cloud-content", "--json"]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("not a cloud-drive path"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );

    let prepared = fixture.prepare(&local, &[]);
    assert_eq!(prepared["cloud_content"]["mode"], "full");
    let found = fixture.find(&local, "plainword");
    assert_eq!(paths(&found), ["a.txt"]);
    assert!(found.get("cloud_content").is_none());
}

#[test]
fn an_unapproved_or_missing_cloud_folder_fails_clearly() {
    let fixture = Fixture::new();
    let missing = fixture.drive.join("no-such-folder");
    let refused = fixture.output(&[
        "prepare",
        &missing.to_string_lossy(),
        "--approve-cloud-content",
        "--json",
    ]);
    assert!(!refused.status.success());
    let found = fixture.find(&fixture.drive, "anything");
    assert_eq!(found["index_status"], "missing");
}

#[test]
fn a_cloud_index_built_with_text_before_the_policy_is_reported_as_it_is_and_kept_on_refresh() {
    // Simulate an index made by an older release: contents of a cloud folder stored without any
    // recorded approval, by indexing the folder as an ordinary one under another name, then
    // checking how `find` and a refresh treat it once it sits under a cloud-named path.
    let fixture = Fixture::new();
    let legacy = fixture.drive.join("docs");
    fixture.prepare(&legacy, &["--approve-cloud-content"]);
    // Drop the recorded decision, as if it never existed.
    let db = fixture.data.join("jikji/index.sqlite");
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute("DELETE FROM cloud_content_approvals", [])
        .unwrap();
    drop(connection);

    let found = fixture.find(&legacy, "kelvorin");
    assert_eq!(paths(&found).len(), 1, "{found}");
    assert_eq!(found["cloud_content"]["mode"], "indexed_before_policy");
    assert!(!codes(&found).contains(&"cloud_filename_only".to_owned()));

    // A refresh keeps the text instead of silently dropping it.
    let refreshed = fixture.prepare(&legacy, &[]);
    assert_eq!(refreshed["cloud_content"]["mode"], "approved_folder");
    assert_eq!(paths(&fixture.find(&legacy, "kelvorin")).len(), 1);
}
