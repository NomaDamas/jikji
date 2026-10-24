use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use jikji_core::{Result, io_error, json_error};
use jikji_public_datasets::{DatasetError, ResourceFetcher};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use crate::public_datasets::{
    HardBenchDifficulty, HardBenchDocument, HardBenchOptions, SplitDatasetBuildResult,
    WorkspaceBenchBuildResult, WorkspaceFile, WorkspaceTask, build_hardbench_fixture,
    build_workspacebench_fixture,
};

const WORKSPACE_REPO: &str = "Workspace-Bench/Workspace-Bench-Lite";
const WORKSPACE_PREFIX: &str = "task_lite_clean_en";

#[derive(Debug, Clone)]
pub struct WorkspaceBenchFetchOptions {
    pub dest: PathBuf,
    pub api_url: String,
    pub resolve_base_url: String,
    pub max_tasks: usize,
    pub start: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

impl WorkspaceBenchFetchOptions {
    pub fn new(dest: impl Into<PathBuf>) -> Self {
        Self {
            dest: dest.into(),
            api_url: format!("https://huggingface.co/api/datasets/{WORKSPACE_REPO}"),
            resolve_base_url: format!(
                "https://huggingface.co/datasets/{WORKSPACE_REPO}/resolve/main/"
            ),
            max_tasks: 12,
            start: 0,
            max_file_bytes: 25 * 1024 * 1024,
            max_total_bytes: 500 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Deserialize)]
struct DatasetInfo {
    #[serde(default)]
    siblings: Vec<DatasetSibling>,
}

#[derive(Debug, Deserialize)]
struct DatasetSibling {
    rfilename: String,
}

#[derive(Debug, Deserialize)]
struct RemoteWorkspaceFile {
    filename: String,
    stored_relpath: String,
}

#[derive(Debug, Deserialize)]
struct RemoteWorkspaceEdge {
    from: String,
}

#[derive(Debug, Deserialize)]
struct RemoteWorkspaceTask {
    absolute_id: u64,
    #[serde(default)]
    persona: String,
    #[serde(default)]
    task: String,
    #[serde(default)]
    task_diff: Value,
    #[serde(default)]
    tested_capabilities: Vec<String>,
    #[serde(default)]
    output_files: Vec<String>,
    #[serde(default)]
    data_manifest: Vec<RemoteWorkspaceFile>,
    #[serde(default)]
    file_dep_graph: Vec<RemoteWorkspaceEdge>,
}

pub fn fetch_workspacebench(
    fetcher: &impl ResourceFetcher,
    options: &WorkspaceBenchFetchOptions,
) -> Result<WorkspaceBenchBuildResult> {
    validate_positive("max_tasks", options.max_tasks)?;
    validate_positive_u64("max_file_bytes", options.max_file_bytes)?;
    validate_positive_u64("max_total_bytes", options.max_total_bytes)?;
    let staging = options.dest.join(".workspacebench-downloads");
    reset_dir(&staging)?;
    let info_path = staging.join("dataset-info.json");
    fetch(fetcher, &options.api_url, &info_path, 8 * 1024 * 1024)?;
    let info: DatasetInfo = read_json(&info_path)?;
    let task_pattern = Regex::new(&format!(r"^{WORKSPACE_PREFIX}/(\d+)/metadata\.json$"))
        .map_err(|error| invalid(error.to_string()))?;
    let mut ids = info
        .siblings
        .iter()
        .filter_map(|row| task_pattern.captures(&row.rfilename))
        .filter_map(|captures| captures[1].parse::<u64>().ok())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .skip(options.start)
        .take(options.max_tasks)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    if ids.is_empty() {
        return Err(invalid("No Workspace-Bench-Lite task metadata found"));
    }

    let mut tasks = Vec::with_capacity(ids.len());
    let mut total = 0_u64;
    for id in ids {
        let base = format!("{WORKSPACE_PREFIX}/{id}");
        let metadata_path = staging.join(format!("task-{id}-metadata.json"));
        fetch(
            fetcher,
            &resolve_url(&options.resolve_base_url, &format!("{base}/metadata.json"))?,
            &metadata_path,
            options.max_file_bytes,
        )?;
        let metadata: RemoteWorkspaceTask = read_json(&metadata_path)?;
        if metadata.absolute_id != id {
            return Err(invalid(format!(
                "Workspace-Bench metadata id mismatch: expected {id}, got {}",
                metadata.absolute_id
            )));
        }
        let required_filenames = metadata
            .file_dep_graph
            .iter()
            .map(|edge| edge.from.clone())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        let mut files = Vec::with_capacity(metadata.data_manifest.len());
        for row in metadata.data_manifest {
            let relative = checked_relative(&row.stored_relpath)?;
            let body_path = staging.join(format!("task-{id}")).join(&relative);
            let size = fetch(
                fetcher,
                &resolve_url(
                    &options.resolve_base_url,
                    &format!("{base}/{}", relative.to_string_lossy().replace('\\', "/")),
                )?,
                &body_path,
                options.max_file_bytes,
            )?;
            total = total
                .checked_add(size)
                .ok_or_else(|| invalid("Workspace-Bench byte count overflow"))?;
            if total > options.max_total_bytes {
                return Err(invalid(format!(
                    "Workspace-Bench download exceeds max_total_bytes={}",
                    options.max_total_bytes
                )));
            }
            files.push(WorkspaceFile {
                filename: row.filename,
                stored_relpath: relative.to_string_lossy().replace('\\', "/"),
                bytes: fs::read(&body_path).map_err(|source| io_error(&body_path, source))?,
            });
        }
        tasks.push(WorkspaceTask {
            absolute_id: metadata.absolute_id,
            persona: metadata.persona,
            task: metadata.task,
            task_diff: metadata.task_diff,
            tested_capabilities: metadata.tested_capabilities,
            output_files: metadata.output_files,
            data_manifest: files,
            required_filenames,
        });
    }
    let result = build_workspacebench_fixture(&options.dest, &tasks)?;
    rewrite_manifest(
        &result.manifest_path,
        json!({"network": "downloaded", "source_api": options.api_url}),
    )?;
    let _ = fs::remove_dir_all(staging);
    Ok(result)
}

#[derive(Debug, Clone)]
pub struct HardBenchFetchOptions {
    pub dest: PathBuf,
    pub view_base_url: String,
    pub file_base_url: String,
    pub target_docs: usize,
    pub max_data_idx: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_cases_per_split: usize,
    pub seed: u64,
    pub difficulty: HardBenchDifficulty,
}

impl HardBenchFetchOptions {
    pub fn new(dest: impl Into<PathBuf>) -> Self {
        Self {
            dest: dest.into(),
            view_base_url: "https://www.kogl.or.kr/edu/eduDataView.do".to_owned(),
            file_base_url: "https://www.kogl.or.kr/edu/eduDataFileDown.do".to_owned(),
            target_docs: 180,
            max_data_idx: 180,
            max_file_bytes: 80 * 1024 * 1024,
            max_total_bytes: 5 * 1024 * 1024 * 1024,
            max_cases_per_split: 240,
            seed: 20_260_603,
            difficulty: HardBenchDifficulty::Hard,
        }
    }
}

pub fn fetch_hardbench(
    fetcher: &impl ResourceFetcher,
    options: &HardBenchFetchOptions,
) -> Result<SplitDatasetBuildResult> {
    validate_positive("target_docs", options.target_docs)?;
    validate_positive("max_data_idx", options.max_data_idx)?;
    validate_positive_u64("max_file_bytes", options.max_file_bytes)?;
    validate_positive_u64("max_total_bytes", options.max_total_bytes)?;
    let staging = options.dest.join(".hardbench-downloads");
    reset_dir(&staging)?;
    let link_pattern =
        Regex::new(r#"(?is)href=[\"']([^\"']*eduDataFileDown\.do[^\"']*)[\"'][^>]*>(.*?)</a>"#)
            .map_err(|error| invalid(error.to_string()))?;
    let tag_pattern = Regex::new(r"(?is)<[^>]+>").map_err(|error| invalid(error.to_string()))?;
    let allowed = BTreeSet::from(["pdf", "hwp", "hwpx", "pptx", "xlsx"]);
    let mut docs = Vec::new();
    let mut seen = BTreeSet::new();
    let mut total = 0_u64;
    for data_idx in 1..=options.max_data_idx {
        if docs.len() >= options.target_docs {
            break;
        }
        let page_path = staging.join(format!("page-{data_idx}.html"));
        let page_url = append_query(&options.view_base_url, &[("dataIdx", data_idx.to_string())])?;
        if fetch(fetcher, &page_url, &page_path, 4 * 1024 * 1024).is_err() {
            continue;
        }
        let page = fs::read_to_string(&page_path).map_err(|source| io_error(&page_path, source))?;
        for captures in link_pattern.captures_iter(&page) {
            if docs.len() >= options.target_docs {
                break;
            }
            let href = html_unescape(&captures[1]);
            let filename = clean_html(&captures[2], &tag_pattern);
            let Some(extension) = filename.rsplit('.').next().map(str::to_ascii_lowercase) else {
                continue;
            };
            if !allowed.contains(extension.as_str()) {
                continue;
            }
            let link = Url::parse(&options.view_base_url)
                .and_then(|base| base.join(&href))
                .map_err(|error| invalid(format!("invalid KOGL attachment URL: {error}")))?;
            let params = link.query_pairs().collect::<BTreeMap<_, _>>();
            let file_idx = params
                .get("dataFileIdx")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            let row_idx = params
                .get("dataIdx")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(data_idx as u64);
            if file_idx == 0 || !seen.insert((row_idx, file_idx, filename.clone())) {
                continue;
            }
            let download_url = append_query(
                &options.file_base_url,
                &[
                    ("dataIdx", row_idx.to_string()),
                    ("dataFileIdx", file_idx.to_string()),
                ],
            )?;
            let body_path = staging.join(format!("doc-{row_idx}-{file_idx}.{extension}"));
            let Ok(size) = fetch(fetcher, &download_url, &body_path, options.max_file_bytes) else {
                continue;
            };
            let bytes = fs::read(&body_path).map_err(|source| io_error(&body_path, source))?;
            if !valid_document_signature(&extension, &bytes) {
                continue;
            }
            if total.saturating_add(size) > options.max_total_bytes {
                return Err(invalid(format!(
                    "HardBench download exceeds max_total_bytes={}",
                    options.max_total_bytes
                )));
            }
            total += size;
            docs.push(HardBenchDocument {
                id: format!("kogl-{row_idx}-{file_idx}"),
                filename: filename.clone(),
                extension: format!(".{extension}"),
                page_title: filename
                    .trim_end_matches(&format!(".{extension}"))
                    .to_owned(),
                text_excerpt: filename.clone(),
                doc_type: document_type(&filename).to_owned(),
                source_url: page_url.clone(),
                bytes,
            });
        }
    }
    if docs.len() < 3 {
        return Err(invalid(format!(
            "Too few hardbench documents downloaded: {} (need at least 3 for train/valid/test)",
            docs.len()
        )));
    }
    let result = build_hardbench_fixture(
        &options.dest,
        &docs,
        HardBenchOptions {
            max_cases_per_split: options.max_cases_per_split,
            seed: options.seed,
            difficulty: options.difficulty,
        },
    )?;
    rewrite_manifest(
        &result.manifest_path,
        json!({
            "network": "downloaded",
            "source_family": "KOGL public resource attachments",
            "source_url": options.view_base_url,
            "bytes_downloaded": total,
        }),
    )?;
    let _ = fs::remove_dir_all(staging);
    Ok(result)
}

fn fetch(
    fetcher: &impl ResourceFetcher,
    resource: &str,
    destination: &Path,
    max_bytes: u64,
) -> Result<u64> {
    fetcher
        .fetch_to(resource, destination, max_bytes)
        .map_err(dataset_error)
}

fn dataset_error(error: DatasetError) -> jikji_core::JikjiError {
    io_error("public-dataset", std::io::Error::other(error.to_string()))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
    serde_json::from_slice(&bytes).map_err(|source| json_error(path, source))
}

fn rewrite_manifest(path: &Path, additions: Value) -> Result<()> {
    let mut manifest: Value = read_json(path)?;
    let Some(target) = manifest.as_object_mut() else {
        return Err(invalid("benchmark manifest is not a JSON object"));
    };
    let Some(additions) = additions.as_object() else {
        return Err(invalid("manifest additions are not a JSON object"));
    };
    target.extend(additions.clone());
    let text =
        serde_json::to_string_pretty(&manifest).map_err(|source| json_error(path, source))?;
    fs::write(path, format!("{text}\n")).map_err(|source| io_error(path, source))
}

fn resolve_url(base: &str, relative: &str) -> Result<String> {
    let mut url =
        Url::parse(base).map_err(|error| invalid(format!("invalid base URL: {error}")))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| invalid("base URL cannot hold path segments"))?;
        segments.pop_if_empty();
        for segment in relative.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err(invalid(format!("unsafe remote path: {relative:?}")));
            }
            segments.push(segment);
        }
    }
    Ok(url.into())
}

