//! `jikji find` says what kind of request it served and what it searched: per-result file kind and
//! match place, name-only labelling for non-text files, and grep/read guidance for exact-text
//! requests. Everything runs the real binary against an isolated data dir.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

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
        for dir in ["docs", "src", "사진/제주", "media"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(
            root.join("docs/plan.md"),
            "quarterly kelvorin budget renewal",
        )
        .unwrap();
        let filler = "alpha beta gamma delta ".repeat(300);
        fs::write(
            root.join("docs/long_contract.txt"),
            format!("{filler}the zyxwvut clause requires two months notice. {filler}"),
        )
        .unwrap();
        fs::write(
            root.join("docs/임대차 계약.txt"),
            "서론입니다. 보증금 반환 시기는 퇴거 후 한 달 이내로 한다. 끝",
        )
        .unwrap();
        fs::write(root.join("src/util.rs"), "pub fn rotateAccessToken() {}\n").unwrap();
        fs::write(
            root.join("src/config.rs"),
            "fn parse_settings_file() -> u8 { 1 }\n",
        )
        .unwrap();
        fs::write(
            root.join("사진/제주/한라산 일출 2024.jpg"),
            [0xFF, 0xD8, 0xFF, 0xE0, 1, 2],
        )
        .unwrap();
        fs::write(
            root.join("media/standup_recording.mp3"),
            [0x49, 0x44, 0x33, 3, 0, 0],
        )
        .unwrap();
        fs::write(
            root.join("media/product_demo.mp4"),
            [0, 0, 0, 0x18, b'f', b't', b'y', b'p'],
        )
        .unwrap();
        let fixture = Self {
            _temp: temp,
            root,
            data,
        };
        fixture.run(&[
            "prepare",
            &fixture.root.to_string_lossy(),
            "--no-agent-rules",
            "--json",
        ]);
        fixture
    }

    fn run(&self, args: &[&str]) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_jikji"))
            .env("JIKJI_DATA_DIR", &self.data)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "args={args:?} stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn find(&self, folder: &Path, query: &str) -> Value {
        let folder = folder.to_string_lossy().into_owned();
        self.run(&["find", &folder, query, "--json", "--no-background-refresh"])
    }
}

fn codes(payload: &Value) -> Vec<String> {
    payload["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|note| note["code"].as_str().unwrap().to_owned())
        .collect()
}

fn result_for<'a>(payload: &'a Value, path: &str) -> &'a Value {
    payload["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|result| result["path"] == path)
        .unwrap_or_else(|| panic!("no result for {path}: {payload}"))
}

#[test]
fn a_document_found_by_its_text_is_a_content_match_with_no_warning() {
    let fixture = Fixture::new();
    let found = fixture.find(&fixture.root, "kelvorin budget");
    assert_eq!(found["paths"][0], "docs/plan.md");
    let result = result_for(&found, "docs/plan.md");
    assert_eq!(result["file_kind"], "text");
    assert_eq!(result["text_indexed"], true);
    assert_eq!(result["matched_in"], serde_json::json!(["text"]));
    assert_eq!(found["request_plan"]["intent"], "files_by_name_or_text");
    assert!(codes(&found).is_empty(), "{found}");
}

