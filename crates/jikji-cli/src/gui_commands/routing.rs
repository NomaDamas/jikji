#[cfg(all(unix, not(target_os = "macos")))]
use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Read;
#[cfg(all(unix, not(target_os = "macos")))]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, UNIX_EPOCH};

use super::http::{
    HttpRequest, HttpResponse, malformed_request, query_bool, query_flag, query_value, query_values,
};
use super::jobs::{JobRegistry, snapshot_response};
use super::scenarios;
use super::token::ManagementToken;
use jikji_core::PrepareOptions;
use jikji_core::storage::{
    clear_artifact, database_path, delete_root_by_key, indexed_roots, load_artifact,
    load_artifact_paths, load_artifacts, migrate_legacy, register_root, remove_artifacts_under,
    root_key, root_statistics, searchable_root_paths, searchable_root_paths_for_extensions,
    store_artifact,
};
use jikji_index::{CleanOptions, clean, doctor, prepare, read_map};
use jikji_search::{
    BriefOptions, DiscoverOptions, SearchOptions, brief_payload, discover, explain_source,
    graph_query, graph_status, search, search_index_status,
};
use serde_json::json;

#[derive(Clone)]
pub(crate) struct GuiState {
    root: Arc<RwLock<PathBuf>>,
    mutation: Arc<Mutex<()>>,
    manage_token: ManagementToken,
    jobs: JobRegistry,
}

impl GuiState {
    pub(crate) fn new(root: PathBuf, manage_token: ManagementToken) -> Self {
        Self {
            root: Arc::new(RwLock::new(root)),
            mutation: Arc::new(Mutex::new(())),
            manage_token,
            jobs: JobRegistry::default(),
        }
    }

    fn root(&self) -> std::result::Result<PathBuf, HttpResponse> {
        self.root
            .read()
            .map(|root| root.clone())
            .map_err(|_| HttpResponse::json(500, json!({"error": "root state lock poisoned"})))
    }

    fn switch_root(&self, root: PathBuf) -> std::result::Result<(), HttpResponse> {
        let mut guard = self
            .root
            .write()
            .map_err(|_| HttpResponse::json(500, json!({"error": "root state lock poisoned"})))?;
        *guard = root;
        Ok(())
    }

    fn token_matches(&self, query: &str) -> bool {
        query_value(query, "token").is_some_and(|token| self.manage_token.matches(&token))
    }

    fn mutation_guard(&self) -> std::result::Result<std::sync::MutexGuard<'_, ()>, HttpResponse> {
        self.mutation
            .lock()
            .map_err(|_| HttpResponse::json(500, json!({"error": "management lock poisoned"})))
    }
}

pub(crate) fn route_request(
    state: &GuiState,
    request: &HttpRequest,
    index_html: &'static str,
) -> HttpResponse {
    if request.method.is_empty() {
        return malformed_request();
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => HttpResponse::html(200, index_html),
        ("GET", "/api/status") | ("GET", "/api/root-status") => with_root(state, root_status),
        ("GET", path) if path.starts_with("/api/jobs/") => {
            let id = path.trim_start_matches("/api/jobs/");
            match state.jobs.get(id) {
                Some(job) => HttpResponse::json(200, snapshot_response(job)),
                None => HttpResponse::json(404, json!({"error":"job not found"})),
            }
        }
        ("POST", path) if path.starts_with("/api/jobs/") && path.ends_with("/cancel") => {
            let id = path
                .trim_start_matches("/api/jobs/")
                .trim_end_matches("/cancel");
            cancel_job_response(state, id, &request.query)
        }
        ("GET", "/api/roots") => roots_response(state),
        ("GET", "/api/files") => with_root(state, |root| files_response(root, &request.query)),
        ("GET", "/api/indexed-files") => {
            with_root(state, |root| indexed_files_response(root, &request.query))
        }
        ("GET", "/api/search") => with_root(state, |root| search_response(root, &request.query)),
        ("GET", "/api/find") | ("GET", "/api/discover") => discover_response(state, &request.query),
        ("GET", "/api/scenarios") => HttpResponse::json(200, scenarios::list()),
        ("GET", "/api/scenario-run") => scenario_response(state, &request.query),
        ("GET", "/api/preview") => preview_response(state, &request.query),
        ("GET", "/api/preview/file") => preview_file_response(state, &request.query),
        ("GET", "/api/doctor") => with_root(state, doctor_response),
        ("GET", "/api/map") => with_root(state, map_response),
        ("GET", "/api/graph") => with_root(state, |root| graph_response(root, &request.query)),
        ("GET", "/api/brief") => with_root(state, |root| brief_response(root, &request.query)),
        ("GET", "/api/settings") => settings_response(state, &request.query, false),
        ("POST", "/api/settings") => {
            management_response(state, &request.query, settings_mutation_response)
        }
        ("POST", "/api/exclude-selection") => {
            management_response(state, &request.query, exclude_selection_response)
        }
        ("POST", "/api/clean") => management_response(state, &request.query, clean_response),
        ("GET", "/download") => download_response(state, &request.query),
        ("POST", "/open") => management_response(state, &request.query, open_response),
        ("POST", "/reveal") => management_response(state, &request.query, reveal_response),
        ("POST", "/api/refresh") => management_response(state, &request.query, refresh_response),
        ("POST", "/api/reindex-folder") => {
            management_response(state, &request.query, reindex_folder_response)
        }
        ("POST", "/api/index-selection") => {
            management_response(state, &request.query, index_selection_response)
        }
        ("POST", "/api/deep-index") => {
            management_response(state, &request.query, deep_index_response)
        }
        ("POST" | "DELETE", "/api/remove-folder") => {
            management_response(state, &request.query, remove_folder_response)
        }
        ("POST" | "DELETE", "/api/deep-index-target") => {
            management_response(state, &request.query, deep_index_target_response)
        }
        ("POST", "/api/reindex") => management_response(state, &request.query, reindex_response),
        ("POST", "/api/root") => management_response(state, &request.query, root_switch_response),
        ("POST" | "DELETE", "/api/remove-root") => {
            management_response(state, &request.query, remove_root_response)
        }
        _ => HttpResponse::json(404, json!({"error": "not found"})),
    }
}

fn management_response(
    state: &GuiState,
    query: &str,
    action: fn(&GuiState, &str) -> HttpResponse,
) -> HttpResponse {
    if !state.token_matches(query) {
        return HttpResponse::json(403, json!({"error": "invalid management token"}));
    }
    let _guard = match state.mutation_guard() {
        Ok(guard) => guard,
        Err(response) => return response,
    };
    action(state, query)
}

fn cancel_job_response(state: &GuiState, id: &str, query: &str) -> HttpResponse {
    if !state.token_matches(query) {
        return HttpResponse::json(403, json!({"error":"invalid management token"}));
    }
    match state.jobs.cancel(id) {
        Some(job) => HttpResponse::json(200, snapshot_response(job)),
        None => HttpResponse::json(404, json!({"error":"job not found"})),
    }
}

fn with_root(state: &GuiState, action: impl FnOnce(&Path) -> HttpResponse) -> HttpResponse {
    match state.root() {
        Ok(root) => action(&root),
        Err(response) => response,
    }
}

fn roots_response(state: &GuiState) -> HttpResponse {
    let active_root = match state.root() {
        Ok(root) => root,
        Err(response) => return response,
    };
    let library_roots = crate::post_install_commands::library_search_prefixes(
        &crate::post_install_commands::post_install_home(),
    );
    match indexed_roots() {
        Ok(mut roots) => {
            roots.retain(|item| keep_listed_root(&item.root, &active_root));
            roots.sort_by(|left, right| {
                root_list_rank(&right.root, &active_root, &library_roots)
                    .cmp(&root_list_rank(&left.root, &active_root, &library_roots))
                    .then_with(|| left.root.cmp(&right.root))
            });
            HttpResponse::json(
                200,
                json!({
                    "active_root": active_root,
                    "roots": roots,
                    "library_roots": library_roots,
                }),
            )
        }
        Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
    }
}

fn keep_listed_root(path: &Path, active: &Path) -> bool {
    if path == active {
        return true;
    }
    let tmp = std::env::temp_dir();
    if path.starts_with(&tmp) {
        if path
            .components()
            .any(|component| component.as_os_str().to_string_lossy().starts_with("jikji"))
        {
            return false;
        }
        return path.is_dir();
    }
    true
}

fn library_rank(path: &Path, library: &[PathBuf]) -> u8 {
    u8::from(
        library
            .iter()
            .any(|root| path == root || path.starts_with(root)),
    )
}

fn root_list_rank(path: &Path, active: &Path, library: &[PathBuf]) -> u8 {
    if path == active {
        2
    } else {
        library_rank(path, library)
    }
}

fn search_roots_for_query(active: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    push_unique_search_root(&mut roots, active);
    let home = crate::post_install_commands::post_install_home();
    let mut extras: Vec<PathBuf> = searchable_root_paths()
        .unwrap_or_default()
        .into_iter()
        .filter(|path| crate::post_install_commands::is_library_search_path(path, &home))
        .collect();
    extras.sort_by_key(|path| path.components().count());
    for path in extras {
        push_unique_search_root(&mut roots, &path);
    }
    roots
}

fn discover_roots_for_query(active: &Path, extensions: &[String]) -> Vec<PathBuf> {
    let roots = search_roots_for_query(active);
    if extensions.is_empty() {
        return roots;
    }
    let matching = searchable_root_paths_for_extensions(extensions).unwrap_or_default();
    let roots = filter_roots_by_matching_extensions(roots, active, &matching);
    drop_redundant_nested_search_roots(roots, active)
}

fn filter_roots_by_matching_extensions(
    roots: Vec<PathBuf>,
    active: &Path,
    matching: &[PathBuf],
) -> Vec<PathBuf> {
    roots
        .into_iter()
        .filter(|root| {
            root == active
                || matching
                    .iter()
                    .any(|candidate| paths_refer_to_same_root(root, candidate))
        })
        .collect()
}