fn append_query(base: &str, pairs: &[(&str, String)]) -> Result<String> {
    let mut url = Url::parse(base).map_err(|error| invalid(format!("invalid URL: {error}")))?;
    url.query_pairs_mut()
        .clear()
        .extend_pairs(pairs.iter().map(|(key, value)| (*key, value)));
    Ok(url.into())
}

fn checked_relative(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(invalid(format!("unsafe Workspace-Bench path: {value:?}")));
    }
    Ok(path.to_path_buf())
}

fn reset_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|source| io_error(path, source))?;
    }
    fs::create_dir_all(path).map_err(|source| io_error(path, source))
}

fn validate_positive(name: &str, value: usize) -> Result<()> {
    if value == 0 {
        Err(invalid(format!("{name} must be greater than zero")))
    } else {
        Ok(())
    }
}

fn validate_positive_u64(name: &str, value: u64) -> Result<()> {
    if value == 0 {
        Err(invalid(format!("{name} must be greater than zero")))
    } else {
        Ok(())
    }
}

fn valid_document_signature(extension: &str, bytes: &[u8]) -> bool {
    match extension {
        "pdf" => bytes.starts_with(b"%PDF"),
        "hwp" => bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0]),
        "hwpx" | "pptx" | "xlsx" => bytes.starts_with(b"PK"),
        _ => false,
    }
}

