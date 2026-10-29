use serde_json::Value;
use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

static GUI_CHILD_LOCK: Mutex<()> = Mutex::new(());

pub(crate) struct GuiChild {
    url: String,
    child: Child,
    manage_token: String,
    session: String,
    _lock: MutexGuard<'static, ()>,
}

impl GuiChild {
    pub(crate) fn start(root: &Path) -> Self {
        Self::start_with_env(root, &[])
    }
    pub(crate) fn start_with_env(root: &Path, extra_env: &[(&str, &OsStr)]) -> Self {
        let lock = GUI_CHILD_LOCK.lock().expect("gui child lock");
        let port = reserve_loopback_port();
        let manage_token = format!("test-token-{port}");
        let access_token = format!("test-access-token-{port}");
        let mut command = Command::new(env!("CARGO_BIN_EXE_jikji"));
        command
            .args([
                "gui",
                path_str(root).as_str(),
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--no-open",
                "--serve-child",
                "--manage-token",
                manage_token.as_str(),
                "--access-token",
                access_token.as_str(),
            ])
            .env(
                "JIKJI_POST_INSTALL_HOME",
                "/nonexistent-jikji-gui-test-home",
            )
            .env("JIKJI_DATA_DIR", isolated_data_dir(root))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn GUI child");
        let url = format!("http://127.0.0.1:{port}");
        if let Err(error) = wait_until_ready(port) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("connect GUI: {error}");
        }
        let login = http_request_with(
            "POST",
            &url,
            "/api/login",
            &format!("token={access_token}"),
            "",
        );
        let session = session_cookie(&login).unwrap_or_else(|| panic!("GUI login failed: {login}"));
        Self {
            url,
            child,
            manage_token,
            session,
            _lock: lock,
        }
    }

    pub(crate) fn manage_token(&self) -> &str {
        &self.manage_token
    }

    pub(crate) fn get(&self, path: &str) -> String {
        http_request_with("GET", &self.url, path, "", &self.session)
    }

    pub(crate) fn post(&self, path: &str) -> String {
        http_request_with("POST", &self.url, path, "", &self.session)
    }

    /// Request without the session cookie, as an unauthenticated visitor.
    pub(crate) fn get_anonymous(&self, path: &str) -> String {
        http_request("GET", &self.url, path)
    }

    /// Logged-in GET with one extra raw header line, e.g. `Range: bytes=0-9`.
    pub(crate) fn get_with_header(&self, path: &str, header: &str) -> String {
        let cookie = format!("{}\r\n{header}", self.session);
        http_request_with("GET", &self.url, path, "", &cookie)
    }
}

/// `name=value` of the session cookie set by a login response.
fn session_cookie(response: &str) -> Option<String> {
    response.lines().find_map(|line| {
        let value = line.strip_prefix("Set-Cookie: ")?;
        let pair = value.split(';').next()?;
        pair.starts_with("jikji_session=").then(|| pair.to_owned())
    })
}

impl Drop for GuiChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) fn run_ok<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<S> = args.into_iter().collect();
    let mut command = Command::new(env!("CARGO_BIN_EXE_jikji"));
    command.args(&args);
    isolate_command_data_dir(&mut command, &args);
    let output = command.output().expect("run jikji");
    assert!(
        output.status.success(),
        "stderr={}\nstdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    output
}

pub(crate) fn json_cmd<I, S>(args: I) -> Value
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_ok(args);
    serde_json::from_slice(&output.stdout).expect("json stdout")
}

pub(crate) fn run_fail<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<S> = args.into_iter().collect();
    let mut command = Command::new(env!("CARGO_BIN_EXE_jikji"));
    command.args(&args);
    isolate_command_data_dir(&mut command, &args);
    let output = command.output().expect("run jikji");
    assert!(
        !output.status.success(),
        "expected failure stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    output
}

pub(crate) fn path_str(path: &Path) -> String {
    path.display().to_string()
}

fn isolated_data_dir(root: impl AsRef<OsStr>) -> PathBuf {
    use std::hash::{Hash, Hasher};

    let path = PathBuf::from(root.as_ref());
    let key = path
        .ancestors()
        .find(|ancestor| {
            ancestor
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".tmp"))
        })
        .unwrap_or(path.as_path());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.to_string_lossy().hash(&mut hasher);
    let data = std::env::temp_dir().join(format!("jikji-gui-test-db-{:016x}", hasher.finish()));
    fs::create_dir_all(&data).expect("jikji test data dir");
    data
}

fn isolate_command_data_dir<S: AsRef<OsStr>>(command: &mut Command, args: &[S]) {
    let Some(root) = args.get(1) else {
        return;
    };
    if root.as_ref().to_string_lossy().starts_with('-') {
        return;
    }
    command.env("JIKJI_DATA_DIR", isolated_data_dir(root));
}

pub(crate) fn assert_rejected(response: &str) {
    assert!(
        response.starts_with("HTTP/1.1 403 Forbidden")
            || response.starts_with("HTTP/1.1 404 Not Found")
            || response.starts_with("HTTP/1.1 400 Bad Request"),
        "{response}"
    );
}

fn http_request(method: &str, base_url: &str, path: &str) -> String {
    http_request_with(method, base_url, path, "", "")
}

fn http_request_with(method: &str, base_url: &str, path: &str, body: &str, cookie: &str) -> String {
    let (host, port) = parse_url(base_url);
    let started = Instant::now();
    let cookie = if cookie.is_empty() {
        String::new()
    } else {
        format!("Cookie: {cookie}\r\n")
    };
    loop {
        match TcpStream::connect((host.as_str(), port)) {
            Ok(mut stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(15)))
                    .expect("timeout");
                let request = format!(
                    "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n{cookie}Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(request.as_bytes()).expect("write");
                let mut response = Vec::new();
                match stream.read_to_end(&mut response) {
                    Ok(_) => {}
                    Err(error)
                        if error.kind() == std::io::ErrorKind::ConnectionReset
                            && !response.is_empty() => {}
                    Err(error) => panic!("read {method} {path}: {error}"),
                }
                return String::from_utf8_lossy(&response).into_owned();
            }
            Err(error) if started.elapsed() < Duration::from_secs(5) => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => panic!("connect GUI: {error}"),
        }
    }
}

fn reserve_loopback_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve port");
    let port = listener.local_addr().expect("reserved address").port();
    drop(listener);
    port
}

fn wait_until_ready(port: u16) -> std::io::Result<()> {
    let started = Instant::now();
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(_) => return Ok(()),
            Err(error) if started.elapsed() < Duration::from_secs(5) => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error),
        }
    }
}

fn parse_url(url: &str) -> (String, u16) {
    let without_scheme = url.strip_prefix("http://").expect("http url");
    let host_port = without_scheme.trim_end_matches('/');
    let (host, port) = host_port.rsplit_once(':').expect("host port");
    (host.to_owned(), port.parse().expect("port"))
}