#[test]
fn a_content_hit_carries_the_passage_around_the_match_not_the_file_start() {
    let fixture = Fixture::new();
    let found = fixture.find(&fixture.root, "zyxwvut");
    assert_eq!(found["paths"][0], "docs/long_contract.txt", "{found}");
    let snippet = result_for(&found, "docs/long_contract.txt")["snippet"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        snippet.contains("zyxwvut clause requires two months notice"),
        "{snippet}"
    );
    assert!(snippet.chars().count() < 260);

    let korean = fixture.find(&fixture.root.join("docs"), "보증금 퇴거");
    assert_eq!(korean["paths"][0], "임대차 계약.txt", "{korean}");
    let passage = result_for(&korean, "임대차 계약.txt")["snippet"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(passage.contains("보증금 반환 시기는 퇴거 후"), "{passage}");

    // A file matched only by its name has no passage to show.
    let by_name = fixture.find(&fixture.root, "standup recording");
    assert!(result_for(&by_name, "media/standup_recording.mp3")["snippet"].is_null());
}

#[test]
fn media_is_found_by_name_and_never_presented_as_a_content_match() {
    let fixture = Fixture::new();
    for (query, path, kind) in [
        ("한라산 일출", "사진/제주/한라산 일출 2024.jpg", "image"),
        ("standup recording", "media/standup_recording.mp3", "audio"),
        ("product demo", "media/product_demo.mp4", "video"),
    ] {
        let found = fixture.find(&fixture.root, query);
        assert_eq!(found["paths"][0], path, "{query}: {found}");
        let result = result_for(&found, path);
        assert_eq!(result["file_kind"], kind);
        assert_eq!(result["text_indexed"], false);
        assert_eq!(result["matched_in"], serde_json::json!(["name_path"]));
        assert!(
            codes(&found).contains(&"non_text_name_match_only".to_owned()),
            "{query}: {found}"
        );
        assert_eq!(found["request_plan"]["intent"], "non_text_files");
    }
}

#[test]
fn asking_for_what_a_photo_shows_explains_that_media_content_is_not_searched() {
    let fixture = Fixture::new();
    let found = fixture.find(&fixture.root, "sunrise over mountain");
    assert!(found["paths"].as_array().unwrap().is_empty());
    let codes = codes(&found);
    assert!(
        codes.contains(&"non_text_content_not_searchable".to_owned()),
        "{found}"
    );
    assert_eq!(found["empty_reason"], "no_match_in_index");
    assert_eq!(found["coverage"]["by_kind"]["image"]["files"], 1);
    assert_eq!(found["coverage"]["by_kind"]["image"]["text_indexed"], 0);
}

#[test]
fn a_code_identifier_returns_files_and_says_lines_need_grep() {
    let fixture = Fixture::new();
    let found = fixture.find(&fixture.root, "parse_settings_file");
    assert_eq!(found["paths"][0], "src/config.rs");
    assert_eq!(found["request_plan"]["intent"], "code_identifier");
    assert!(codes(&found).contains(&"identifier_files_only".to_owned()));
    assert_eq!(result_for(&found, "src/config.rs")["file_kind"], "code");

    // Defined inside a file with another name, in camelCase.
    let camel = fixture.find(&fixture.root, "rotateAccessToken");
    assert_eq!(camel["paths"][0], "src/util.rs");
    assert_eq!(camel["request_plan"]["intent"], "code_identifier");
    assert!(
        result_for(&camel, "src/util.rs")["snippet"]
            .as_str()
            .unwrap()
            .contains("rotateAccessToken")
    );
}

#[test]
fn regex_code_and_line_requests_are_pointed_at_the_right_tool() {
    let fixture = Fixture::new();
    let regex = fixture.find(&fixture.root, "kelv\\w+n");
    assert_eq!(regex["request_plan"]["intent"], "exact_text");
    assert_eq!(regex["request_plan"]["use_instead"]["tool"], "grep");
    let command = regex["request_plan"]["use_instead"]["command"]
        .as_str()
        .unwrap();
    assert!(
        command.starts_with("grep -rnE -e 'kelv\\w+n' '"),
        "{command}"
    );
    assert!(codes(&regex).contains(&"pattern_syntax_not_supported".to_owned()));

    let code = fixture.find(&fixture.root, "fn parse_settings_file(");
    assert!(
        code["request_plan"]["use_instead"]["command"]
            .as_str()
            .unwrap()
            .starts_with("grep -rnF")
    );

    let line = fixture.find(&fixture.root, "config.rs:1");
    assert_eq!(line["request_plan"]["use_instead"]["tool"], "read");
    assert_eq!(
        line["request_plan"]["use_instead"]["narrow_to"][0],
        "src/config.rs"
    );
}

#[test]
fn a_subfolder_search_reports_result_paths_relative_to_the_asked_folder() {
    let fixture = Fixture::new();
    let found = fixture.find(&fixture.root.join("사진"), "한라산");
    assert_eq!(found["paths"][0], "제주/한라산 일출 2024.jpg");
    let result = result_for(&found, "제주/한라산 일출 2024.jpg");
    assert_eq!(result["file_kind"], "image");
    assert_eq!(found["scope"], "사진");
}

#[test]
fn repeating_a_request_gives_the_same_plan_and_results() {
    let fixture = Fixture::new();
    let first = fixture.find(&fixture.root, "product demo");
    let second = fixture.find(&fixture.root, "product demo");
    for key in ["paths", "results", "request_plan", "notes"] {
        assert_eq!(first[key], second[key], "{key}");
    }
}

#[test]
fn each_result_lists_which_of_the_request_words_it_contains() {
    let fixture = Fixture::new();
    // `kelvorin` is in plan.md and `zyxwvut` in long_contract.txt; no file has both.
    let split = fixture.find(&fixture.root, "kelvorin zyxwvut");
    assert_eq!(
        split["request_plan"]["query_words"],
        serde_json::json!(["kelvorin", "zyxwvut"])
    );
    assert_eq!(
        result_for(&split, "docs/plan.md")["matched_words"],
        serde_json::json!(["kelvorin"])
    );
    assert_eq!(
        result_for(&split, "docs/long_contract.txt")["matched_words"],
        serde_json::json!(["zyxwvut"])
    );
    let whole = fixture.find(&fixture.root, "kelvorin budget renewal");
    assert_eq!(
        result_for(&whole, "docs/plan.md")["matched_words"],
        serde_json::json!(["kelvorin", "budget", "renewal"])
    );
    // Korean words match through their stem and the passage shows where.
    let korean = fixture.find(&fixture.root, "보증금을 퇴거");
    assert_eq!(
        result_for(&korean, "docs/임대차 계약.txt")["matched_words"],
        serde_json::json!(["보증금을", "퇴거"])
    );
}