fn clean_html(value: &str, tags: &Regex) -> String {
    html_unescape(tags.replace_all(value, " ").trim())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn html_unescape(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn document_type(filename: &str) -> &'static str {
    if ["매뉴얼", "지침", "가이드"]
        .iter()
        .any(|term| filename.contains(term))
    {
        "manual"
    } else if ["보고서", "연구", "조사"]
        .iter()
        .any(|term| filename.contains(term))
    {
        "report"
    } else if ["교육", "교안", "발표"]
        .iter()
        .any(|term| filename.contains(term))
    {
        "training"
    } else {
        "reference"
    }
}

fn invalid(message: impl Into<String>) -> jikji_core::JikjiError {
    io_error(
        "<public-adapter>",
        std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into()),
    )
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use jikji_public_datasets::HttpFetcher;

    use super::*;

    #[test]
    fn workspacebench_downloads_real_endpoint_payloads() {
        let server = FixtureServer::start(|path| {
            match path {
            "/api" => response(
                200,
                "application/json",
                br#"{"siblings":[{"rfilename":"task_lite_clean_en/7/metadata.json"}]}"#,
            ),
            "/resolve/task_lite_clean_en/7/metadata.json" => response(
                200,
                "application/json",
                br#"{"absolute_id":7,"persona":"analyst","task":"forecast","data_manifest":[{"filename":"input.csv","stored_relpath":"data/input.csv"},{"filename":"notes.txt","stored_relpath":"notes.txt"}],"file_dep_graph":[{"from":"input.csv"}]}"#,
            ),
            "/resolve/task_lite_clean_en/7/data/input.csv" => {
                response(200, "text/csv", b"year,value\n2026,10\n")
            }
            "/resolve/task_lite_clean_en/7/notes.txt" => {
                response(200, "text/plain", b"supporting notes")
            }
            _ => response(404, "text/plain", b"missing"),
        }
        });
        let temp = tempfile::tempdir().expect("tempdir");
        let mut options = WorkspaceBenchFetchOptions::new(temp.path());
        options.api_url = format!("{}/api", server.base_url);
        options.resolve_base_url = format!("{}/resolve/", server.base_url);
        options.max_tasks = 1;

        let result = fetch_workspacebench(&HttpFetcher::default(), &options).expect("download");
        assert_eq!(result.tasks, 1);
        assert_eq!(result.files_materialized, 2);
        let eval = fs::read_to_string(result.eval_set_path).expect("eval");
        assert!(eval.contains("task_7/data/input.csv"));
        assert!(!eval.contains("task_7/notes.txt"));
        let manifest: Value = read_json(&result.manifest_path).expect("manifest");
        assert_eq!(manifest["network"], "downloaded");
    }

    #[test]
    fn workspacebench_rejects_remote_path_traversal() {
        let server = FixtureServer::start(|path| {
            match path {
            "/api" => response(
                200,
                "application/json",
                br#"{"siblings":[{"rfilename":"task_lite_clean_en/1/metadata.json"}]}"#,
            ),
            "/resolve/task_lite_clean_en/1/metadata.json" => response(
                200,
                "application/json",
                br#"{"absolute_id":1,"data_manifest":[{"filename":"escape","stored_relpath":"../escape.txt"}]}"#,
            ),
            _ => response(404, "text/plain", b"missing"),
        }
        });
        let temp = tempfile::tempdir().expect("tempdir");
        let mut options = WorkspaceBenchFetchOptions::new(temp.path());
        options.api_url = format!("{}/api", server.base_url);
        options.resolve_base_url = format!("{}/resolve/", server.base_url);
        options.max_tasks = 1;

        let error = fetch_workspacebench(&HttpFetcher::default(), &options).expect_err("unsafe");
        assert!(error.to_string().contains("unsafe Workspace-Bench path"));
        assert!(
            !temp
                .path()
                .parent()
                .expect("parent")
                .join("escape.txt")
                .exists()
        );
    }

    #[test]
    fn hardbench_crawls_and_validates_actual_document_bodies() {
        let server = FixtureServer::start(|path| {
            if let Some(index) = path.strip_prefix("/view?dataIdx=") {
                let body = format!(
                    "<a href=\"/edu/eduDataFileDown.do?dataIdx={index}&amp;dataFileIdx={index}\">정책보고서_{index}.pdf</a>"
                );
                response(200, "text/html", body.as_bytes())
            } else if path.starts_with("/file?") {
                response(
                    200,
                    "application/pdf",
                    b"%PDF-1.7\nreal endpoint document\n",
                )
            } else {
                response(404, "text/plain", b"missing")
            }
        });
        let temp = tempfile::tempdir().expect("tempdir");
        let mut options = HardBenchFetchOptions::new(temp.path());
        options.view_base_url = format!("{}/view", server.base_url);
        options.file_base_url = format!("{}/file", server.base_url);
        options.target_docs = 3;
        options.max_data_idx = 3;
        options.max_cases_per_split = 2;

        let result = fetch_hardbench(&HttpFetcher::default(), &options).expect("download");
        assert_eq!(result.docs, 3);
        assert!(result.train_docs > 0 && result.valid_docs > 0 && result.test_docs > 0);
        assert!(result.eval_set_path.is_file());
        let manifest: Value = read_json(&result.manifest_path).expect("manifest");
        assert_eq!(manifest["network"], "downloaded");
    }

    #[test]
    fn hardbench_rejects_html_disguised_as_pdf() {
        let server = FixtureServer::start(|path| {
            if path.starts_with("/view?") {
                response(
                    200,
                    "text/html",
                    b"<a href=\"/edu/eduDataFileDown.do?dataIdx=1&amp;dataFileIdx=1\">report.pdf</a>",
                )
            } else {
                response(200, "text/html", b"<html>error</html>")
            }
        });
        let temp = tempfile::tempdir().expect("tempdir");
        let mut options = HardBenchFetchOptions::new(temp.path());
        options.view_base_url = format!("{}/view", server.base_url);
        options.file_base_url = format!("{}/file", server.base_url);
        options.target_docs = 3;
        options.max_data_idx = 3;

        let error = fetch_hardbench(&HttpFetcher::default(), &options).expect_err("invalid bodies");
        assert!(
            error
                .to_string()
                .contains("Too few hardbench documents downloaded")
        );
    }

    /// Serves canned responses until dropped. It must outlive the whole test: a server that stops
    /// itself after an idle period races with a slow client on a loaded machine and turns into
    /// `Connection refused`.
    struct FixtureServer {
        base_url: String,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl FixtureServer {
        fn start(handler: impl Fn(&str) -> Vec<u8> + Send + 'static) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
            let address = listener.local_addr().expect("address");
            listener.set_nonblocking(true).expect("nonblocking");
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stopped = std::sync::Arc::clone(&stop);
            let handle = thread::spawn(move || {
                while !stopped.load(std::sync::atomic::Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            // The accepted socket must block even though the listener does not.
                            let _ = stream.set_nonblocking(false);
                            let path = request_path(&mut stream);
                            let bytes = handler(&path);
                            let _ = stream.write_all(&bytes);
                            let _ = stream.flush();
                            // Let the client read the whole response before the socket closes.
                            let _ = stream.shutdown(std::net::Shutdown::Write);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(std::time::Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                base_url: format!("http://{address}"),
                stop,
                thread: Some(handle),
            }
        }
    }

    impl Drop for FixtureServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(handle) = self.thread.take() {
                let _ = handle.join();
            }
        }
    }

    fn request_path(stream: &mut TcpStream) -> String {
        let mut bytes = [0_u8; 4096];
        let count = stream.read(&mut bytes).expect("request");
        let request = String::from_utf8_lossy(&bytes[..count]);
        request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_owned()
    }

    fn response(status: u16, content_type: &str, body: &[u8]) -> Vec<u8> {
        let reason = if status == 200 { "OK" } else { "Not Found" };
        let mut response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }
}