fn drop_redundant_nested_search_roots(roots: Vec<PathBuf>, active: &Path) -> Vec<PathBuf> {
    roots
        .iter()
        .filter(|root| {
            if *root == active {
                return true;
            }
            !roots
                .iter()
                .any(|other| other != *root && other != active && root.starts_with(other))
        })
        .cloned()
        .collect()
}

const DISCOVER_ROOT_CONCURRENCY: usize = 32;

fn collect_discover_payloads(
    roots: &[PathBuf],
    query: &str,
    top_k: usize,
    retry_exhausted: bool,
    retry_proof: &str,
    active: &Path,
) -> (Vec<serde_json::Value>, Option<jikji_core::JikjiError>) {
    let mut payloads = Vec::new();
    let mut last_error = None;
    let active = active.to_path_buf();
    for chunk in roots.chunks(DISCOVER_ROOT_CONCURRENCY) {
        std::thread::scope(|scope| {
            let mut joins = Vec::new();
            for root in chunk {
                let root = root.clone();
                let q = query.to_owned();
                let proof = retry_proof.to_owned();
                let active = active.clone();
                joins.push(scope.spawn(move || {
                    let started = Instant::now();
                    let lite = !paths_look_like_same_root(&root, &active);
                    discover(
                        &root,
                        &q,
                        DiscoverOptions {
                            top_k,
                            retry_exhausted,
                            retry_proof: proof,
                            lite,
                        },
                    )
                    .map(|mut payload| {
                        annotate_candidates_with_root(&root, &mut payload);
                        payload["root"] = json!(root.to_string_lossy());
                        payload["root_elapsed_ms"] = json!(started.elapsed().as_millis());
                        payload
                    })
                }));
            }
            for join in joins {
                match join.join() {
                    Ok(Ok(payload)) => payloads.push(payload),
                    Ok(Err(error)) => last_error = Some(error),
                    Err(_) => {
                        last_error = Some(jikji_core::io_error(
                            "<gui-find>",
                            std::io::Error::other("discover thread panicked"),
                        ));
                    }
                }
            }
        });
    }
    (payloads, last_error)
}

fn push_unique_search_root(roots: &mut Vec<PathBuf>, path: &Path) {
    // Do not canonicalize or STAT: rclone FUSE lookups of a busy Drive child
    // (e.g. 마커) block the entire GUI /api/find handler.
    if roots.iter().any(|existing| existing == path) {
        return;
    }
    roots.push(path.to_path_buf());
}

fn requested_search_root(
    state: &GuiState,
    query: &str,
) -> std::result::Result<PathBuf, HttpResponse> {
    let active = state.root()?;
    let Some(raw) = query_value(query, "root").filter(|value| !value.is_empty()) else {
        return Ok(active);
    };
    let requested = PathBuf::from(raw);
    if searchable_root_contains(&search_roots_for_query(&active), &requested) {
        Ok(requested)
    } else {
        Err(HttpResponse::json(
            403,
            json!({"error": "path traversal is not allowed"}),
        ))
    }
}

fn searchable_root_contains(roots: &[PathBuf], requested: &Path) -> bool {
    roots
        .iter()
        .any(|root| paths_refer_to_same_root(root, requested))
}

fn paths_look_like_same_root(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let normalize = |path: &Path| path.to_string_lossy().trim_end_matches('/').to_owned();
    normalize(left) == normalize(right)
}
fn paths_refer_to_same_root(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let normalize = |path: &Path| path.to_string_lossy().trim_end_matches('/').to_owned();
    if normalize(left) == normalize(right) {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn root_status(root: &Path) -> HttpResponse {
    let manifest = load_artifact(root, "manifest")
        .ok()
        .flatten()
        .unwrap_or_else(|| json!({}));
    let deep_index = load_artifact(root, "deep_index_status").ok().flatten();
    let statistics = match root_statistics(root) {
        Ok(statistics) => statistics,
        Err(error) => return HttpResponse::json(500, json!({"error": error.to_string()})),
    };
    let doctor_ok = doctor(root).map(|report| report.ok).unwrap_or(false);
    HttpResponse::json(
        200,
        json!({
            "root": root,
            "prepared": doctor_ok,
            "manifest": manifest,
            "statistics": statistics,
            "deep_index": deep_index,
            "artifacts": {
                "storage": "central_sqlite",
                "database": database_path().ok().map(|path| path.display().to_string()),
                "root_key": root_key(root).ok()
            },
            "default_agent_command": "jikji find ROOT \"query\" --json",
        }),
    )
}

const MAX_TEXT_PREVIEW_BYTES: u64 = 256 * 1024;
const MAX_BINARY_PREVIEW_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SNIPPET_CHARS: usize = 240;

fn files_response(root: &Path, query: &str) -> HttpResponse {
    let rel = query_value(query, "path").unwrap_or_else(|| ".".to_owned());
    let directory = match resolve_root_path(root, &rel) {
        Ok(path) => path,
        Err(response) => return response,
    };
    if !directory.is_dir() {
        return HttpResponse::json(400, json!({"error": "explorer path is not a directory"}));
    }
    let indexed = indexed_file_statuses(root);
    let read_dir = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(source) => return HttpResponse::json(500, json!({"error": source.to_string()})),
    };
    let mut entries = Vec::new();
    for entry in read_dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) => return HttpResponse::json(500, json!({"error": source.to_string()})),
        };
        if entry.file_name() == ".jikji" {
            continue;
        }
        let path = entry.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(source) => return HttpResponse::json(500, json!({"error": source.to_string()})),
        };
        let relative = relative_display_path(root, &path);
        let file_type = if metadata.file_type().is_symlink() {
            "symlink"
        } else if metadata.is_dir() {
            "directory"
        } else if metadata.is_file() {
            "file"
        } else {
            "other"
        };
        let scope = if file_type == "file" {
            indexed
                .get(&relative)
                .cloned()
                .unwrap_or_else(|| "unindexed".to_owned())
        } else {
            "unindexed".to_owned()
        };
        let status = if file_type == "directory" || (file_type == "file" && scope != "unindexed") {
            "current"
        } else if file_type == "file" {
            "unindexed"
        } else {
            "unsupported"
        };
        entries.push(json!({
            "path": relative,
            "name": entry.file_name().to_string_lossy(),
            "size": if metadata.is_file() { metadata.len() } else { 0 },
            "mtime": metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|duration| duration.as_secs()),
            "type": file_type,
            "status": status,
            "scope": scope,
        }));
    }
    entries.sort_by(|left, right| {
        let left_dir = left["type"] == "directory";
        let right_dir = right["type"] == "directory";
        right_dir.cmp(&left_dir).then_with(|| {
            left["name"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .cmp(&right["name"].as_str().unwrap_or("").to_lowercase())
        })
    });
    HttpResponse::json(
        200,
        json!({"root": root, "path": relative_display_path(root, &directory), "entries": entries}),
    )
}

fn indexed_file_statuses(root: &Path) -> std::collections::HashMap<String, String> {
    load_artifacts(root, "files")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| {
            let path = row.get("path")?.as_str()?.to_owned();
            let status = if row
                .get("text_cache_path")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.is_empty())
                || row
                    .get("parse_status")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| value != "metadata_only" && value != "unsupported")
            {
                "content"
            } else {
                "basic"
            };
            Some((path, status.to_owned()))
        })
        .collect()
}
fn indexed_files_response(root: &Path, query: &str) -> HttpResponse {
    let prefix = query_value(query, "path")
        .unwrap_or_default()
        .trim_matches('/')
        .to_owned();
    let mut entries = Vec::new();
    let mut folders = std::collections::BTreeSet::new();
    let statuses = indexed_file_statuses(root);
    for row in load_artifacts(root, "files").unwrap_or_default() {
        let Some(path) = row.get("path").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !(prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))) {
            continue;
        }
        let relative = path.strip_prefix(&prefix).unwrap_or(path).trim_matches('/');
        if relative.is_empty() {
            continue;
        }
        let mut parts = relative.splitn(2, '/');
        let name = parts.next().unwrap_or(relative);
        if let Some(rest) = parts.next() {
            let folder = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            folders.insert(folder);
            let _ = rest;
        } else {
            let scope = statuses
                .get(path)
                .cloned()
                .unwrap_or_else(|| "basic".to_owned());
            entries.push(json!({
                "path": path,
                "name": name,
                "type": "file",
                "status": scope,
                "scope": scope,
                "parse_status": row.get("parse_status").cloned().unwrap_or(serde_json::Value::Null),
                "indexed_at": row.get("indexed_at").cloned().unwrap_or(serde_json::Value::Null),
                "size": row.get("size").and_then(serde_json::Value::as_u64).unwrap_or(0),
            }));
        }
    }
    for folder in folders {
        let name = folder.rsplit('/').next().unwrap_or(&folder);
        entries
            .push(json!({"path": folder, "name": name, "type": "directory", "status": "indexed"}));
    }
    entries.sort_by(|left, right| {
        (right["type"] == "directory")
            .cmp(&(left["type"] == "directory"))
            .then_with(|| {
                left["name"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(right["name"].as_str().unwrap_or(""))
            })
    });
    HttpResponse::json(
        200,
        json!({"root": root, "path": prefix, "entries": entries, "source": "jikji_index"}),
    )
}
fn preview_response(state: &GuiState, query: &str) -> HttpResponse {
    let root = match requested_search_root(state, query) {
        Ok(root) => root,
        Err(response) => return response,
    };
    let Some(rel) = query_value(query, "path").or_else(|| query_value(query, "p")) else {
        return HttpResponse::json(400, json!({"error": "missing path"}));
    };
    let path = match resolve_root_path(&root, &rel) {
        Ok(path) => path,
        Err(response) => return response,
    };
    if !path.is_file() {
        return HttpResponse::json(400, json!({"error": "preview target is not a file"}));
    }
    let metadata = match path.metadata() {
        Ok(metadata) => metadata,
        Err(source) => return HttpResponse::json(500, json!({"error": source.to_string()})),
    };
    let renderer = preview_renderer(&path);
    let mut payload = json!({
        "path": relative_display_path(&root, &path),
        "type": "file",
        "size": metadata.len(),
        "mtime": metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|duration| duration.as_secs()),
        "extension": path.extension().and_then(|value| value.to_str()).unwrap_or_default().to_ascii_lowercase(),
        "renderer": renderer.name,
        "renderer_available": renderer.available,
        "media_type": renderer.media_type,
        "binary_url": format!("/api/preview/file?{}", query),
    });
    match read_text_preview(&path, metadata.len()) {
        Ok(Some(content)) => {
            let q = query_value(query, "q").unwrap_or_default();
            payload["supported"] = json!(true);
            payload["encoding"] = json!("utf-8");
            payload["matches"] = json!(match_ranges(&content, &q));
            payload["match_unit"] = json!("utf16_code_unit");
            payload["content"] = json!(content);
            payload["query"] = json!(q);
            HttpResponse::json(200, payload)
        }
        Ok(None) => {
            payload["supported"] = json!(renderer.available && renderer.media_type.is_some());
            payload["reason"] = json!(if !renderer.available {
                "renderer_unavailable"
            } else if renderer.media_type.is_some() {
                "binary"
            } else if metadata.len() > MAX_TEXT_PREVIEW_BYTES {
                "too_large"
            } else {
                "binary"
            });
            HttpResponse::json(200, payload)
        }
        Err(source) => HttpResponse::json(500, json!({"error": source.to_string()})),
    }
}

