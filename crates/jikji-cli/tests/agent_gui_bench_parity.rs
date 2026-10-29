#[path = "agent_gui_bench_parity/mod.rs"]
mod helpers;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
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
    let revealed = response_json(&reveal);
    assert!(
        Path::new(revealed["path"].as_str().expect("path")).ends_with("root"),
        "{reveal}"
    );
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
fn gui_find_keeps_indexed_hits_without_full_query_substring() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("root");
    fs::write(
        root.join("cascade_notes.txt"),
        "Adaptive Jikji discovery cascade for local retrieval.",
    )
    .expect("fixture");
    json_cmd(["prepare", path_str(&root).as_str(), "--json"]);

    let gui = GuiChild::start(&root);
    let found = response_json(&gui.get("/api/find?q=adaptive%20discovery%20cascade"));
    let candidates = found["candidates"].as_array().expect("candidates");
    assert!(
        !candidates.is_empty(),
        "GUI find should keep BM25 hits even when the full query is not a contiguous substring: {found}"
    );
    assert_eq!(candidates[0]["p"], "cascade_notes.txt");
    assert_eq!(candidates[0]["path"], "cascade_notes.txt");
    assert_ne!(found["answerability"], "no_match");
}

#[test]
fn gui_find_includes_hits_from_indexed_library_roots() {
    use std::process::Command;

    let temp = tempfile::tempdir().expect("tempdir");
    let data_dir = temp.path().join("data");
    let home = temp.path().join("home");
    let active = home.join("projects/app");
    let documents = home.join("Documents");
    fs::create_dir_all(&active).expect("active");
    fs::create_dir_all(&documents).expect("documents");
    fs::write(active.join("readme.txt"), "active workspace readme").expect("readme");
    fs::write(
        documents.join("library_invoice.txt"),
        "unique library invoice token zebra-cascade-42",
    )
    .expect("invoice");

    for root in [&active, &documents] {
        let output = Command::new(env!("CARGO_BIN_EXE_jikji"))
            .env("JIKJI_DATA_DIR", &data_dir)
            .args(["prepare", path_str(root).as_str(), "--json"])
            .output()
            .expect("prepare");
        assert!(
            output.status.success(),
            "prepare failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let gui = GuiChild::start_with_env(
        &active,
        &[
            ("JIKJI_DATA_DIR", data_dir.as_os_str()),
            ("JIKJI_POST_INSTALL_HOME", home.as_os_str()),
        ],
    );
    let found = response_json(&gui.get("/api/find?q=zebra-cascade-42"));
    let candidates = found["candidates"].as_array().expect("candidates");
    assert!(
        candidates.iter().any(|item| {
            item["p"] == "library_invoice.txt" || item["path"] == "library_invoice.txt"
        }),
        "library document should be searchable from a non-library GUI root: {found}"
    );
    let hit = candidates
        .iter()
        .find(|item| item["p"] == "library_invoice.txt" || item["path"] == "library_invoice.txt")
        .expect("hit");
    let root = hit["root"].as_str().expect("root");
    assert!(
        Path::new(root).ends_with("Documents"),
        "candidate root should be the Documents library folder, got {root}"
    );
}

#[test]
fn gui_find_keeps_nested_drive_root_when_parent_is_indexed() {
    use std::process::Command;

    let temp = tempfile::tempdir().expect("tempdir");
    let data_dir = temp.path().join("data");
    let home = temp.path().join("home");
    let active = home.join("projects/app");
    let drive = home.join("GoogleDrive");
    let nested = drive.join("마커");
    fs::create_dir_all(&active).expect("active");
    fs::create_dir_all(&nested).expect("nested drive");
    fs::write(active.join("readme.txt"), "active workspace readme").expect("readme");
    fs::write(drive.join("top-note.txt"), "parent drive token").expect("parent note");
    fs::write(
        nested.join("nested-note.txt"),
        "unique nested drive token nested-drive-token-99",
    )
    .expect("nested note");

    for root in [&active, &drive, &nested] {
        let output = Command::new(env!("CARGO_BIN_EXE_jikji"))
            .env("JIKJI_DATA_DIR", &data_dir)
            .args(["prepare", path_str(root).as_str(), "--json"])
            .output()
            .expect("prepare");
        assert!(
            output.status.success(),
            "prepare failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let gui = GuiChild::start_with_env(
        &active,
        &[
            ("JIKJI_DATA_DIR", data_dir.as_os_str()),
            ("JIKJI_POST_INSTALL_HOME", home.as_os_str()),
        ],
    );
    let found = response_json(&gui.get("/api/find?q=nested-drive-token-99"));
    let candidates = found["candidates"].as_array().expect("candidates");
    assert!(
        candidates.iter().any(|item| {
            let nested_hit = item["p"] == "nested-note.txt" || item["path"] == "nested-note.txt";
            let nested_root = item["root"]
                .as_str()
                .is_some_and(|root| Path::new(root).ends_with("마커"));
            nested_hit && nested_root
        }),
        "nested GoogleDrive child root must stay searchable after parent is indexed: {found}"
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

#[test]
fn gui_refuses_unauthenticated_visitors_and_sends_security_headers() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("root");
    fs::write(root.join("secret.txt"), "private body").expect("fixture");
    let gui = GuiChild::start(&root);

    let page = gui.get_anonymous("/");
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    assert!(
        page.contains("/api/login"),
        "anonymous visitors only get the login page"
    );
    assert!(
        !page.contains("panel-overview"),
        "console markup must not be served without login"
    );
    for header in [
        "Content-Security-Policy: default-src 'self'",
        "X-Frame-Options: DENY",
        "X-Content-Type-Options: nosniff",
        "Referrer-Policy: no-referrer",
    ] {
        assert!(page.contains(header), "missing {header}");
    }
    for path in [
        "/api/status",
        "/api/roots",
        "/api/files",
        "/api/find?q=private",
        "/api/preview?path=secret.txt",
        "/api/preview/file?path=secret.txt",
        "/download?path=secret.txt",
    ] {
        let response = gui.get_anonymous(path);
        assert!(response.starts_with("HTTP/1.1 401"), "{path}: {response}");
        assert!(
            !response.contains("private body"),
            "{path} leaked file content"
        );
    }
    let console = gui.get("/");
    assert!(
        console.contains("panel-overview"),
        "logged-in session gets the console"
    );
}

#[test]
fn gui_preview_reports_a_kind_for_every_format() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("root");
    fs::write(root.join("note.md"), "# heading\nbody").expect("text");
    fs::write(root.join("big.log"), "line\n".repeat(80_000)).expect("big text");
    fs::write(root.join("pic.png"), b"\x89PNG\r\n\x1a\n").expect("png");
    fs::write(
        root.join("logo.svg"),
        "<svg xmlns='http://www.w3.org/2000/svg'/>",
    )
    .expect("svg");
    fs::write(root.join("song.mp3"), [0u8; 64]).expect("mp3");
    fs::write(
        root.join("clip.mp4"),
        (0u8..=255).cycle().take(4096).collect::<Vec<_>>(),
    )
    .expect("mp4");
    fs::write(root.join("bundle.zip"), b"PK\x03\x04").expect("zip");
    fs::write(root.join("blob.bin"), [0, 1, 2, 3]).expect("bin");
    let gui = GuiChild::start(&root);

    for (path, kind) in [
        ("note.md", "text"),
        ("big.log", "text"),
        ("pic.png", "image"),
        ("logo.svg", "image"),
        ("song.mp3", "audio"),
        ("clip.mp4", "video"),
        ("bundle.zip", "none"),
        ("blob.bin", "none"),
    ] {
        let preview = response_json(&gui.get(&format!("/api/preview?path={path}")));
        assert_eq!(preview["kind"], kind, "{path}: {preview}");
    }
    let big = response_json(&gui.get("/api/preview?path=big.log"));
    assert_eq!(big["truncated"], true, "large text is cut, not refused");
    assert_eq!(big["content"].as_str().expect("content").len(), 256 * 1024);
    let zip = response_json(&gui.get("/api/preview?path=bundle.zip"));
    assert_eq!(zip["reason"], "archive");

    let range = gui.get_with_header("/api/preview/file?path=clip.mp4", "Range: bytes=100-199");
    assert!(range.starts_with("HTTP/1.1 206"), "{range}");
    assert!(
        range.contains("Content-Range: bytes 100-199/4096"),
        "{range}"
    );
    assert!(range.contains("Content-Length: 100"), "{range}");
    let whole = gui.get("/api/preview/file?path=clip.mp4");
    assert!(
        whole.starts_with("HTTP/1.1 200") && whole.contains("Accept-Ranges: bytes"),
        "{whole}"
    );
    let past_end = gui.get_with_header("/api/preview/file?path=clip.mp4", "Range: bytes=5000-");
    assert!(past_end.starts_with("HTTP/1.1 416"), "{past_end}");
}

#[test]
fn gui_document_preview_uses_parsed_text_and_rejects_binary_endpoints() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("root");
    fs::write(
        root.join("plan_v0.1.final.docx"),
        minimal_docx("😀가 Contract Needle body"),
    )
    .expect("docx");
    let gui = GuiChild::start(&root);
    let preview = response_json(&gui.get("/api/preview?path=plan_v0.1.final.docx&q=needle"));
    assert_eq!(preview["kind"], "document", "{preview}");
    assert_eq!(preview["source"], "parsed_now", "{preview}");
    assert!(
        preview["content"]
            .as_str()
            .expect("content")
            .contains("Contract Needle body")
    );
    assert_eq!(preview["matches"][0]["start"], 13);
    assert_eq!(preview["matches"][0]["end"], 19);
    assert!(
        gui.get("/api/preview/file?path=plan_v0.1.final.docx")
            .starts_with("HTTP/1.1 415")
    );
    assert!(
        gui.get("/api/preview/pages?path=plan_v0.1.final.docx")
            .starts_with("HTTP/1.1 415")
    );
    assert!(
        gui.get("/api/preview/page?path=plan_v0.1.final.docx&page=1")
            .starts_with("HTTP/1.1 415")
    );
}

/// Minimal valid DOCX (Office Open XML) holding `text` as one paragraph.
fn minimal_docx(text: &str) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut bytes = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut bytes);
    let options = SimpleFileOptions::default();
    let parts = [
        ("[Content_Types].xml", r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.to_owned()),
        ("_rels/.rels", r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_owned()),
        ("word/document.xml", format!(r#"<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#)),
    ];
    for (name, body) in parts {
        zip.start_file(name, options).expect("zip entry");
        zip.write_all(body.as_bytes()).expect("zip body");
    }
    zip.finish().expect("zip finish");
    bytes.into_inner()
}

#[test]
fn gui_find_and_snippet_use_indexed_docx_body_without_cache_headers() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("root");
    let late_body = format!("{} needlepreview2026 body marker", "intro ".repeat(100));
    fs::write(root.join("report.docx"), minimal_docx(&late_body)).expect("docx");
    fs::write(root.join("notes.md"), "# notes\nplain markdown body marker").expect("md");
    fs::write(root.join("blob.bin"), [0u8, 1, 2, 3, 255]).expect("bin");
    json_cmd(["prepare", path_str(&root).as_str(), "--json"]);
    let gui = GuiChild::start(&root);
    let found = response_json(&gui.get("/api/find?q=needlepreview2026"));
    let snippet = found["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .find(|candidate| candidate["p"] == "report.docx")
        .expect("docx candidate")["preview_snippet"]
        .as_str()
        .expect("preview snippet");
    assert!(
        snippet.contains("needlepreview2026 body marker"),
        "{snippet}"
    );
    assert!(
        !snippet.contains("# Source:") && !snippet.contains("# File ID:"),
        "{snippet}"
    );
    assert!(snippet.chars().count() <= 242, "{snippet}");
    let docx = response_json(&gui.get("/api/snippet?path=report.docx&q=NEEDLEPREVIEW2026"));
    assert!(
        docx["text"]
            .as_str()
            .expect("text")
            .contains("needlepreview2026 body marker")
    );
    assert_eq!(docx["matches"].as_array().expect("matches").len(), 1);
    assert!(!docx["text"].as_str().expect("text").contains("# Source:"));
    fs::remove_file(root.join("report.docx")).expect("remove");
    let moved = response_json(&gui.get("/api/snippet?path=report.docx"));
    assert!(
        moved["text"]
            .as_str()
            .expect("text")
            .contains("body marker"),
        "{moved}"
    );
    assert_rejected(&gui.get("/api/snippet?path=../outside.txt"));
    assert!(
        gui.get_anonymous("/api/snippet?path=notes.md")
            .starts_with("HTTP/1.1 401")
    );
}

#[test]
fn gui_pdf_pages_stay_native_and_malformed_documents_fail_gracefully() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("root");
    fs::write(root.join("broken.docx"), b"not a zip").expect("broken docx");
    fs::write(root.join("native.pdf"), minimal_pdf()).expect("pdf");
    let gui = GuiChild::start(&root);
    let broken = response_json(&gui.get("/api/preview?path=broken.docx"));
    assert_eq!(broken["kind"], "document", "{broken}");
    assert_eq!(broken["content"], "", "{broken}");
    assert!(broken["reason"].is_string(), "{broken}");
    let pdf = response_json(&gui.get("/api/preview?path=native.pdf"));
    assert_eq!(pdf["kind"], "pdf", "{pdf}");
    let file = gui.get("/api/preview/file?path=native.pdf");
    assert!(
        file.starts_with("HTTP/1.1 200") && file.contains("Content-Type: application/pdf"),
        "{file}"
    );
    let pages = response_json(&gui.get("/api/preview/pages?path=native.pdf"));
    assert_eq!(pages["converter"], "native-pdf", "{pages}");
    assert_eq!(pages["pages"], 1, "{pages}");
    assert_rejected(&gui.get("/api/preview/pages?path=../outside.pdf"));
}

fn minimal_pdf() -> Vec<u8> {
    let mut body = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for object in [
        "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n",
    ] {
        offsets.push(body.len());
        body.extend_from_slice(object.as_bytes());
    }
    let xref = body.len();
    body.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for offset in offsets {
        body.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    body.extend_from_slice(
        format!("trailer\n<< /Root 1 0 R /Size 4 >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    body
}

fn response_json(response: &str) -> serde_json::Value {
    let (_, body) = response.split_once("\r\n\r\n").expect("HTTP body");
    serde_json::from_str(body).expect("JSON response")
}
