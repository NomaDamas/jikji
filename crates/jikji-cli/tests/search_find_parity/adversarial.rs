use std::fs;

use super::fixture::{database_path, json_cmd, root_arg, run, temp_root};

#[test]
fn missing_stale_shell_noise_path_anchor_and_cjk_classes_are_covered() {
    let missing = temp_root("task5-missing");
    fs::write(missing.join("note.txt"), "ACME agreement").expect("write missing");
    let missing_arg = root_arg(&missing);
    let missing_output = run(&["find", &missing_arg, "ACME", "--json"]);
    assert!(missing_output.status.success());
    assert!(!missing.join(".jikji").exists());

    let root = temp_root("task5-adversarial");
    write_adversarial_fixture(&root);
    let root_arg = root_arg(&root);
    json_cmd(&["prepare", &root_arg, "--json"]);

    assert_path_anchor_contract(&root_arg);
    assert_cjk_contract(&root_arg);
    assert_shell_noise_contract(&root_arg);
    assert_stale_previous_index_contract(&root, &root_arg);
}

#[test]
fn corrupted_sqlite_search_index_fails_without_shell_noise_or_panic() {
    let root = temp_root("task5-corrupt-sqlite");
    fs::write(root.join("note.txt"), "ACME agreement").expect("write note");
    let root_arg = root_arg(&root);
    json_cmd(&["prepare", &root_arg, "--json"]);
    fs::write(database_path(&root), b"not a sqlite database").expect("corrupt sqlite");

    let output = run(&["find", &root_arg, "ACME", "--json"]);
    assert!(output.status.success());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).expect("failure json");
    assert_eq!(response["handoff_action"], "jikji_retry");
    assert_eq!(response["max_jikji_retries"], 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("rm -rf"));
}

fn write_adversarial_fixture(root: &std::path::Path) {
    fs::create_dir(root.join("financebench")).expect("create financebench");
    for name in [
        "PFIZER_2015_10K.pdf",
        "NETFLIX_2015_10K.pdf",
        "NETFLIX_2017_10K.pdf",
    ] {
        fs::write(
            root.join("financebench").join(name),
            "statement of income cash flows unadjusted EBITDA margin",
        )
        .expect("write finance");
    }
    fs::write(
        root.join("日本語調査資料.txt"),
        "東京都市計画資料 contains long span Japanese retrieval text",
    )
    .expect("write japanese");
    fs::write(root.join("photo.jpg"), "fake jpg metadata rm token").expect("write jpg");
}

fn assert_path_anchor_contract(root_arg: &str) {
    let anchored = json_cmd(&[
        "find",
        root_arg,
        "What is the FY2015 unadjusted EBITDA margin for Netflix?",
        "--top-k",
        "5",
        "--json",
    ]);
    assert_eq!(
        anchored["answer_paths"][0],
        "financebench/NETFLIX_2015_10K.pdf"
    );
}

fn assert_cjk_contract(root_arg: &str) {
    let cjk = json_cmd(&[
        "search",
        root_arg,
        "東京都市計画資料",
        "--top-k",
        "3",
        "--json",
    ]);
    assert_eq!(cjk["candidates"][0]["path"], "日本語調査資料.txt");
}

fn assert_shell_noise_contract(root_arg: &str) {
    let shell = json_cmd(&[
        "find",
        root_arg,
        "zzzznohit \"semi; rm -rf /\" $(echo nope)",
        "--top-k",
        "3",
        "--json",
    ]);
    assert_eq!(shell["confidence"], "low");
    assert_eq!(shell["handoff_action"], "jikji_retry");
    assert_eq!(shell["answerability"], "needs_one_jikji_retry");
    assert_eq!(shell["raw_fallback_allowed"], false);
    assert_eq!(shell["max_raw_fallback_commands"], 0);
    assert_eq!(shell["paths"].as_array().expect("paths").len(), 0);
    assert!(
        !shell["query_variants"]
            .to_string()
            .to_ascii_lowercase()
            .contains("rm")
    );
    assert!(shell["retry_proof"].as_str().expect("retry proof").len() >= 16);

    let forged = json_cmd(&[
        "find",
        root_arg,
        "zzzznohit \"semi; rm -rf /\" $(echo nope)",
        "--top-k",
        "3",
        "--after-jikji-retry",
        "--retry-proof",
        "forged",
        "--json",
    ]);
    assert_eq!(forged["handoff_action"], "jikji_retry");
    assert_eq!(forged["raw_fallback_allowed"], false);
}

fn assert_stale_previous_index_contract(root: &std::path::Path, root_arg: &str) {
    fs::write(root.join("new-file.txt"), "new content after prepare").expect("write changed");
    let stale = json_cmd(&[
        "search",
        root_arg,
        "ACME",
        "--stale-after-seconds",
        "-1",
        "--json",
    ]);
    assert_eq!(stale["index_status"], "ready");
    assert_eq!(stale["foreground_prepared"], true);
    assert_eq!(stale["empty_result_reindexed"], true);
}