struct PreviewRenderer {
    name: &'static str,
    available: bool,
    media_type: Option<&'static str>,
}

fn preview_renderer(path: &Path) -> PreviewRenderer {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "hwp" | "hwpx" => {
            let rwhp = executable_available("rwhp");
            PreviewRenderer {
                name: if rwhp { "rwhp" } else { "libreoffice-fallback" },
                available: rwhp
                    || executable_available("libreoffice")
                    || executable_available("soffice"),
                media_type: Some("application/pdf"),
            }
        }
        "pdf" => PreviewRenderer {
            name: "browser-pdf",
            available: true,
            media_type: Some("application/pdf"),
        },
        "doc" | "docx" | "ppt" | "pptx" | "xls" | "xlsx" | "odt" | "ods" | "odp" => {
            PreviewRenderer {
                name: "libreoffice",
                available: executable_available("libreoffice") || executable_available("soffice"),
                media_type: Some("application/pdf"),
            }
        }
        "png" => PreviewRenderer {
            name: "browser-image",
            available: true,
            media_type: Some("image/png"),
        },
        "jpg" | "jpeg" => PreviewRenderer {
            name: "browser-image",
            available: true,
            media_type: Some("image/jpeg"),
        },
        "gif" => PreviewRenderer {
            name: "browser-image",
            available: true,
            media_type: Some("image/gif"),
        },
        "webp" => PreviewRenderer {
            name: "browser-image",
            available: true,
            media_type: Some("image/webp"),
        },
        _ => PreviewRenderer {
            name: "text",
            available: true,
            media_type: None,
        },
    }
}

fn executable_available(name: &str) -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        executable_on_path(name).is_some()
    }
    #[cfg(any(target_os = "macos", windows))]
    {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
    }
}

fn preview_file_response(state: &GuiState, query: &str) -> HttpResponse {
    let root = match requested_search_root(state, query) {
        Ok(root) => root,
        Err(response) => return response,
    };
    let Some(rel) = query_value(query, "path").or_else(|| query_value(query, "p")) else {
        return HttpResponse::json(400, json!({"error":"missing path"}));
    };
    let path = match resolve_root_path(&root, &rel) {
        Ok(path) => path,
        Err(response) => return response,
    };
    if !path.is_file() {
        return HttpResponse::json(400, json!({"error":"preview target is not a file"}));
    }
    let renderer = preview_renderer(&path);
    let Some(media_type) = renderer.media_type else {
        return HttpResponse::json(415, json!({"error":"no binary renderer for file"}));
    };
    if !renderer.available {
        return HttpResponse::json(
            503,
            json!({"error":"renderer unavailable","renderer":renderer.name}),
        );
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let rendered = if media_type == "application/pdf" && extension != "pdf" {
        convert_document_to_pdf(&path, renderer.name == "rwhp")
    } else {
        fs::read(&path).map_err(|error| error.to_string())
    };
    match rendered {
        Ok(body) if body.len() as u64 <= MAX_BINARY_PREVIEW_BYTES => {
            HttpResponse::binary(200, body, media_type)
        }
        Ok(_) => HttpResponse::json(413, json!({"error":"preview target is too large"})),
        Err(error) => HttpResponse::json(
            422,
            json!({"error":"preview conversion failed","renderer":renderer.name,"detail":error}),
        ),
    }
}

fn convert_document_to_pdf(path: &Path, prefer_rwhp: bool) -> std::result::Result<Vec<u8>, String> {
    let mut errors = Vec::new();
    if prefer_rwhp && executable_available("rwhp") {
        match convert_with_rwhp(path) {
            Ok(body) => return Ok(body),
            Err(error) => errors.push(format!("rwhp: {error}")),
        }
    }
    match convert_with_libreoffice(path) {
        Ok(body) => Ok(body),
        Err(error) => {
            errors.push(format!("libreoffice: {error}"));
            Err(errors.join("; "))
        }
    }
}

fn convert_with_rwhp(path: &Path) -> std::result::Result<Vec<u8>, String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let output_pdf = temp.path().join("preview.pdf");
    let attempts = [
        vec![
            "to-pdf".to_owned(),
            path.display().to_string(),
            output_pdf.display().to_string(),
        ],
        vec![
            "convert".to_owned(),
            path.display().to_string(),
            output_pdf.display().to_string(),
        ],
    ];
    let mut last_error = String::from("rwhp produced no PDF");
    for args in attempts {
        let output = Command::new("rwhp")
            .args(&args)
            .current_dir(temp.path())
            .output()
            .map_err(|error| error.to_string())?;
        if output_pdf.is_file() {
            return fs::read(&output_pdf).map_err(|error| error.to_string());
        }
        if let Some(stem) = path.file_stem() {
            let fallback = temp.path().join(stem).with_extension("pdf");
            if fallback.is_file() {
                return fs::read(fallback).map_err(|error| error.to_string());
            }
        }
        last_error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if last_error.is_empty() {
            last_error = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        }
        if last_error.is_empty() {
            last_error = format!("rwhp exited {}", output.status);
        }
    }
    Err(last_error)
}

fn libreoffice_program() -> Option<&'static str> {
    if executable_available("libreoffice") {
        Some("libreoffice")
    } else if executable_available("soffice") {
        Some("soffice")
    } else {
        None
    }
}

fn convert_with_libreoffice(path: &Path) -> std::result::Result<Vec<u8>, String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let Some(program) = libreoffice_program() else {
        return Err("libreoffice/soffice is not available".to_owned());
    };
    let output = Command::new(program)
        .args(["--headless", "--convert-to", "pdf", "--outdir"])
        .arg(temp.path())
        .arg(path)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let stem = path
        .file_stem()
        .ok_or_else(|| "preview target has no file stem".to_owned())?;
    let pdf = temp.path().join(stem).with_extension("pdf");
    fs::read(pdf).map_err(|error| error.to_string())
}

