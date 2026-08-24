use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

#[test]
fn gui_root_management_is_token_protected_isolated_and_complete() {
    let temp = tempfile::tempdir().expect("tempdir");
    let data_dir = temp.path().join("data");
    let root1 = temp.path().join("root1");
    let root2 = temp.path().join("root2");
    fs::create_dir_all(&root1).expect("root1");
    fs::create_dir_all(&root2).expect("root2");
    fs::write(root1.join("alpha.txt"), "alpha root marker").expect("alpha");
    fs::write(root2.join("beta.txt"), "beta root marker").expect("beta");

    prepare(&root1, &data_dir);
    let gui = GuiChild::start(&root1, &data_dir);

    for endpoint in [
        "/api/root?path=/tmp",
        "/api/refresh",
        "/api/reindex",
        "/api/deep-index",
        "/api/remove-root?path=/tmp",
    ] {
        assert_status(&gui.post(endpoint), 403);
    }

    let roots = response_json(&gui.get("/api/roots"), 200);
    assert_eq!(roots["active_root"], path_value(&root1));
    assert_eq!(roots["roots"].as_array().expect("roots").len(), 1);
    assert_eq!(roots["roots"][0]["statistics"]["files"], 1);

    let switched = response_json(
        &gui.post(&format!(
            "/api/root?path={}&prepare=true&token={}",
            root2.display(),
            gui.token
        )),
        200,
    );
    assert_eq!(switched["root"], path_value(&root2));
    assert_eq!(switched["statistics"]["files"], 1);

    let roots = response_json(&gui.get("/api/roots"), 200);
    let listed = roots["roots"].as_array().expect("roots");
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().any(|root| root["root"] == path_value(&root1)));
    assert!(listed.iter().any(|root| root["root"] == path_value(&root2)));

    let alpha_search = response_json(&gui.get("/api/search?q=alpha"), 200);
    assert!(
        alpha_search["candidates"]
            .as_array()
            .expect("candidates")
            .is_empty()
    );
    let beta_search = response_json(&gui.get("/api/search?q=beta"), 200);
    assert!(
        beta_search["candidates"]
            .as_array()
            .expect("candidates")
            .iter()
            .any(|row| { row.to_string().contains("beta.txt") })
    );

    fs::write(root2.join("gamma.txt"), "gamma added after switch").expect("gamma");
    let refreshed = response_json(&gui.post(&format!("/api/refresh?token={}", gui.token)), 200);
    assert_eq!(refreshed["statistics"]["files"], 2);
    let reindexed = response_json(&gui.post(&format!("/api/reindex?token={}", gui.token)), 200);
    assert_eq!(reindexed["statistics"]["files"], 2);

    let deep = response_json(
        &gui.post(&format!("/api/deep-index?token={}", gui.token)),
        200,
    );
    assert_eq!(deep["deep_index"]["state"], "completed");
    assert_eq!(deep["deep_index"]["deep_archive_index"], true);
    let roots = response_json(&gui.get("/api/roots"), 200);
    let root2_status = roots["roots"]
        .as_array()
        .expect("roots")
        .iter()
        .find(|entry| entry["root"] == path_value(&root2))
        .expect("root2 status");
    assert_eq!(root2_status["deep_index"]["state"], "completed");

    let removed = response_json(
        &gui.post(&format!(
            "/api/remove-root?path={}&token={}",
            root2.display(),
            gui.token
        )),
        200,
    );
    assert_eq!(removed["removed"], true);
    assert_eq!(removed["active_root"], path_value(&root1));
    assert!(root2.join("beta.txt").is_file());
    assert!(root2.join("gamma.txt").is_file());

    let roots = response_json(&gui.get("/api/roots"), 200);
    assert_eq!(roots["roots"].as_array().expect("roots").len(), 1);
    assert_eq!(roots["roots"][0]["root"], path_value(&root1));
    assert_status(
        &gui.post(&format!(
            "/api/remove-root?path={}&token={}",
            root1.display(),
            gui.token
        )),
        400,
    );
    assert!(root1.join("alpha.txt").is_file());

    assert_status(
        &gui.post(&format!(
            "/api/root?path={}&token={}",
            temp.path().join("missing").display(),
            gui.token
        )),
        400,
    );
}

fn prepare(root: &Path, data_dir: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_jikji"))
        .args(["prepare", root.to_str().expect("root utf8"), "--json"])
        .env("JIKJI_DATA_DIR", data_dir)
        .output()
        .expect("prepare");
    assert!(
        output.status.success(),
        "prepare failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct GuiChild {
    port: u16,
    token: String,
    child: Child,
}

impl GuiChild {
    fn start(root: &Path, data_dir: &Path) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve port");
        let port = listener.local_addr().expect("address").port();
        drop(listener);
        let token = format!("root-management-{port}");
        let child = Command::new(env!("CARGO_BIN_EXE_jikji"))
            .args([
                "gui",
                root.to_str().expect("root utf8"),
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--no-open",
                "--serve-child",
                "--manage-token",
                &token,
            ])
            .env("JIKJI_DATA_DIR", data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("gui child");
        let started = Instant::now();
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "GUI start timeout"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        Self { port, token, child }
    }

    fn get(&self, path: &str) -> String {
        self.request("GET", path)
    }

    fn post(&self, path: &str) -> String {
        self.request("POST", path)
    }

    fn request(&self, method: &str, path: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("timeout");
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
            self.port
        )
        .expect("request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("response");
        response
    }
}

impl Drop for GuiChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn response_json(response: &str, status: u16) -> Value {
    assert_status(response, status);
    let (_, body) = response.split_once("\r\n\r\n").expect("response body");
    serde_json::from_str(body).expect("JSON body")
}

fn assert_status(response: &str, status: u16) {
    assert!(
        response.starts_with(&format!("HTTP/1.1 {status} ")),
        "unexpected response: {response}"
    );
}

fn path_value(path: &Path) -> Value {
    Value::String(
        path.canonicalize()
            .expect("canonical path")
            .display()
            .to_string(),
    )
}
