//! `jikji find` on folders that are not themselves indexed roots, and the explanations it gives
//! when nothing can be returned. Everything runs the real binary against an isolated data dir.

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
    /// An indexed root holding `pkg/src`, `pkg/test`, `other` and an empty `solo` folder.
    fn indexed() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let data = temp.path().join("data");
        for dir in [
            "pkg/src/deep",
            "pkg/test",
            "other",
            "solo",
            "with_under_score",
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("pkg/src/deep/up.ts"), "webchat attachment upload").unwrap();
        fs::write(root.join("pkg/test/up.test.ts"), "webchat upload test body").unwrap();
        fs::write(root.join("other/o.txt"), "webchat elsewhere").unwrap();
        fs::write(root.join("other/문서.txt"), "알파 계약서 본문").unwrap();
        fs::write(root.join("with_under_score/u.txt"), "webchat underscore").unwrap();
        fs::write(root.join("withXunderXscore.txt"), "webchat lookalike").unwrap();
        let fixture = Self {
            _temp: temp,
            root,
            data,
        };
        fixture.run(
            &[
                "prepare",
                fixture.root_str().as_str(),
                "--no-agent-rules",
                "--json",
            ],
            &[],
        );
        fixture
    }

    fn root_str(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Value {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jikji"));
        command.env("JIKJI_DATA_DIR", &self.data).args(args);
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "args={args:?} stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn find(&self, folder: &Path, query: &str, extra: &[&str]) -> Value {
        let folder = folder.to_string_lossy().into_owned();
        let mut args = vec![
            "find",
            folder.as_str(),
            query,
            "--json",
            "--no-background-refresh",
        ];
        args.extend_from_slice(extra);
        self.run(&args, &[])
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

fn note_codes(payload: &Value) -> Vec<String> {
    payload["notes"]
        .as_array()
        .map(|notes| {
            notes
                .iter()
                .map(|note| note["code"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_subfolder_of_an_indexed_root_is_answered_from_that_index_within_the_subfolder() {
    let fixture = Fixture::indexed();
    let found = fixture.find(&fixture.root.join("pkg"), "webchat", &[]);
    let mut got = paths(&found);
    got.sort();
    assert_eq!(got, ["src/deep/up.ts", "test/up.test.ts"], "{found}");
    assert_eq!(found["index_status"], "ready");
    assert_eq!(found["scope"], "pkg");
    assert_eq!(found["index_root"], fixture.root_str().as_str());
    assert!(note_codes(&found).contains(&"searched_enclosing_index".to_owned()));
    // Candidates and evidence use the same folder-relative paths as `paths`.
    for candidate in found["candidates"].as_array().unwrap() {
        assert!(
            !candidate["p"].as_str().unwrap().starts_with("pkg/"),
            "{candidate}"
        );
    }

    let deeper = fixture.find(&fixture.root.join("pkg/src"), "webchat upload", &[]);
    assert_eq!(paths(&deeper), ["deep/up.ts"], "{deeper}");

    let hangul = fixture.find(&fixture.root.join("other"), "계약서", &[]);
    assert_eq!(paths(&hangul), ["문서.txt"], "{hangul}");
}

#[test]
fn subfolder_scope_treats_like_wildcards_in_folder_names_literally() {
    let fixture = Fixture::indexed();
    let found = fixture.find(&fixture.root.join("with_under_score"), "webchat", &[]);
    assert_eq!(
        paths(&found),
        ["u.txt"],
        "`_` must not match any character: {found}"
    );
}

#[test]
fn a_subfolder_search_never_indexes_or_writes_anything() {
    let fixture = Fixture::indexed();
    let before = fs::read(fixture.data.join("jikji/index.sqlite"))
        .unwrap()
        .len();
    fixture.find(
        &fixture.root.join("pkg"),
        "nothing-matches-zzq",
        &["--fresh"],
    );
    let listing = |dir: &Path| {
        let mut names = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    assert!(!fixture.root.join("pkg/.jikji").exists());
    assert!(
        !listing(&fixture.root.join("pkg"))
            .iter()
            .any(|name| name.starts_with('.'))
    );
    let after = fs::read(fixture.data.join("jikji/index.sqlite"))
        .unwrap()
        .len();
    assert_eq!(
        before, after,
        "the index must not be rebuilt for a sub-folder search"
    );
}

#[test]
fn a_large_top_k_returns_results_instead_of_aborting() {
    let fixture = Fixture::indexed();
    for top_k in ["50", "100", "101", "500"] {
        let found = fixture.find(&fixture.root, "webchat", &["--top-k", top_k]);
        assert_eq!(paths(&found).len(), 5, "top-k {top_k}: {found}");
    }
}

#[test]
fn a_folder_without_any_index_explains_why_and_what_to_do() {
    let fixture = Fixture::indexed();
    let stranger = fixture._temp.path().join("stranger");
    fs::create_dir_all(stranger.join("inner")).unwrap();
    fs::write(stranger.join("a.txt"), "webchat").unwrap();
    let found = fixture.find(&stranger, "webchat", &[]);
    assert_eq!(found["index_status"], "missing");
    assert_eq!(found["reason"], "not_indexed");
    assert!(
        found["message"]
            .as_str()
            .unwrap()
            .contains("No Jikji index covers")
    );
    assert!(found["jikji_version"].is_string());
    let steps = found["next_steps"].as_array().unwrap();
    assert!(steps.iter().any(|step| {
        step.as_str()
            .unwrap()
            .contains("ask the user before indexing")
    }));
    assert!(
        !stranger.join(".jikji").exists(),
        "find must never index by itself"
    );

    // A parent of an indexed root lists the indexed folders inside it.
    let parent = fixture.find(fixture._temp.path(), "webchat", &[]);
    assert_eq!(parent["reason"], "not_indexed");
    assert_eq!(
        parent["indexed_roots_inside"][0],
        fixture.root_str().as_str()
    );
}

#[test]
fn zero_results_say_whether_nothing_matched_the_scope_is_empty_or_the_path_is_wrong() {
    let fixture = Fixture::indexed();
    let no_match = fixture.find(&fixture.root, "zzqqxxnothing", &[]);
    assert_eq!(no_match["empty_reason"], "no_match_in_index", "{no_match}");
    assert_eq!(no_match["indexed_files_searched"], 6);

    let empty_folder = fixture.find(&fixture.root.join("solo"), "webchat", &[]);
    assert_eq!(
        empty_folder["empty_reason"], "scope_has_no_indexed_files",
        "{empty_folder}"
    );
    let advice = empty_folder["notes"].to_string();
    assert!(
        advice.contains("jikji refresh") && advice.contains("ask the user first"),
        "{advice}"
    );

    let missing_folder = fixture.find(&fixture.root.join("no-such-folder"), "webchat", &[]);
    assert_eq!(
        missing_folder["empty_reason"], "path_not_found",
        "{missing_folder}"
    );
}

#[test]
fn pattern_queries_are_flagged_as_outside_what_jikji_evaluates() {
    let fixture = Fixture::indexed();
    let found = fixture.find(&fixture.root, "impl\\s+Foo<(a|b)>", &[]);
    assert!(
        note_codes(&found).contains(&"pattern_syntax_not_supported".to_owned()),
        "{found}"
    );
    assert!(paths(&found).is_empty());
    let plain = fixture.find(&fixture.root, "webchat upload", &[]);
    assert!(!note_codes(&plain).contains(&"pattern_syntax_not_supported".to_owned()));
}

#[test]
fn a_folder_too_large_to_compare_is_not_reindexed_in_the_foreground() {
    let fixture = Fixture::indexed();
    fs::write(fixture.root.join("fresh-file.txt"), "qznew997unique").unwrap();
    let root = fixture.root_str();
    let args = [
        "find",
        root.as_str(),
        "qznew997unique",
        "--json",
        "--no-background-refresh",
    ];
    let slow = fixture.run(&args, &[("JIKJI_FRESHNESS_BUDGET_MS", "0")]);
    assert_eq!(slow["empty_result_reindexed"], false, "{slow}");
    assert_eq!(slow["foreground_prepared"], false);
    let codes = note_codes(&slow);
    assert!(
        codes.contains(&"freshness_not_checked".to_owned()),
        "{slow}"
    );
    assert!(codes.contains(&"no_match_in_index".to_owned()));
    // With the normal budget the change is noticed and the empty result is retried once.
    let normal = fixture.run(&args, &[]);
    assert_eq!(normal["empty_result_reindexed"], true, "{normal}");
    assert_eq!(paths(&normal), ["fresh-file.txt"]);
}

#[test]
fn a_blank_query_is_explained_and_never_triggers_a_reindex() {
    let fixture = Fixture::indexed();
    fs::write(fixture.root.join("fresh-file.txt"), "qznew997unique").unwrap();
    for blank in ["", "   "] {
        let found = fixture.find(&fixture.root, blank, &[]);
        assert_eq!(found["empty_reason"], "empty_query", "{found}");
        assert_eq!(found["empty_result_reindexed"], false, "{found}");
        assert_eq!(found["foreground_prepared"], false, "{found}");
        assert!(note_codes(&found).contains(&"empty_query".to_owned()));
    }
}

#[test]
fn decomposed_and_composed_korean_file_names_find_each_other() {
    // macOS-synced folders store names decomposed (NFD); an agent types composed text (NFC).
    let fixture = Fixture::indexed();
    let nfd_name = format!("{}.txt", decompose_hangul("하저보 계약서"));
    fs::write(fixture.root.join(&nfd_name), "본문").unwrap();
    fixture.run(
        &[
            "prepare",
            fixture.root_str().as_str(),
            "--no-agent-rules",
            "--json",
        ],
        &[],
    );
    for query in [
        "하저보".to_owned(),
        "계약서".to_owned(),
        "하저보 계약서".to_owned(),
        decompose_hangul("하저보"),
    ] {
        let found = fixture.find(&fixture.root, &query, &[]);
        assert_eq!(paths(&found).first(), Some(&nfd_name), "{query}: {found}");
    }
}

/// Hangul syllable -> conjoining jamo (canonical decomposition), other characters unchanged.
fn decompose_hangul(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        let code = ch as u32;
        if (0xAC00..=0xD7A3).contains(&code) {
            let index = code - 0xAC00;
            out.push(char::from_u32(0x1100 + index / 588).unwrap());
            out.push(char::from_u32(0x1161 + (index % 588) / 28).unwrap());
            if index % 28 != 0 {
                out.push(char::from_u32(0x11A7 + index % 28).unwrap());
            }
        } else {
            out.push(ch);
        }
    }
    out
}