fn read_text_preview(path: &Path, size: u64) -> std::io::Result<Option<String>> {
    if size > MAX_TEXT_PREVIEW_BYTES {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(size as usize);
    File::open(path)?
        .take(MAX_TEXT_PREVIEW_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.contains(&0)
        || bytes
            .iter()
            .any(|byte| *byte < b' ' && !matches!(*byte, b'\t' | b'\n' | b'\r'))
    {
        return Ok(None);
    }
    Ok(String::from_utf8(bytes).ok())
}

fn match_ranges(content: &str, query: &str) -> Vec<serde_json::Value> {
    if query.is_empty() {
        return Vec::new();
    }
    content
        .match_indices(query)
        .map(|(byte_start, value)| {
            let start = content[..byte_start].encode_utf16().count();
            json!({"start": start, "end": start + value.encode_utf16().count()})
        })
        .collect()
}

fn relative_display_path(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    if relative.as_os_str().is_empty() {
        ".".to_owned()
    } else {
        relative.to_string_lossy().replace('\\', "/")
    }
}
fn search_response(root: &Path, query: &str) -> HttpResponse {
    let q = query_value(query, "q").unwrap_or_default();
    if q.trim().is_empty() {
        return HttpResponse::json(400, json!({"error": "missing q"}));
    }
    let top_k = query_value(query, "top_k")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, 100);
    match search(root, &q, SearchOptions { top_k }) {
        Ok(candidates) => HttpResponse::json(
            200,
            json!({"root": root, "query": q, "candidates": candidates}),
        ),
        Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
    }
}

fn discover_response(state: &GuiState, query: &str) -> HttpResponse {
    let q = query_value(query, "q").unwrap_or_default();
    if q.trim().is_empty() {
        return HttpResponse::json(400, json!({"error": "missing q"}));
    }
    let extension = query_value(query, "extension").unwrap_or_default();
    let extensions = parse_extensions(&extension);
    let top_k = query_value(query, "top_k")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, 100);
    let fetch_k = top_k;
    let handler_start = Instant::now();
    let active = match state.root() {
        Ok(root) => root,
        Err(response) => return response,
    };
    let mut prepared_status = None;
    if query_bool(query, "fresh") || query_bool(query, "auto_prepare") {
        let options = prepare_options_from_query(query);
        let stale_after_seconds = query_i64(query, "stale_after_seconds").unwrap_or(86_400);
        match crate::search_commands::maybe_prepare_for_search(
            &active,
            query_bool(query, "fresh"),
            query_bool(query, "auto_prepare") && !query_bool(query, "no_auto_prepare"),
            stale_after_seconds,
            &options,
            !query_bool(query, "no_background_refresh"),
        ) {
            Ok(mut prepared) => {
                crate::search_commands::start_deferred_background_refresh(
                    &mut prepared,
                    &active,
                    &options,
                );
                prepared_status = Some(prepared);
            }
            Err(error) => {
                return HttpResponse::json(500, json!({"error": error.to_string()}));
            }
        }
    }
    let retry_exhausted = query_bool(query, "after_jikji_retry");
    let retry_proof = query_value(query, "retry_proof").unwrap_or_default();
    let t_pre_roots = handler_start.elapsed();
    let roots = discover_roots_for_query(&active, &extensions);
    let t_after_roots = handler_start.elapsed();
    let (payloads, last_error) =
        collect_discover_payloads(&roots, &q, fetch_k, retry_exhausted, &retry_proof, &active);
    let t_after_collect = handler_start.elapsed();
    if payloads.is_empty() {
        return match last_error {
            Some(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
            None => HttpResponse::json(500, json!({"error": "no searchable roots"})),
        };
    }
    let search_root_elapsed_ms: Vec<serde_json::Value> = payloads
        .iter()
        .map(|item| {
            json!({
                "root": item.get("root").cloned().unwrap_or(json!(null)),
                "elapsed_ms": item.get("root_elapsed_ms").cloned().unwrap_or(json!(null)),
            })
        })
        .collect();
    let mut payload = merge_discover_payloads(&q, fetch_k, payloads);
    let t_after_merge = handler_start.elapsed();
    payload["mode"] = json!("find");
    payload["command"] = json!("jikji find");
    payload["search_roots"] = json!(
        roots
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    );
    payload["search_root_count"] = json!(roots.len());
    payload["search_root_elapsed_ms"] = json!(search_root_elapsed_ms);
    if let Some(prepared) = prepared_status {
        payload["index_status"] = json!(prepared.status.as_str());
        payload["foreground_prepared"] = json!(prepared.foreground_prepared);
        payload["background_refresh_started"] = json!(prepared.background_refresh_started);
    }

    filter_candidates_to_index(&active, &mut payload);
    let t_after_filter_index = handler_start.elapsed();
    filter_candidates_by_extension(&mut payload, &extension);
    let t_after_filter_ext = handler_start.elapsed();
    if query_bool(query, "first") {
        apply_first_result(&mut payload);
    } else {
        truncate_payload_candidates(&mut payload, top_k);
    }
    add_candidate_snippets(&active, &q, &mut payload);
    let t_after_snippets = handler_start.elapsed();
    let handler_ms = t_after_snippets.as_millis();
    payload["phase_timings_ms"] = json!({
        "pre_roots_ms": t_pre_roots.as_millis(),
        "roots_ms": (t_after_roots - t_pre_roots).as_millis(),
        "collect_ms": (t_after_collect - t_after_roots).as_millis(),
        "merge_ms": (t_after_merge - t_after_collect).as_millis(),
        "filter_index_ms": (t_after_filter_index - t_after_merge).as_millis(),
        "filter_ext_ms": (t_after_filter_ext - t_after_filter_index).as_millis(),
        "snippets_ms": (t_after_snippets - t_after_filter_ext).as_millis(),
        "handler_ms": handler_ms,
    });
    HttpResponse::json(200, payload)
}

fn scenario_response(state: &GuiState, query: &str) -> HttpResponse {
    let mut q = query_value(query, "query")
        .or_else(|| query_value(query, "q"))
        .unwrap_or_default();
    let mut extension = query_value(query, "extension").unwrap_or_default();
    if let Some(id) = query_value(query, "id").filter(|value| !value.is_empty()) {
        match scenarios::get(&id) {
            Some(scenario) => {
                if q.trim().is_empty() {
                    q = scenario.query.to_owned();
                }
                if extension.trim().is_empty() {
                    extension = scenario.extension.to_owned();
                }
            }
            None => {
                return HttpResponse::json(404, json!({"error":"scenario not found","id":id}));
            }
        }
    }
    if q.trim().is_empty() {
        return HttpResponse::json(400, json!({"error":"missing query"}));
    }
    let top_k = query_value(query, "top_k")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10)
        .clamp(1, 100);
    let mut discover_query = format!("q={}&top_k={top_k}", percent_encode_query(&q));
    if !extension.trim().is_empty() {
        discover_query.push_str(&format!("&extension={}", percent_encode_query(&extension)));
    }
    append_find_option_flags(&mut discover_query, query);
    discover_response(state, &discover_query)
}

fn append_find_option_flags(out: &mut String, query: &str) {
    for name in [
        "first",
        "after_jikji_retry",
        "fresh",
        "auto_prepare",
        "no_auto_prepare",
        "no_background_refresh",
        "include_hidden",
        "include_sensitive",
    ] {
        if query_bool(query, name) {
            out.push_str(&format!("&{name}=1"));
        }
    }
    if let Some(proof) = query_value(query, "retry_proof").filter(|value| !value.is_empty()) {
        out.push_str(&format!("&retry_proof={}", percent_encode_query(&proof)));
    }
    for name in [
        "exclude",
        "max_files",
        "stale_after_seconds",
        "max_hash_bytes",
        "parse_timeout",
    ] {
        for value in query_values(query, name) {
            if !value.trim().is_empty() {
                out.push_str(&format!("&{name}={}", percent_encode_query(&value)));
            }
        }
    }
}

fn percent_encode_query(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}
fn annotate_candidates_with_root(root: &Path, payload: &mut serde_json::Value) {
    let Some(candidates) = payload
        .get_mut("candidates")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    let root_value = json!(root.to_string_lossy().as_ref());
    for candidate in candidates {
        candidate["root"] = root_value.clone();
        if let Some(name) = candidate_rel_path(candidate)
            .and_then(|rel| Path::new(rel).file_name())
            .and_then(|value| value.to_str())
            .map(str::to_owned)
        {
            candidate["name"] = json!(name);
        }
    }
}

fn merge_discover_payloads(
    query: &str,
    top_k: usize,
    payloads: Vec<serde_json::Value>,
) -> serde_json::Value {
    let mut merged = payloads.first().cloned().unwrap_or_else(|| json!({}));
    let mut candidates = Vec::new();
    for payload in &payloads {
        if let Some(rows) = payload
            .get("candidates")
            .and_then(serde_json::Value::as_array)
        {
            candidates.extend(rows.iter().cloned());
        }
    }
    candidates.sort_by(|left, right| {
        let right_score = right
            .get("s")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        let left_score = left
            .get("s")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|candidate| {
        seen.insert((
            candidate
                .get("root")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            candidate_rel_path(candidate).unwrap_or_default().to_owned(),
        ))
    });
    candidates.truncate(top_k);
    let answer_paths: Vec<serde_json::Value> = candidates
        .iter()
        .filter_map(candidate_rel_path)
        .map(|path| json!(path))
        .collect();
    let has_hits = !candidates.is_empty();
    merged["query"] = json!(query);
    merged["candidates"] = json!(candidates);
    merged["answer_paths"] = json!(answer_paths);
    if has_hits {
        merged["answerability"] = json!("answerable_from_payload");
        merged["handoff_action"] = json!("direct_use");
        if merged
            .get("confidence")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|value| value == "none")
        {
            merged["confidence"] = json!("medium_high");
        }
    } else {
        merged["answerability"] = json!("no_match");
        merged["confidence"] = json!("none");
    }
    merged
}

fn candidate_rel_path(candidate: &serde_json::Value) -> Option<&str> {
    candidate
        .get("p")
        .and_then(serde_json::Value::as_str)
        .or_else(|| candidate.get("path").and_then(serde_json::Value::as_str))
}

#[cfg(test)]
fn keep_indexed_find_candidate(
    candidate: &serde_json::Value,
    indexed: &std::collections::HashMap<String, String>,
) -> bool {
    candidate_rel_path(candidate).is_some_and(|path| indexed.contains_key(path))
}

fn filter_candidates_to_index(fallback_root: &Path, payload: &mut serde_json::Value) {
    let mut indexed_by_root =
        std::collections::HashMap::<PathBuf, std::collections::HashSet<String>>::new();
    let Some(candidates) = payload
        .get_mut("candidates")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    candidates.retain(|candidate| {
        let root = candidate
            .get("root")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| fallback_root.to_path_buf());
        let Some(path) = candidate_rel_path(candidate) else {
            return false;
        };
        let indexed = indexed_by_root
            .entry(root.clone())
            .or_insert_with(|| load_artifact_paths(&root, "files").unwrap_or_default());
        indexed.contains(path)
    });
    let kept: Vec<String> = candidates
        .iter()
        .filter_map(candidate_rel_path)
        .map(str::to_owned)
        .collect();
    if let Some(answer_paths) = payload
        .get_mut("answer_paths")
        .and_then(serde_json::Value::as_array_mut)
    {
        answer_paths.retain(|value| {
            value
                .as_str()
                .is_some_and(|path| kept.iter().any(|kept_path| kept_path == path))
        });
    }
    if kept.is_empty() {
        payload["answerability"] = json!("no_match");
        payload["confidence"] = json!("none");
    }
}

fn parse_extensions(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|part| part.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|part| !part.is_empty())
        .collect()
}

fn candidate_matches_extensions(candidate: &serde_json::Value, extensions: &[String]) -> bool {
    if extensions.is_empty() {
        return true;
    }
    candidate_rel_path(candidate).is_some_and(|path| {
        Path::new(path)
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|ext| {
                let ext = ext.to_ascii_lowercase();
                extensions.iter().any(|wanted| wanted == &ext)
            })
    })
}

