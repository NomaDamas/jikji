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
use std::time::UNIX_EPOCH;

use jikji_core::PrepareOptions;
use jikji_core::storage::{
    clear_artifact, database_path, delete_root_by_key, indexed_roots, load_artifact,
    load_artifacts, migrate_legacy, register_root, remove_artifacts_under, root_key,
    root_statistics, store_artifact,
};
use jikji_index::{doctor, prepare};
use jikji_search::{DiscoverOptions, SearchOptions, discover, search};
use serde_json::json;

use super::http::{HttpRequest, HttpResponse, malformed_request, query_bool, query_value};
use super::token::ManagementToken;

#[derive(Clone)]
pub(crate) struct GuiState {
    root: Arc<RwLock<PathBuf>>,
    mutation: Arc<Mutex<()>>,
    manage_token: ManagementToken,
}

impl GuiState {
    pub(crate) fn new(root: PathBuf, manage_token: ManagementToken) -> Self {
        Self {
            root: Arc::new(RwLock::new(root)),
            mutation: Arc::new(Mutex::new(())),
            manage_token,
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
        ("GET", "/api/roots") => roots_response(state),
        ("GET", "/api/files") => with_root(state, |root| files_response(root, &request.query)),
        ("GET", "/api/search") => with_root(state, |root| search_response(root, &request.query)),
        ("GET", "/api/find") | ("GET", "/api/discover") => {
            with_root(state, |root| discover_response(root, &request.query))
        }
        ("GET", "/api/preview") => with_root(state, |root| preview_response(root, &request.query)),
        ("GET", "/download") => with_root(state, |root| download_response(root, &request.query)),
        ("POST", "/open") => management_response(state, &request.query, open_response),
        ("POST", "/reveal") => management_response(state, &request.query, reveal_response),
        ("POST", "/api/refresh") => management_response(state, &request.query, refresh_response),
        ("POST", "/api/reindex-folder") => {
            management_response(state, &request.query, reindex_folder_response)
        }
        ("POST" | "DELETE", "/api/remove-folder") => {
            management_response(state, &request.query, remove_folder_response)
        }
        ("POST" | "DELETE", "/api/deep-index-target") => {
            management_response(state, &request.query, deep_index_target_response)
        }
        ("POST", "/api/reindex") => management_response(state, &request.query, reindex_response),
        ("POST", "/api/deep-index") => {
            management_response(state, &request.query, deep_index_response)
        }
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
    match indexed_roots() {
        Ok(roots) => HttpResponse::json(200, json!({"active_root": active_root, "roots": roots})),
        Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
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

const MAX_PREVIEW_BYTES: u64 = 256 * 1024;
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
        let status = if file_type == "file" {
            indexed
                .get(&relative)
                .map(String::as_str)
                .unwrap_or("unindexed")
        } else if file_type == "directory" {
            "current"
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
            Some((
                row.get("path")?.as_str()?.to_owned(),
                row.get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("current")
                    .to_owned(),
            ))
        })
        .collect()
}

fn preview_response(root: &Path, query: &str) -> HttpResponse {
    let Some(rel) = query_value(query, "path") else {
        return HttpResponse::json(400, json!({"error": "missing path"}));
    };
    let path = match resolve_root_path(root, &rel) {
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
    let mut payload = json!({
        "path": relative_display_path(root, &path),
        "type": "file",
        "size": metadata.len(),
        "mtime": metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|duration| duration.as_secs()),
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
            payload["supported"] = json!(false);
            payload["reason"] = json!(if metadata.len() > MAX_PREVIEW_BYTES {
                "too_large"
            } else {
                "binary"
            });
            HttpResponse::json(200, payload)
        }
        Err(source) => HttpResponse::json(500, json!({"error": source.to_string()})),
    }
}

fn read_text_preview(path: &Path, size: u64) -> std::io::Result<Option<String>> {
    if size > MAX_PREVIEW_BYTES {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(size as usize);
    File::open(path)?
        .take(MAX_PREVIEW_BYTES + 1)
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

fn discover_response(root: &Path, query: &str) -> HttpResponse {
    let q = query_value(query, "q").unwrap_or_default();
    if q.trim().is_empty() {
        return HttpResponse::json(400, json!({"error": "missing q"}));
    }
    let top_k = query_value(query, "top_k")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, 100);
    match discover(
        root,
        &q,
        DiscoverOptions {
            top_k,
            ..DiscoverOptions::default()
        },
    ) {
        Ok(mut payload) => {
            payload["mode"] = json!("find");
            payload["command"] = json!("jikji find");
            add_candidate_snippets(root, &q, &mut payload);
            HttpResponse::json(200, payload)
        }
        Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
    }
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
        let snippet = resolve_root_path(root, &relative)
            .ok()
            .and_then(|path| path.metadata().ok().map(|metadata| (path, metadata.len())))
            .and_then(|(path, size)| read_text_preview(&path, size).ok().flatten())
            .map(|content| preview_snippet(&content, query))
            .or_else(|| {
                candidate
                    .get("ev")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            });
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

fn download_response(root: &Path, query: &str) -> HttpResponse {
    let Some(path_value) = query_value(query, "path") else {
        return HttpResponse::json(400, json!({"error": "missing path"}));
    };
    let path = match resolve_root_path(root, &path_value) {
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
    with_root(state, |root| {
        let path = match action_path(root, query) {
            Ok(path) => path,
            Err(response) => return response,
        };
        match open_local_path(&path) {
            Ok(()) => HttpResponse::json(200, json!({"ok": true, "path": path})),
            Err(error) => HttpResponse::json(500, json!({"error": error})),
        }
    })
}

fn reveal_response(state: &GuiState, query: &str) -> HttpResponse {
    with_root(state, |root| {
        let path = match action_path(root, query) {
            Ok(path) => path,
            Err(response) => return response,
        };
        let reveal_path = if path.is_dir() {
            path.clone()
        } else {
            path.parent().unwrap_or(root).to_path_buf()
        };
        match open_local_path(&reveal_path) {
            Ok(()) => HttpResponse::json(200, json!({"ok": true, "path": reveal_path})),
            Err(error) => HttpResponse::json(500, json!({"error": error})),
        }
    })
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
        .map_err(|source| source.to_string())
}

fn refresh_response(state: &GuiState, _query: &str) -> HttpResponse {
    prepare_active_root(state, PrepareOptions::default())
}

fn reindex_response(state: &GuiState, _query: &str) -> HttpResponse {
    prepare_active_root(state, PrepareOptions::default())
}

fn deep_index_response(state: &GuiState, _query: &str) -> HttpResponse {
    let options = PrepareOptions {
        enable_media_index: true,
        deep_archive_index: true,
        ..PrepareOptions::default()
    };
    with_root(state, |root| match prepare(root, &options) {
        Ok(result) => {
            let status = json!({"state":"completed","root":root,"files":result.files,"documents":result.docs_parsed,"media_index":true,"deep_archive_index":true});
            match store_artifact(root, "deep_index_status", status) {
                Ok(()) => root_status(root),
                Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
            }
        }
        Err(error) => HttpResponse::json(500, json!({"error": error.to_string()})),
    })
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

fn reindex_folder_response(state: &GuiState, query: &str) -> HttpResponse {
    with_root(state, |root| {
        let (rel, _) = match folder_query_path(root, query) {
            Ok(v) => v,
            Err(r) => return r,
        };
        match prepare(root, &PrepareOptions::default()) {
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
        prepare(&root, &PrepareOptions::default()).map(|_| ())
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
    if !resolved.starts_with(root) {
        return Err(HttpResponse::json(
            403,
            json!({"error": "path escapes root"}),
        ));
    }
    Ok(resolved)
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
