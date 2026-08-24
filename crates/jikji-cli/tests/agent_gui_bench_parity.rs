#[path = "agent_gui_bench_parity/mod.rs"]
mod helpers;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use helpers::{GuiChild, assert_rejected, json_cmd, path_str, run_fail, run_ok};

#[test]
fn task6_public_agent_and_benchmark_commands_match_contract() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("root");
    fs::write(root.join("ACME_contract.txt"), "ACME payment contract").expect("fixture");

    let help = run_ok(["--help"]);
    let help_text = String::from_utf8(help.stdout).expect("help utf8");
    for command in [
        "agent-skill-install",
        "codex-skill-install",
        "skill-export",
        "gui",
        "eval-generate",
        "eval",
        "bench-analyze",
        "bench-run",
        "beir-import",
        "edith-suite",
        "hardbench-build",
    ] {
        assert!(
            help_text.contains(command),
            "missing help command {command}"
        );
    }

    let skill_dest = temp.path().join("agent/skills/jikji/SKILL.md");
    let installed = json_cmd([
        "agent-skill-install",
        "--agent",
        "codex",
        "--dest",
        path_str(&skill_dest).as_str(),
        "--no-prepare",
        "--json",
    ]);
    assert_eq!(installed["installed_any"], true);
    assert!(skill_dest.exists());

    let exported = json_cmd(["skill-export", "--json"]);
    assert!(
        exported["skill_markdown"]
            .as_str()
            .expect("skill markdown")
            .contains("Never move, rename, delete, or reorganize")
    );

    json_cmd(["prepare", path_str(&root).as_str(), "--json"]);
    for args in [
        vec![
            "eval-generate".to_owned(),
            path_str(&root),
            "--cases".to_owned(),
            "3".to_owned(),
            "--json".to_owned(),
        ],
        vec!["eval".to_owned(), path_str(&root), "--json".to_owned()],
        vec![
            "bench-analyze".to_owned(),
            path_str(&root),
            "--json".to_owned(),
        ],
        vec!["bench-run".to_owned(), path_str(&root), "--json".to_owned()],
        vec![
            "beir-import".to_owned(),
            path_str(&temp.path().join("beir")),
            "--cases".to_owned(),
            "1".to_owned(),
            "--no-fetch".to_owned(),
            "--json".to_owned(),
        ],
    ] {
        let output = run_ok(args);
        assert!(
            output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn task6_gui_management_token_protects_root_and_refresh() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root1 = temp.path().join("root1");
    let root2 = temp.path().join("root2");
    fs::create_dir(&root1).expect("root1");
    fs::create_dir(&root2).expect("root2");
    fs::write(root1.join("ACME_contract.txt"), "ACME payment contract").expect("fixture1");
    fs::write(root2.join("BETA_notes.txt"), "BETA migration memo").expect("fixture2");
    json_cmd(["prepare", path_str(&root1).as_str(), "--json"]);
    json_cmd(["prepare", path_str(&root2).as_str(), "--json"]);

    let gui = GuiChild::start(&root1);

    let unauthorized_root = gui.post(&format!("/api/root?path={}", path_str(&root2)));
    let unauthorized_refresh = gui.post("/api/refresh");
    assert_rejected(&unauthorized_root);
    assert_rejected(&unauthorized_refresh);

    let switch = gui.post(&format!(
        "/api/root?path={}&token={}",
        path_str(&root2),
        gui.manage_token()
    ));
    assert!(switch.starts_with("HTTP/1.1 200 OK"), "{switch}");

    let status = gui.get("/api/status");
    let search = gui.get("/api/search?q=BETA");
    assert!(status.starts_with("HTTP/1.1 200 OK"), "{status}");
    assert!(status.contains("root2"), "{status}");
    assert!(search.starts_with("HTTP/1.1 200 OK"), "{search}");
    assert!(search.contains("BETA_notes.txt"), "{search}");
}

#[test]
fn task6_gui_download_rejects_traversal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("root");
    fs::write(root.join("ACME_contract.txt"), "ACME payment contract").expect("fixture");
    json_cmd(["prepare", path_str(&root).as_str(), "--json"]);

    let gui = GuiChild::start(&root);
    let traversal = gui.get("/download?path=../outside.txt");

    assert_rejected(&traversal);
}