fn filter_candidates_by_extension(payload: &mut serde_json::Value, raw: &str) {
    let extensions = parse_extensions(raw);
    if extensions.is_empty() {
        return;
    }
    let Some(candidates) = payload
        .get_mut("candidates")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    candidates.retain(|candidate| candidate_matches_extensions(candidate, &extensions));
    let kept: Vec<String> = candidates
        .iter()
        .filter_map(candidate_rel_path)
        .map(str::to_owned)
        .collect();
    if let Some(answer_paths) = payload
        .get_mut("answer_paths")
        .and_then(serde_json::Value::as_array_mut)
    {
        answer_paths.retain(|value| {
            value
                .as_str()
                .is_some_and(|path| kept.iter().any(|kept_path| kept_path == path))
        });
    }
    if kept.is_empty() {
        payload["answerability"] = json!("no_match");
        payload["confidence"] = json!("none");
    }
}

fn truncate_payload_candidates(payload: &mut serde_json::Value, top_k: usize) {
    if let Some(candidates) = payload
        .get_mut("candidates")
        .and_then(serde_json::Value::as_array_mut)
    {
        candidates.truncate(top_k);
    }
}

fn apply_first_result(payload: &mut serde_json::Value) {
    for key in ["answer_paths", "paths", "candidates", "evidence_pack"] {
        if let Some(array) = payload
            .get_mut(key)
            .and_then(serde_json::Value::as_array_mut)
        {
            array.truncate(1);
        }
    }
    payload["first"] = json!(true);
}

fn add_candidate_snippets(root: &Path, query: &str, payload: &mut serde_json::Value) {
    let Some(candidates) = payload
        .get_mut("candidates")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for candidate in candidates {
        let Some(relative) = candidate
            .get("p")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        candidate["path"] = json!(relative);
        candidate["score"] = candidate
            .get("s")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let snippet_root = candidate
            .get("root")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| root.to_path_buf());
        let on_fuse_drive = snippet_root.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some("GoogleDrive" | "Google Drive")
            )
        });
        let snippet = if on_fuse_drive {
            candidate
                .get("ev")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        } else {
            resolve_root_path(&snippet_root, &relative)
                .ok()
                .and_then(|path| path.metadata().ok().map(|metadata| (path, metadata.len())))
                .and_then(|(path, size)| read_text_preview(&path, size).ok().flatten())
                .or_else(|| {
                    candidate
                        .get("ev")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
        }
        .map(|content| preview_snippet(&content, query));
        candidate["preview_snippet"] =
            snippet.map_or(serde_json::Value::Null, serde_json::Value::String);
    }
}

fn preview_snippet(content: &str, query: &str) -> String {
    let chars = content.chars().collect::<Vec<_>>();
    if chars.len() <= MAX_SNIPPET_CHARS {
        return content.to_owned();
    }
    let match_start = if query.is_empty() {
        0
    } else {
        content
            .find(query)
            .map(|byte| content[..byte].chars().count())
            .unwrap_or(0)
    };
    let start = match_start.saturating_sub(MAX_SNIPPET_CHARS / 3);
    let end = (start + MAX_SNIPPET_CHARS).min(chars.len());
    let mut snippet = chars[start..end].iter().collect::<String>();
    if start > 0 {
        snippet.insert(0, '…');
    }
    if end < chars.len() {
        snippet.push('…');
    }
    snippet
}

fn download_response(state: &GuiState, query: &str) -> HttpResponse {
    let root = match requested_search_root(state, query) {
        Ok(root) => root,
        Err(response) => return response,
    };
    let Some(path_value) = query_value(query, "path") else {
        return HttpResponse::json(400, json!({"error": "missing path"}));
    };
    let path = match resolve_root_path(&root, &path_value) {
        Ok(path) => path,
        Err(response) => return response,
    };
    if !path.is_file() {
        return HttpResponse::json(400, json!({"error": "download target is not a file"}));
    }
    match fs::read(&path) {
        Ok(body) => HttpResponse::binary(200, body, "application/octet-stream"),
        Err(source) => HttpResponse::json(500, json!({"error": source.to_string()})),
    }
}

fn open_response(state: &GuiState, query: &str) -> HttpResponse {
    let root = match requested_search_root(state, query) {
        Ok(root) => root,
        Err(response) => return response,
    };
    let path = match action_path(&root, query) {
        Ok(path) => path,
        Err(response) => return response,
    };
    match open_local_path(&path) {
        Ok(()) => HttpResponse::json(200, json!({"ok": true, "path": path})),
        Err(error) => HttpResponse::json(500, json!({"error": error})),
    }
}

fn reveal_response(state: &GuiState, query: &str) -> HttpResponse {
    let root = match requested_search_root(state, query) {
        Ok(root) => root,
        Err(response) => return response,
    };
    let path = match action_path(&root, query) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let reveal_path = if path.is_dir() {
        path.clone()
    } else {
        path.parent().unwrap_or(&root).to_path_buf()
    };
    match open_local_path(&reveal_path) {
        Ok(()) => HttpResponse::json(200, json!({"ok": true, "path": reveal_path})),
        Err(error) => HttpResponse::json(500, json!({"error": error})),
    }
}

fn action_path(root: &Path, query: &str) -> std::result::Result<PathBuf, HttpResponse> {
    let Some(path_value) = query_value(query, "path") else {
        return Err(HttpResponse::json(403, json!({"error": "missing path"})));
    };
    resolve_root_path(root, &path_value)
}

#[cfg(target_os = "macos")]
fn open_local_path(path: &Path) -> std::result::Result<(), String> {
    spawn_opener("open", std::iter::once(path.as_os_str()))
}

#[cfg(windows)]
fn open_local_path(path: &Path) -> std::result::Result<(), String> {
    spawn_opener("explorer.exe", std::iter::once(path.as_os_str()))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_local_path(path: &Path) -> std::result::Result<(), String> {
    if executable_on_path("xdg-open").is_some() {
        return spawn_opener("xdg-open", std::iter::once(path.as_os_str()));
    }
    if executable_on_path("gio").is_some() {
        return spawn_opener("gio", [OsStr::new("open"), path.as_os_str()]);
    }
    Err("No desktop opener found (expected xdg-open or gio)".to_owned())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn executable_on_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|path| executable_in_path(name, &path))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn executable_in_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|directory| directory.join(name))
        .find(|candidate| {
            candidate.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
}

fn spawn_opener<'a>(
    program: &str,
    args: impl IntoIterator<Item = &'a OsStr>,
) -> std::result::Result<(), String> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

const GUI_SETTINGS_KIND: &str = "gui_settings";

fn default_gui_settings() -> serde_json::Value {
    json!({"algorithm":"basic","extensions":[],"max_file_bytes":null,"exclusions":[],"include_hidden":false,"include_sensitive":false,"max_files":null,"max_hash_bytes":512_u64*1024*1024,"parse_timeout_seconds":5.0,"schedule_interval_seconds":null,"llm":{"enabled":false,"configured":false,"available":false,"reason":"disabled by default"},"last_run":{"state":"idle","started_at":null,"finished_at":null,"files":0,"error":null}})
}

fn settings_for_root(root: &Path) -> serde_json::Value {
    load_artifact(root, GUI_SETTINGS_KIND)
        .ok()
        .flatten()
        .unwrap_or_else(default_gui_settings)
}

fn settings_response(state: &GuiState, _query: &str, _mutating: bool) -> HttpResponse {
    let root = match state.root() {
        Ok(root) => root,
        Err(response) => return response,
    };
    HttpResponse::json(
        200,
        json!({"root":root,"settings":settings_for_root(&root)}),
    )
}