#[test]
fn task6_gui_open_and_reveal_protect_paths_and_token() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("root");
    fs::write(root.join("ACME_contract.txt"), "ACME payment contract").expect("fixture");
    json_cmd(["prepare", path_str(&root).as_str(), "--json"]);

    let gui = GuiChild::start(&root);
    assert_rejected(&gui.post("/open?path=ACME_contract.txt"));
    assert_rejected(&gui.post(&format!(
        "/open?path=../outside.txt&token={}",
        gui.manage_token()
    )));
    assert_rejected(&gui.post(&format!(
        "/reveal?path=missing.txt&token={}",
        gui.manage_token()
    )));

    let open = gui.post(&format!(
        "/open?path=ACME_contract.txt&token={}",
        gui.manage_token()
    ));
    assert!(open.starts_with("HTTP/1.1 200 OK"), "{open}");
    assert!(open.contains("ACME_contract.txt"), "{open}");

    let reveal = gui.post(&format!(
        "/reveal?path=ACME_contract.txt&token={}",
        gui.manage_token()
    ));
    assert!(reveal.starts_with("HTTP/1.1 200 OK"), "{reveal}");
    assert!(reveal.contains(root.to_string_lossy().as_ref()), "{reveal}");
}

#[test]
fn workspacebench_and_hardbench_require_real_adapters() {
    let temp = tempfile::tempdir().expect("tempdir");
    for command in [
        "workspacebench-build",
        "workspacebench-suite",
        "hardbench-build",
        "hardbench-suite",
    ] {
        let destination = temp.path().join(command);
        let output = run_fail([
            command,
            path_str(&destination).as_str(),
            "--no-fetch",
            "--json",
        ]);
        assert!(!output.status.success(), "{command} unexpectedly succeeded");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("requires an actual URL adapter"),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn hardbench_rejects_unknown_difficulty_before_download() {
    let temp = tempfile::tempdir().expect("tempdir");
    let destination = temp.path().join("hardbench");
    let output = run_fail([
        "hardbench-build",
        path_str(&destination).as_str(),
        "--difficulty",
        "impossible",
        "--base-url",
        "http://127.0.0.1:1",
        "--json",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unsupported hardbench difficulty"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn workspacebench_suite_uses_downloaded_endpoint_data() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind fixture endpoint");
    let address = listener.local_addr().expect("fixture address");
    let server = thread::spawn(move || {
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 4096];
            let count = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..count]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            let (content_type, body): (&str, &[u8]) = match path {
                "/api" => (
                    "application/json",
                    br#"{"siblings":[{"rfilename":"task_lite_clean_en/9/metadata.json"}]}"#,
                ),
                "/resolve/task_lite_clean_en/9/metadata.json" => (
                    "application/json",
                    br#"{"absolute_id":9,"persona":"analyst","task":"find revenue input","data_manifest":[{"filename":"revenue.txt","stored_relpath":"finance/revenue.txt"}],"file_dep_graph":[{"from":"revenue.txt"}]}"#,
                ),
                "/resolve/task_lite_clean_en/9/finance/revenue.txt" => {
                    ("text/plain", b"revenue input for 2026")
                }
                _ => ("text/plain", b"missing"),
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).expect("headers");
            stream.write_all(body).expect("body");
        }
    });
    let temp = tempfile::tempdir().expect("tempdir");
    let destination = temp.path().join("workspacebench");
    let output = run_ok([
        "workspacebench-suite",
        path_str(&destination).as_str(),
        "--cases",
        "1",
        "--base-url",
        format!("http://{address}").as_str(),
        "--json",
    ]);
    server.join().expect("server");
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).expect("suite json");
    assert_eq!(payload["build"]["tasks"], 1);
    assert_eq!(payload["build"]["files_materialized"], 1);
    assert!(payload["benchmark_report"].as_str().is_some());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(destination.join("manifest.json")).expect("manifest")
        )
        .expect("manifest json")["network"],
        "downloaded"
    );
}

#[test]
fn gui_explorer_lists_current_root_entries_with_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("source");
    fs::write(root.join("README.txt"), "project overview\n").expect("readme");
    json_cmd(["prepare", path_str(&root).as_str(), "--json"]);

    let gui = GuiChild::start(&root);
    let root_listing = response_json(&gui.get("/api/files"));
    assert_eq!(root_listing["path"], ".");
    let entries = root_listing["entries"].as_array().expect("entries");
    assert_eq!(entries[0]["name"], "src");
    let readme = entries
        .iter()
        .find(|entry| entry["name"] == "README.txt")
        .expect("README entry");
    assert_eq!(readme["type"], "file");
    assert_eq!(readme["status"], "current");
    assert!(readme["size"].as_u64().expect("size") > 0);
    assert!(readme["mtime"].as_u64().is_some());

    let nested = response_json(&gui.get("/api/files?path=src"));
    assert_eq!(nested["path"], "src");
    assert_eq!(nested["entries"][0]["path"], "src/main.rs");
}

#[test]
fn gui_find_candidates_include_preview_snippets() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("root");
    fs::write(
        root.join("ACME_contract.txt"),
        "ACME payment terms are net thirty days and renewable annually.",
    )
    .expect("fixture");
    json_cmd(["prepare", path_str(&root).as_str(), "--json"]);

    let gui = GuiChild::start(&root);
    let found = response_json(&gui.get("/api/find?q=payment"));
    assert_eq!(found["mode"], "find");
    assert_eq!(found["answer_pack_version"], 1);
    assert_eq!(found["candidates"][0]["p"], "ACME_contract.txt");
    assert_eq!(found["candidates"][0]["path"], "ACME_contract.txt");
    assert!(found["candidates"][0]["score"].as_f64().is_some());
    assert!(
        found["candidates"][0]["preview_snippet"]
            .as_str()
            .expect("snippet")
            .contains("payment terms")
    );
}

#[test]
fn gui_text_and_code_preview_return_unicode_highlight_ranges() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("root");
    fs::write(root.join("notes.txt"), "😀가 needle and needle").expect("text");
    fs::write(root.join("main.rs"), "fn needle() {}\n").expect("code");
    let gui = GuiChild::start(&root);

    let text = response_json(&gui.get("/api/preview?path=notes.txt&q=needle"));
    assert_eq!(text["supported"], true);
    assert_eq!(text["encoding"], "utf-8");
    assert_eq!(text["match_unit"], "utf16_code_unit");
    assert_eq!(text["matches"][0]["start"], 4);
    assert_eq!(text["matches"][0]["end"], 10);
    assert_eq!(text["matches"][1]["start"], 15);
    assert_eq!(text["matches"][1]["end"], 21);

    let code = response_json(&gui.get("/api/preview?path=main.rs&q=needle"));
    assert_eq!(code["supported"], true);
    assert!(
        code["content"]
            .as_str()
            .expect("code")
            .contains("fn needle")
    );
    assert_eq!(code["matches"][0]["start"], 3);
}

#[test]
fn gui_preview_binary_fallback_and_traversal_are_controlled() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("root");
    fs::write(root.join("image.bin"), [0, 159, 146, 150]).expect("binary");
    let gui = GuiChild::start(&root);

    let binary = response_json(&gui.get("/api/preview?path=image.bin"));
    assert_eq!(binary["supported"], false);
    assert_eq!(binary["reason"], "binary");
    assert_eq!(binary["size"], 4);
    assert!(binary.get("content").is_none());

    assert_rejected(&gui.get("/api/preview?path=../outside.txt"));
    assert_rejected(&gui.get("/api/files?path=.."));
    assert_rejected(&gui.get("/api/preview?path=%2e%2e%2foutside.txt"));
}

fn response_json(response: &str) -> serde_json::Value {
    let (_, body) = response.split_once("\r\n\r\n").expect("HTTP body");
    serde_json::from_str(body).expect("JSON response")
}