fn settings_mutation_response(state: &GuiState, query: &str) -> HttpResponse {
    let root = match state.root() {
        Ok(root) => root,
        Err(response) => return response,
    };
    let mut settings = settings_for_root(&root);
    let algorithm = query_value(query, "algorithm")
        .unwrap_or_else(|| settings["algorithm"].as_str().unwrap_or("basic").to_owned());
    if !matches!(algorithm.as_str(), "basic" | "content" | "deep") {
        return HttpResponse::json(
            400,
            json!({"error":"algorithm must be basic, content, or deep"}),
        );
    }
    settings["algorithm"] = json!(algorithm);
    if query_value(query, "extensions").is_some() {
        settings["extensions"] = json!(
            parse_extensions(&query_values(query, "extensions").join(","))
                .into_iter()
                .map(|e| e.trim_start_matches('.').to_owned())
                .collect::<Vec<_>>()
        );
    }
    if query_value(query, "exclude")
        .or_else(|| query_value(query, "exclusions"))
        .is_some()
    {
        settings["exclusions"] = json!(
            query_values(query, "exclude")
                .into_iter()
                .chain(query_values(query, "exclusions"))
                .flat_map(|v| v
                    .split([',', '\n'])
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
    }
    if query_value(query, "include_hidden").is_some() {
        settings["include_hidden"] = json!(query_bool(query, "include_hidden"));
    }
    if query_value(query, "include_sensitive").is_some() {
        settings["include_sensitive"] = json!(query_bool(query, "include_sensitive"));
    }
    if let Some(v) = query_usize(query, "max_files") {
        settings["max_files"] = json!(v);
    }
    if let Some(v) = query_u64(query, "max_file_bytes") {
        settings["max_file_bytes"] = json!(v.clamp(1, 16 * 1024 * 1024 * 1024));
    }
    if let Some(v) =
        query_f64(query, "parse_timeout_seconds").or_else(|| query_f64(query, "parse_timeout"))
    {
        if !v.is_finite() || !(0.1..=300.0).contains(&v) {
            return HttpResponse::json(
                400,
                json!({"error":"parse_timeout_seconds must be 0.1..300"}),
            );
        }
        settings["parse_timeout_seconds"] = json!(v);
    }
    if let Some(v) = query_u64(query, "schedule_interval_seconds") {
        settings["schedule_interval_seconds"] = json!(if v == 0 {
            serde_json::Value::Null
        } else {
            json!(v.clamp(1, 86400))
        });
    }
    if let Err(error) = store_artifact(&root, GUI_SETTINGS_KIND, settings.clone()) {
        return HttpResponse::json(500, json!({"error":error.to_string()}));
    }
    HttpResponse::json(200, json!({"root":root,"settings":settings,"applied":true}))
}

fn exclude_selection_response(state: &GuiState, query: &str) -> HttpResponse {
    let root = match state.root() {
        Ok(root) => root,
        Err(response) => return response,
    };
    let mut settings = settings_for_root(&root);
    let mut exclusions = settings["exclusions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let excluded = query_bool(query, "excluded");
    let mut changed = Vec::new();
    for raw in query_values(query, "path") {
        let rel = raw.trim_matches('/');
        if rel.is_empty() || rel.contains("..") {
            return HttpResponse::json(400, json!({"error":"invalid relative path"}));
        }
        let value = json!(rel);
        if excluded {
            if !exclusions.iter().any(|v| v == &value) {
                exclusions.push(value);
                changed.push(rel.to_owned());
            }
        } else if let Some(i) = exclusions.iter().position(|v| v == &value) {
            exclusions.remove(i);
            changed.push(rel.to_owned());
        }
    }
    settings["exclusions"] = json!(exclusions);
    if let Err(error) = store_artifact(&root, GUI_SETTINGS_KIND, settings.clone()) {
        return HttpResponse::json(500, json!({"error":error.to_string()}));
    }
    HttpResponse::json(
        200,
        json!({"root":root,"exclusions":settings["exclusions"],"changed":changed}),
    )
}

fn prepare_options_from_settings(settings: &serde_json::Value) -> PrepareOptions {
    PrepareOptions {
        include_hidden: settings["include_hidden"].as_bool().unwrap_or(false),
        include_sensitive: settings["include_sensitive"].as_bool().unwrap_or(false),
        max_files: settings["max_files"]
            .as_u64()
            .and_then(|v| usize::try_from(v).ok()),
        extensions: settings["extensions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        max_file_bytes: settings["max_file_bytes"].as_u64(),
        max_hash_bytes: settings["max_hash_bytes"]
            .as_u64()
            .unwrap_or(512 * 1024 * 1024),
        parse_timeout_seconds: settings["parse_timeout_seconds"].as_f64().unwrap_or(5.0),
        exclude_patterns: settings["exclusions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        deep_archive_index: settings["algorithm"].as_str() == Some("deep"),
        ..PrepareOptions::default()
    }
}

fn refresh_response(state: &GuiState, query: &str) -> HttpResponse {
    let root = match state.root() {
        Ok(root) => root,
        Err(response) => return response,
    };
    let settings = settings_for_root(&root);
    prepare_operation_response(
        state,
        query,
        prepare_options_from_settings(&settings),
        settings["algorithm"].as_str() == Some("deep"),
    )
}

fn reindex_response(state: &GuiState, query: &str) -> HttpResponse {
    refresh_response(state, query)
}

fn prepare_options_from_query(query: &str) -> PrepareOptions {
    let mut options = PrepareOptions {
        include_hidden: query_bool(query, "include_hidden"),
        include_sensitive: query_bool(query, "include_sensitive"),
        max_files: crate::prepare_commands::normalize_max_files(query_usize(query, "max_files")),
        ..Default::default()
    };
    let exclude = parse_exclude_patterns(query);
    if !exclude.is_empty() {
        options.exclude_patterns = exclude;
    }
    if let Some(max_hash_bytes) = query_u64(query, "max_hash_bytes") {
        options.max_hash_bytes = max_hash_bytes;
    }
    if let Some(parse_timeout) = query_f64(query, "parse_timeout") {
        options.parse_timeout_seconds = parse_timeout;
    }
    options
}

fn parse_exclude_patterns(query: &str) -> Vec<String> {
    query_values(query, "exclude")
        .into_iter()
        .flat_map(|value| {
            value
                .split([',', '\n'])
                .map(|part| part.trim().to_string())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn doctor_response(root: &Path) -> HttpResponse {
    match doctor(root) {
        Ok(report) => HttpResponse::json(200, json!(report)),
        Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
    }
}

fn map_response(root: &Path) -> HttpResponse {
    match read_map(root) {
        Ok(markdown) => HttpResponse::json(200, json!({"root": root, "markdown": markdown})),
        Err(error) => HttpResponse::json(404, json!({"error": error.to_string()})),
    }
}

fn graph_response(root: &Path, query: &str) -> HttpResponse {
    let command = query_value(query, "command").unwrap_or_else(|| "status".to_owned());
    match command.as_str() {
        "status" => HttpResponse::json(200, graph_status(root)),
        "query" => {
            let q = query_value(query, "q").unwrap_or_default();
            let top_k = query_value(query, "top_k")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(20)
                .clamp(1, 100);
            match graph_query(root, &q, top_k) {
                Ok(candidates) => HttpResponse::json(
                    200,
                    json!({"root": root, "query": q, "candidates": candidates}),
                ),
                Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
            }
        }
        "explain" => {
            let path = query_value(query, "path").unwrap_or_default();
            HttpResponse::json(200, explain_source(root, &path))
        }
        _ => HttpResponse::json(400, json!({"error": "unknown graph command"})),
    }
}

fn brief_response(root: &Path, query: &str) -> HttpResponse {
    let q = query_value(query, "q").unwrap_or_default();
    if q.trim().is_empty() {
        return HttpResponse::json(400, json!({"error": "missing q"}));
    }
    let top_k = query_value(query, "top_k")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10)
        .clamp(1, 100);
    let stale_after_seconds = query_value(query, "stale_after_seconds")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(24 * 60 * 60);
    let status = search_index_status(root, stale_after_seconds);
    match search(root, &q, SearchOptions { top_k }) {
        Ok(candidates) => HttpResponse::json(
            200,
            brief_payload(
                root,
                &q,
                status.status.as_str(),
                BriefOptions {
                    top_k,
                    foreground_prepared: false,
                    background_refresh_started: false,
                },
                &candidates,
            ),
        ),
        Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
    }
}

fn clean_response(state: &GuiState, query: &str) -> HttpResponse {
    let root = match state.root() {
        Ok(root) => root,
        Err(response) => return response,
    };
    let force = query_bool(query, "force");
    let dry_run = query_flag(query, "dry_run").unwrap_or(!force);
    match clean(&root, CleanOptions { dry_run, force }) {
        Ok(result) => HttpResponse::json(200, json!(result)),
        Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
    }
}

fn prepare_operation_response(
    state: &GuiState,
    query: &str,
    options: PrepareOptions,
    deep: bool,
) -> HttpResponse {
    if !query_bool(query, "async") {
        return prepare_active_root(state, options);
    }
    let root = match state.root() {
        Ok(root) => root,
        Err(response) => return response,
    };
    let job_options = options.clone();
    let job_id = state.jobs.start(move |_| {
        let result = prepare(&root, &job_options).map_err(|error| error.to_string())?;
        let payload = json!({"root":root,"files":result.files,"documents":result.docs_parsed,"deep":deep});
        if deep {
            store_artifact(&root, "deep_index_status", json!({"state":"completed","root":root,"files":result.files,"documents":result.docs_parsed,"entries":result.files,"elapsed_ms":0,"estimated_cost":"bounded","media_index":job_options.enable_media_index,"deep_archive_index":true})).map_err(|error| error.to_string())?;
        }
        Ok(payload)
    });
    HttpResponse::json(202, json!({"job_id":job_id,"state":"queued","progress":0}))
}

fn deep_index_response(state: &GuiState, query: &str) -> HttpResponse {
    let media_enabled = query_bool(query, "media_ocr") || query_bool(query, "media_asr");
    let options = PrepareOptions {
        enable_media_index: media_enabled,
        media_index_max_mb: query_f64(query, "media_max_mb")
            .unwrap_or(25.0)
            .clamp(1.0, 4096.0),
        deep_archive_index: true,
        archive_max_entries: query_usize(query, "archive_max_entries")
            .unwrap_or(1_000)
            .clamp(1, 100_000),
        archive_max_entry_bytes: query_u64(query, "archive_max_entry_bytes")
            .unwrap_or(16 * 1024 * 1024)
            .clamp(1, 512 * 1024 * 1024),
        archive_max_total_bytes: query_u64(query, "archive_max_total_bytes")
            .unwrap_or(128 * 1024 * 1024)
            .clamp(1, 4 * 1024 * 1024 * 1024),
        ..PrepareOptions::default()
    };
    if query_bool(query, "async") {
        return prepare_operation_response(state, query, options, true);
    }
    let started = Instant::now();
    with_root(state, |root| match prepare(root, &options) {
        Ok(result) => {
            let bytes = load_artifacts(root, "files")
                .ok()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| row.get("size").and_then(serde_json::Value::as_u64))
                        .sum::<u64>()
                })
                .unwrap_or(0);
            let status = json!({"state":"completed","root":root,"files":result.files,"documents":result.docs_parsed,"entries":result.files,"bytes":bytes,"elapsed_ms":started.elapsed().as_millis(),"estimated_cost":if media_enabled {"high"} else {"bounded"},"media_index":media_enabled,"deep_archive_index":true});
            match store_artifact(root, "deep_index_status", status) {
                Ok(()) => root_status(root),
                Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
            }
        }
        Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
    })
}

fn query_u64(query: &str, name: &str) -> Option<u64> {
    query_value(query, name)?.parse().ok()
}
fn query_usize(query: &str, name: &str) -> Option<usize> {
    query_u64(query, name).and_then(|value| usize::try_from(value).ok())
}
fn query_f64(query: &str, name: &str) -> Option<f64> {
    query_value(query, name)?.parse().ok()
}
fn query_i64(query: &str, name: &str) -> Option<i64> {
    query_value(query, name)?.parse().ok()
}

fn folder_query_path(
    root: &Path,
    query: &str,
) -> std::result::Result<(String, PathBuf), HttpResponse> {
    let rel = query_value(query, "path")
        .ok_or_else(|| HttpResponse::json(400, json!({"error": "missing path"})))?;
    let path = resolve_root_path(root, &rel)?;
    if !path.is_dir() {
        return Err(HttpResponse::json(
            400,
            json!({"error": "path is not a directory"}),
        ));
    }
    Ok((rel.trim_matches('/').to_owned(), path))
}

fn index_selection_response(state: &GuiState, query: &str) -> HttpResponse {
    let paths = query_values(query, "path");
    if paths.is_empty() {
        return HttpResponse::json(400, json!({"error":"missing path"}));
    }
    let mode = query_value(query, "mode").unwrap_or_else(|| "basic".to_owned());
    if mode != "basic" && mode != "content" {
        return HttpResponse::json(400, json!({"error":"mode must be basic or content"}));
    }
    let root = match state.root() {
        Ok(root) => root,
        Err(response) => return response,
    };
    for path in &paths {
        if let Err(response) = resolve_root_path(&root, path) {
            return response;
        }
    }
    let mut options = PrepareOptions::default();
    if mode == "content" {
        options.enable_media_index =
            query_bool(query, "media_ocr") || query_bool(query, "media_asr");
        options.deep_archive_index = true;
        options.archive_max_entries = query_usize(query, "archive_max_entries")
            .unwrap_or(options.archive_max_entries)
            .clamp(1, 100_000);
        options.archive_max_entry_bytes = query_u64(query, "archive_max_entry_bytes")
            .unwrap_or(options.archive_max_entry_bytes)
            .clamp(1, 512 * 1024 * 1024);
        options.archive_max_total_bytes = query_u64(query, "archive_max_total_bytes")
            .unwrap_or(options.archive_max_total_bytes)
            .clamp(1, 4 * 1024 * 1024 * 1024);
    }
    let selected = paths.len();
    let job_mode = mode.clone();
    let job_id = state.jobs.start(move |_| {
        let result = prepare(&root, &options).map_err(|error| error.to_string())?;
        Ok(json!({"root":root,"mode":job_mode,"selected":selected,"files":result.files,"documents":result.docs_parsed}))
    });
    HttpResponse::json(
        202,
        json!({"job_id":job_id,"state":"queued","selected":selected,"mode":mode,"progress":0}),
    )
}

fn reindex_folder_response(state: &GuiState, query: &str) -> HttpResponse {
    with_root(state, |root| {
        let (rel, _) = match folder_query_path(root, query) {
            Ok(v) => v,
            Err(r) => return r,
        };
        match prepare(
            root,
            &prepare_options_from_settings(&settings_for_root(root)),
        ) {
            Ok(result) => HttpResponse::json(
                200,
                json!({"root":root,"path":rel,"action":"reindex","state":"completed","files":result.files,"documents":result.docs_parsed,"statistics":root_statistics(root).ok()}),
            ),
            Err(error) => HttpResponse::json(
                500,
                json!({"error":error.to_string(),"root":root,"path":rel,"action":"reindex","state":"failed"}),
            ),
        }
    })
}

fn remove_folder_response(state: &GuiState, query: &str) -> HttpResponse {
    with_root(state, |root| {
        let (rel, _) = match folder_query_path(root, query) {
            Ok(v) => v,
            Err(r) => return r,
        };
        match remove_artifacts_under(root, &rel) {
            Ok(removed) => HttpResponse::json(
                200,
                json!({"root":root,"path":rel,"action":"remove","state":"completed","removed":removed,"source_preserved":true,"statistics":root_statistics(root).ok()}),
            ),
            Err(error) => HttpResponse::json(500, json!({"error":error.to_string()})),
        }
    })
}

fn deep_index_target_response(state: &GuiState, query: &str) -> HttpResponse {
    with_root(state, |root| {
        let (rel, _) = match folder_query_path(root, query) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let enabled = query_value(query, "enabled").is_none_or(|v| v != "false")
            && query_bool(query, "enabled");
        let mut status = load_artifact(root, "deep_index_targets")
            .ok()
            .flatten()
            .unwrap_or_else(|| json!({"targets":[]}));
        let Some(targets) = status.get_mut("targets").and_then(|v| v.as_array_mut()) else {
            return HttpResponse::json(500, json!({"error":"invalid deep index target state"}));
        };
        targets.retain(|v| v.as_str() != Some(rel.as_str()));
        if enabled {
            targets.push(json!(rel.clone()));
        }
        match store_artifact(root, "deep_index_targets", status) {
            Ok(()) => HttpResponse::json(
                200,
                json!({"root":root,"path":rel,"action":"deep-index-target","state":if enabled {"enabled"} else {"disabled"},"enabled":enabled,"statistics":root_statistics(root).ok()}),
            ),
            Err(error) => HttpResponse::json(500, json!({"error":error.to_string()})),
        }
    })
}

fn prepare_active_root(state: &GuiState, options: PrepareOptions) -> HttpResponse {
    with_root(state, |root| match prepare(root, &options) {
        Ok(_) => match clear_artifact(root, "deep_index_status") {
            Ok(()) => root_status(root),
            Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
        },
        Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
    })
}

fn root_switch_response(state: &GuiState, query: &str) -> HttpResponse {
    let root = match requested_root(query) {
        Ok(root) => root,
        Err(response) => return response,
    };
    let result = if query_bool(query, "prepare") {
        prepare(
            &root,
            &prepare_options_from_settings(&settings_for_root(&root)),
        )
        .map(|_| ())
    } else {
        migrate_legacy(&root).and_then(|_| register_root(&root).map(|_| ()))
    };
    if let Err(error) = result {
        return HttpResponse::json(500, json!({"error": error.to_string()}));
    }
    match state.switch_root(root.clone()) {
        Ok(()) => root_status(&root),
        Err(response) => response,
    }
}

fn remove_root_response(state: &GuiState, query: &str) -> HttpResponse {
    let Some(path) = query_value(query, "path") else {
        return HttpResponse::json(400, json!({"error": "missing path"}));
    };
    let canonical = PathBuf::from(&path)
        .canonicalize()
        .map(|root| root.to_string_lossy().into_owned())
        .unwrap_or(path);
    let active = match state.root() {
        Ok(active) => active,
        Err(response) => return response,
    };
    let roots = match indexed_roots() {
        Ok(roots) => roots,
        Err(error) => return HttpResponse::json(500, json!({"error": error.to_string()})),
    };
    let replacement = roots
        .iter()
        .map(|entry| entry.root.clone())
        .find(|candidate| candidate.to_string_lossy() != canonical && candidate.is_dir());
    if active.to_string_lossy() == canonical && replacement.is_none() {
        return HttpResponse::json(400, json!({"error": "cannot remove the only active root"}));
    }
    match delete_root_by_key(&canonical) {
        Ok(removed) => {
            if active.to_string_lossy() == canonical
                && let Some(next) = replacement
                && let Err(response) = state.switch_root(next)
            {
                return response;
            }
            let active_root = match state.root() {
                Ok(active_root) => active_root,
                Err(response) => return response,
            };
            HttpResponse::json(
                200,
                json!({"ok": true, "removed": removed, "root": canonical, "active_root": active_root}),
            )
        }
        Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
    }
}

fn requested_root(query: &str) -> std::result::Result<PathBuf, HttpResponse> {
    let Some(path) = query_value(query, "path") else {
        return Err(HttpResponse::json(400, json!({"error": "missing path"})));
    };
    match PathBuf::from(path).canonicalize() {
        Ok(root) if root.is_dir() => Ok(root),
        Ok(root) => Err(HttpResponse::json(
            400,
            json!({"error": format!("path is not a directory: {}", root.display())}),
        )),
        Err(source) => Err(HttpResponse::json(
            400,
            json!({"error": source.to_string()}),
        )),
    }
}

fn resolve_root_path(root: &Path, rel_path: &str) -> std::result::Result<PathBuf, HttpResponse> {
    let candidate = Path::new(rel_path);
    if rel_path.trim().is_empty()
        || candidate.is_absolute()
        || candidate
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(HttpResponse::json(
            403,
            json!({"error": "path traversal is not allowed"}),
        ));
    }
    let joined = root.join(candidate);
    let resolved = joined
        .canonicalize()
        .map_err(|source| HttpResponse::json(404, json!({"error": source.to_string()})))?;
    if !path_is_within_root(root, &resolved) {
        return Err(HttpResponse::json(
            403,
            json!({"error": "path escapes root"}),
        ));
    }
    Ok(resolved)
}

fn path_is_within_root(root: &Path, resolved: &Path) -> bool {
    if resolved.starts_with(root) {
        return true;
    }
    match root.canonicalize() {
        Ok(canonical) => resolved.starts_with(canonical),
        Err(_) => false,
    }
}

#[cfg(all(test, unix, not(target_os = "macos")))]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::executable_in_path;

    #[test]
    fn executable_lookup_skips_non_executable_candidates() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir(&first).expect("first dir");
        fs::create_dir(&second).expect("second dir");

        let blocked = first.join("xdg-open");
        fs::write(&blocked, "not executable").expect("blocked fixture");
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o644)).expect("blocked mode");

        let executable = second.join("xdg-open");
        fs::write(&executable, "#!/bin/sh\n").expect("executable fixture");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("executable mode");

        let search_path = std::env::join_paths([&first, &second]).expect("search path");
        assert_eq!(
            executable_in_path("xdg-open", &search_path),
            Some(executable)
        );
    }
}

#[cfg(test)]
mod find_filter_tests {
    use super::{
        append_find_option_flags, apply_first_result, candidate_matches_extensions,
        candidate_rel_path, keep_indexed_find_candidate, keep_listed_root, parse_exclude_patterns,
        parse_extensions, paths_look_like_same_root, paths_refer_to_same_root,
        percent_encode_query, prepare_options_from_query, preview_renderer,
    };
    use std::collections::HashMap;

    use serde_json::json;

    #[test]
    fn indexed_find_candidate_does_not_require_full_query_substring() {
        let mut indexed = HashMap::new();
        indexed.insert("notes/cascade.md".to_owned(), "content".to_owned());
        let hit = json!({"p": "notes/cascade.md", "s": 12.5});
        let unindexed = json!({"p": "tmp/unindexed.md", "s": 99.0});
        let path_only = json!({"path": "notes/cascade.md"});

        assert_eq!(candidate_rel_path(&hit), Some("notes/cascade.md"));
        assert!(keep_indexed_find_candidate(&hit, &indexed));
        assert!(keep_indexed_find_candidate(&path_only, &indexed));
        assert!(!keep_indexed_find_candidate(&unindexed, &indexed));
        assert!(!keep_indexed_find_candidate(&json!({"s": 1.0}), &indexed));
    }

    #[test]
    fn nested_searchable_root_is_kept_when_parent_is_selected() {
        use super::push_unique_search_root;
        use std::path::PathBuf;

        let parent = PathBuf::from("/tmp/jikji-drive-parent");
        let nested = parent.join("마커").join("nested-root");
        let mut roots = Vec::new();
        push_unique_search_root(&mut roots, &parent);
        let mut extras = vec![parent.clone(), nested.clone()];
        extras.sort_by_key(|path| path.components().count());
        for path in extras {
            push_unique_search_root(&mut roots, &path);
        }
        assert!(
            roots.iter().any(|path| path == &nested),
            "nested searchable root must stay searchable after parent is selected: {roots:?}"
        );
    }

    #[test]
    fn drop_redundant_nested_extras_keeps_nested_when_parent_is_active() {
        use super::drop_redundant_nested_search_roots;
        use std::path::PathBuf;

        let active = PathBuf::from("/tmp/jikji-drive-parent");
        let nested = active.join("마커");
        let other = PathBuf::from("/tmp/jikji-downloads");
        let filtered = drop_redundant_nested_search_roots(
            vec![active.clone(), nested.clone(), other.clone()],
            &active,
        );
        assert_eq!(filtered, vec![active, nested, other]);
    }

    #[test]
    fn drop_redundant_nested_extras_skips_child_when_parent_is_also_extra() {
        use super::drop_redundant_nested_search_roots;
        use std::path::PathBuf;

        let active = PathBuf::from("/tmp/jikji-active-root");
        let parent = PathBuf::from("/tmp/jikji-drive-parent");
        let nested = parent.join("마커");
        let filtered = drop_redundant_nested_search_roots(
            vec![active.clone(), parent.clone(), nested],
            &active,
        );
        assert_eq!(filtered, vec![active, parent]);
    }

    #[test]
    fn filter_roots_by_matching_extensions_keeps_active_and_nested_hits() {
        use super::filter_roots_by_matching_extensions;
        use std::path::PathBuf;

        let active = PathBuf::from("/tmp/jikji-active-root");
        let nested = PathBuf::from("/tmp/jikji-drive-parent/마커/nested-root");
        let other = PathBuf::from("/tmp/jikji-no-hwp");
        let roots = vec![active.clone(), nested.clone(), other];
        let matching = vec![nested.clone()];
        let filtered = filter_roots_by_matching_extensions(roots, &active, &matching);
        assert_eq!(filtered, vec![active, nested]);
    }

    #[test]
    fn parse_exclude_patterns_splits_commas_and_repeats() {
        let values = parse_exclude_patterns("exclude=tmp/**&exclude=.git,node_modules");
        assert_eq!(
            values,
            vec![
                "tmp/**".to_owned(),
                ".git".to_owned(),
                "node_modules".to_owned()
            ]
        );
    }

    #[test]
    fn prepare_options_from_query_maps_find_prepare_flags() {
        let options = prepare_options_from_query(
            "include_hidden=1&include_sensitive=1&max_files=12&exclude=tmp/**&parse_timeout=2.5&max_hash_bytes=1024",
        );
        assert!(options.include_hidden);
        assert!(options.include_sensitive);
        assert_eq!(options.max_files, Some(12));
        assert_eq!(options.exclude_patterns, vec!["tmp/**".to_owned()]);
        assert_eq!(options.parse_timeout_seconds, 2.5);
        assert_eq!(options.max_hash_bytes, 1024);
    }

    #[test]
    fn append_find_option_flags_forwards_retry_proof_and_exclude() {
        let mut out = String::from("q=hwp&top_k=3");
        append_find_option_flags(
            &mut out,
            "after_jikji_retry=1&retry_proof=proof-1&exclude=tmp/**&fresh=0",
        );
        assert!(out.contains("after_jikji_retry=1"));
        assert!(out.contains("retry_proof=proof-1"));
        assert!(out.contains(&format!("exclude={}", percent_encode_query("tmp/**"))));
        assert!(!out.contains("fresh=1"));
    }

    #[test]
    fn append_find_option_flags_forwards_first_auto_prepare_and_max_hash_bytes() {
        let mut out = String::from("q=hello&top_k=5");
        append_find_option_flags(
            &mut out,
            "first=1&auto_prepare=1&no_auto_prepare=1&max_hash_bytes=1024",
        );
        assert!(out.contains("first=1"));
        assert!(out.contains("auto_prepare=1"));
        assert!(out.contains("no_auto_prepare=1"));
        assert!(out.contains("max_hash_bytes=1024"));
    }

    #[test]
    fn apply_first_result_truncates_find_payload_arrays() {
        let mut payload = json!({
            "candidates": [{"p": "a"}, {"p": "b"}],
            "paths": ["a", "b"],
            "answer_paths": ["a", "b"],
            "evidence_pack": [1, 2],
        });
        apply_first_result(&mut payload);
        assert_eq!(payload["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(payload["paths"].as_array().unwrap().len(), 1);
        assert_eq!(payload["answer_paths"].as_array().unwrap().len(), 1);
        assert_eq!(payload["evidence_pack"].as_array().unwrap().len(), 1);
        assert_eq!(payload["first"], json!(true));
    }

    #[test]
    fn keep_listed_root_does_not_require_missing_drive_child_to_exist() {
        use std::path::PathBuf;
        let active = PathBuf::from("/tmp/jikji-active-root");
        let drive = PathBuf::from("/nonexistent-jikji-home/GoogleDrive/마커");
        assert!(
            keep_listed_root(&drive, &active),
            "indexed Drive children must stay listed without STATing FUSE"
        );
    }

    #[test]
    fn extension_filter_keeps_matching_document_types() {
        let extensions = parse_extensions(".hwp,.hwpx");
        assert_eq!(extensions, vec!["hwp".to_owned(), "hwpx".to_owned()]);
        let hwp = json!({"p": "docs/memo.hwp"});
        let pdf = json!({"path": "docs/memo.pdf"});
        assert!(candidate_matches_extensions(&hwp, &extensions));
        assert!(!candidate_matches_extensions(&pdf, &extensions));
        assert!(candidate_matches_extensions(&pdf, &[]));
    }

    #[test]
    fn searchable_roots_match_without_canonicalize() {
        use std::path::Path;
        assert!(paths_refer_to_same_root(
            Path::new("/home/cheol/GoogleDrive/마커"),
            Path::new("/home/cheol/GoogleDrive/마커")
        ));
        assert!(paths_refer_to_same_root(
            Path::new("/home/cheol/GoogleDrive/마커/"),
            Path::new("/home/cheol/GoogleDrive/마커")
        ));
        assert!(!paths_refer_to_same_root(
            Path::new("/home/cheol/projects/jikji"),
            Path::new("/home/cheol/GoogleDrive/마커")
        ));
    }

    #[test]
    fn lite_root_check_does_not_stat_missing_or_fuse_paths() {
        use std::path::Path;
        assert!(paths_look_like_same_root(
            Path::new("/home/cheol/GoogleDrive/마커"),
            Path::new("/home/cheol/GoogleDrive/마커/")
        ));
        assert!(!paths_look_like_same_root(
            Path::new("/home/cheol/projects/jikji"),
            Path::new("/home/cheol/GoogleDrive/마커")
        ));
        assert!(!paths_look_like_same_root(
            Path::new("/does/not/exist/a"),
            Path::new("/does/not/exist/b")
        ));
    }

    #[test]
    fn preview_renderer_maps_hwp_to_pdf() {
        use std::path::Path;
        let renderer = preview_renderer(Path::new("memo.hwp"));
        assert_eq!(renderer.media_type, Some("application/pdf"));
        assert!(renderer.name == "rwhp" || renderer.name == "libreoffice-fallback");
        let pdf = preview_renderer(Path::new("memo.pdf"));
        assert_eq!(pdf.name, "browser-pdf");
        assert_eq!(pdf.media_type, Some("application/pdf"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_root_path_accepts_symlink_root() {
        use super::resolve_root_path;
        use std::fs;
        let temp = tempfile::tempdir().expect("tempdir");
        let real = temp.path().join("real");
        let link = temp.path().join("link");
        fs::create_dir(&real).expect("real dir");
        fs::write(real.join("doc.txt"), "ok").expect("doc");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        let resolved = resolve_root_path(&link, "doc.txt").unwrap_or_else(|_| panic!("resolve"));
        assert_eq!(
            resolved,
            real.join("doc.txt").canonicalize().expect("canonical doc")
        );
    }
}
