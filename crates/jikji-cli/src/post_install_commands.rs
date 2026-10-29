use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use jikji_core::{JikjiError, PrepareOptions};
use jikji_index::prepare;
use serde_json::{Value, json};

use crate::args::PostInstallPrepareArgs;
use crate::output::print_json;
use crate::post_install_background::start_background_post_install_prepare;
use crate::prepare_commands::normalize_max_files;

const COMMON_RELS: &[&str] = &[
    "Documents",
    "Downloads",
    "Desktop",
    "문서",
    "다운로드",
    "바탕화면",
    "데스크탑",
    "OneDrive/Documents",
    "OneDrive/문서",
    "Google Drive",
    "GoogleDrive",
    "Dropbox",
    "iCloud Drive",
    "Downloads/Telegram Desktop",
    "Downloads/KakaoTalk Downloads",
    "Downloads/카카오톡 받은 파일",
    "Documents/KakaoTalk Downloads",
    "Documents/카카오톡 받은 파일",
];
const CLOUD_LIBRARY_DIR_NAMES: &[&str] = &["GoogleDrive", "Google Drive"];
const CLOUD_SKIP_CHILDREN: &[&str] = &["kaggle", "node_modules"];
const MAX_LIBRARY_ROOTS: usize = 24;

const DOCUMENT_EXTS: &[&str] = &[
    "pdf", "hwp", "hwpx", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "rtf", "odt", "ods", "odp",
];

#[cfg(test)]
static DOCUMENT_HEAVY_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static DOCUMENT_HEAVY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) struct PostInstallRequest {
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) no_prepare: bool,
    pub(crate) foreground: bool,
    pub(crate) parse_timeout: f64,
    pub(crate) max_files: Option<usize>,
}

pub(crate) fn prepare_after_skill_install(
    request: PostInstallRequest,
) -> jikji_core::Result<Value> {
    if request.no_prepare {
        return Ok(json!({"mode": "disabled", "roots": []}));
    }
    let (selected_roots, selection) = if request.roots.is_empty() {
        select_default_roots()
    } else {
        let selected = dedupe_roots(request.roots.iter().cloned());
        (
            selected.clone(),
            json!({"source": "explicit_prepare_root", "common_roots": selected, "document_heavy_roots": []}),
        )
    };
    if selected_roots.is_empty() {
        return Ok(json!({"mode": "none", "roots": [], "selection": selection}));
    }
    if !request.foreground {
        return Ok(start_background_post_install_prepare(
            &request,
            &selected_roots,
            selection,
        ));
    }
    let mut prepared = Vec::new();
    for root in &selected_roots {
        let result = match prepare(
            root,
            &PrepareOptions {
                max_files: normalize_max_files(request.max_files),
                parse_timeout_seconds: request.parse_timeout,
                exclude_patterns: child_dir_excludes(root, &selected_roots),
                ..PrepareOptions::default()
            },
        ) {
            Ok(result) => result,
            Err(JikjiError::Locked(lock_path)) => {
                prepared.push(json!({
                    "root": root,
                    "ok": false,
                    "error": "locked",
                    "lock": lock_path.to_string_lossy(),
                }));
                continue;
            }
            Err(error) => return Err(error),
        };
        jikji_agent::write_routing_blocks(root)?;
        prepared.push(json!({
            "root": result.root,
            "ok": true,
            "files": result.files,
            "agent_map": result.agent_map,
        }));
    }
    Ok(json!({
        "mode": "foreground",
        "roots": prepared,
        "parse_timeout": request.parse_timeout,
        "selection": selection,
    }))
}

pub(crate) fn run_post_install_prepare(
    args: PostInstallPrepareArgs,
) -> jikji_core::Result<ExitCode> {
    let payload = prepare_after_skill_install(PostInstallRequest {
        roots: args.roots,
        no_prepare: false,
        foreground: true,
        parse_timeout: args.parse_timeout,
        max_files: args.max_files,
    })?;
    if args.json {
        print_json(&payload)?;
    } else {
        println!("{payload}");
    }
    Ok(ExitCode::SUCCESS)
}

fn select_default_roots() -> (Vec<PathBuf>, Value) {
    select_default_roots_from(&post_install_home())
}

pub(crate) fn default_library_roots() -> Vec<PathBuf> {
    select_default_roots_from(&post_install_home()).0
}

pub(crate) fn library_search_prefixes(home: &Path) -> Vec<PathBuf> {
    COMMON_RELS.iter().map(|rel| home.join(rel)).collect()
}

pub(crate) fn is_library_search_path(path: &Path, home: &Path) -> bool {
    if path == home {
        return false;
    }
    library_search_prefixes(home)
        .iter()
        .any(|prefix| path == prefix || path.starts_with(prefix))
}

fn select_default_roots_from(home: &Path) -> (Vec<PathBuf>, Value) {
    let common_roots = common_roots(home);
    let document_roots = document_heavy_roots(home, &common_roots);
    let roots = dedupe_roots(common_roots.iter().chain(document_roots.iter()).cloned());
    (
        roots,
        json!({
            "source": "auto_common_and_document_roots",
            "home": home,
            "common_roots": common_roots,
            "document_heavy_roots": document_roots,
            "document_extensions": DOCUMENT_EXTS,
        }),
    )
}

pub(crate) fn post_install_home() -> PathBuf {
    let home = std::env::var_os("JIKJI_POST_INSTALL_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("JIKJI_AGENT_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    home.canonicalize().unwrap_or(home)
}

fn common_roots(home: &Path) -> Vec<PathBuf> {
    expand_cloud_library_roots(dedupe_roots(
        COMMON_RELS
            .iter()
            .map(|rel| home.join(rel))
            .filter(|path| path.is_dir()),
    ))
}

fn document_heavy_roots(home: &Path, covered_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut context = ScanContext {
        home,
        covered_roots,
        roots: &mut roots,
    };
    scan_document_heavy_dirs(home, &mut context, &mut ScanBudget::default());
    dedupe_roots(roots)
}

struct ScanContext<'a> {
    home: &'a Path,
    covered_roots: &'a [PathBuf],
    roots: &'a mut Vec<PathBuf>,
}

#[derive(Default)]
struct ScanBudget {
    dirs_seen: usize,
    files_seen: usize,
}

fn scan_document_heavy_dirs(dir: &Path, context: &mut ScanContext<'_>, budget: &mut ScanBudget) {
    if budget.dirs_seen > 4_000
        || budget.files_seen > 60_000
        || is_under_any(dir, context.covered_roots)
    {
        return;
    }
    budget.dirs_seen += 1;
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut document_files = 0usize;
    let mut child_dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !name.starts_with('.') && name != "node_modules" && name != ".jikji" {
                child_dirs.push(path);
            }
            continue;
        }
        budget.files_seen += 1;
        let ext = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if DOCUMENT_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
            document_files += 1;
        }
    }
    if document_files >= 3 && dir != context.home {
        context.roots.push(dir.to_path_buf());
    }
    for child in child_dirs {
        scan_document_heavy_dirs(&child, context, budget);
    }
}

fn expand_cloud_library_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut expanded = Vec::new();
    for root in roots {
        if is_cloud_library_root(&root) {
            expanded.extend(cloud_child_roots(&root));
        }
        expanded.push(root);
    }
    expanded
}

fn is_cloud_library_root(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| CLOUD_LIBRARY_DIR_NAMES.contains(&name))
}

fn cloud_child_roots(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut children: Vec<(bool, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            is_dir.then_some(entry.path())
        })
        .filter(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            !name.starts_with('.') && !CLOUD_SKIP_CHILDREN.contains(&name)
        })
        .map(|path| (cloud_child_looks_document_heavy(&path), path))
        .collect();
    children.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.file_name().cmp(&right.1.file_name()))
    });
    children.into_iter().map(|(_, path)| path).collect()
}

fn cloud_child_looks_document_heavy(path: &Path) -> bool {
    #[cfg(test)]
    DOCUMENT_HEAVY_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .path()
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                DOCUMENT_EXTS
                    .iter()
                    .any(|item| item.eq_ignore_ascii_case(ext))
            })
    })
}

fn child_dir_excludes(root: &Path, selected: &[PathBuf]) -> Vec<String> {
    selected
        .iter()
        .filter(|other| other.as_path() != root && other.starts_with(root))
        .filter_map(|other| {
            other
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .collect()
}

fn dedupe_roots(roots: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut selected = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let canonical = root.canonicalize().unwrap_or(root);
        if selected.iter().any(|existing| existing == &canonical) {
            continue;
        }
        selected.push(canonical);
        if selected.len() >= MAX_LIBRARY_ROOTS {
            break;
        }
    }
    selected
}

fn is_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|root| path == root || path.starts_with(root))
}

pub(crate) fn enqueue_missing_library_root_prepares() {
    let selected = default_library_roots();
    if selected.is_empty() {
        return;
    }
    let indexed = jikji_core::storage::indexed_roots().unwrap_or_default();
    let missing: Vec<PathBuf> = selected
        .into_iter()
        .filter(|root| {
            !indexed
                .iter()
                .any(|item| item.root == *root && item.statistics.files > 0)
        })
        .collect();
    if missing.is_empty() {
        return;
    }
    let request = PostInstallRequest {
        roots: missing.clone(),
        no_prepare: false,
        foreground: false,
        parse_timeout: 5.0,
        max_files: None,
    };
    let selection = json!({
        "source": "gui_library_roots",
        "common_roots": missing,
        "document_heavy_roots": [],
    });
    let _ = start_background_post_install_prepare(&request, &missing, selection);
}

#[cfg(test)]
mod tests {
    use super::{
        child_dir_excludes, common_roots, is_library_search_path, select_default_roots_from,
    };
    use std::fs;

    #[test]
    fn select_default_roots_includes_google_drive_without_space() {
        let _guard = super::DOCUMENT_HEAVY_TEST_LOCK
            .lock()
            .expect("document-heavy test lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path();
        fs::create_dir(home.join("Documents")).expect("documents");
        fs::create_dir(home.join("GoogleDrive")).expect("google drive");
        fs::write(home.join("Documents").join("brief.pdf"), "%PDF").expect("pdf");
        let (roots, selection) = select_default_roots_from(home);
        assert!(
            roots.iter().any(|root| root.ends_with("GoogleDrive")),
            "expected GoogleDrive library root, got {roots:?}"
        );
        assert!(
            roots.iter().any(|root| root.ends_with("Documents")),
            "expected Documents library root, got {roots:?}"
        );
        assert_eq!(selection["source"], "auto_common_and_document_roots");
        let common = common_roots(home);
        assert!(common.iter().any(|root| root.ends_with("GoogleDrive")));
    }

    #[test]
    fn select_default_roots_expands_google_drive_children() {
        let _guard = super::DOCUMENT_HEAVY_TEST_LOCK
            .lock()
            .expect("document-heavy test lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path();
        fs::create_dir(home.join("Documents")).expect("documents");
        fs::create_dir(home.join("GoogleDrive")).expect("google drive");
        fs::create_dir(home.join("GoogleDrive").join("마커")).expect("marker");
        fs::create_dir(home.join("GoogleDrive").join("kaggle")).expect("kaggle");
        fs::create_dir(home.join("GoogleDrive").join(".hidden")).expect("hidden");
        fs::write(
            home.join("GoogleDrive").join("마커").join("note.pdf"),
            "%PDF",
        )
        .expect("pdf");
        let (roots, _) = select_default_roots_from(home);
        assert!(
            roots.iter().any(|root| root.ends_with("마커")),
            "expected GoogleDrive child library root, got {roots:?}"
        );
        assert!(
            roots.iter().any(|root| root.ends_with("GoogleDrive")),
            "expected GoogleDrive parent library root, got {roots:?}"
        );
        assert!(
            !roots.iter().any(|root| root.ends_with("kaggle")),
            "kaggle must not become a library root: {roots:?}"
        );
        assert!(
            !roots.iter().any(|root| root.ends_with(".hidden")),
            "hidden cloud children must not become library roots: {roots:?}"
        );
        let marker = home
            .join("GoogleDrive")
            .join("마커")
            .canonicalize()
            .expect("marker");
        let parent = home.join("GoogleDrive").canonicalize().expect("parent");
        let excludes = child_dir_excludes(&parent, &roots);
        assert!(
            excludes.iter().any(|name| name == "마커"),
            "parent prepare must exclude expanded children, got {excludes:?}"
        );
        assert!(child_dir_excludes(&marker, &roots).is_empty());
    }

    #[test]
    fn cloud_child_sort_stats_each_child_once() {
        use std::sync::atomic::Ordering;

        let _guard = super::DOCUMENT_HEAVY_TEST_LOCK
            .lock()
            .expect("document-heavy test lock");

        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path();
        fs::create_dir(home.join("Documents")).expect("documents");
        fs::write(home.join("Documents").join("brief.pdf"), "%PDF").expect("pdf");
        let drive = home.join("GoogleDrive");
        fs::create_dir(&drive).expect("google drive");
        let child_count = 20usize;
        for index in 0..child_count {
            let child = drive.join(format!("child-{index:02}"));
            fs::create_dir(&child).expect("child");
            if index % 2 == 0 {
                fs::write(child.join("note.pdf"), "%PDF").expect("pdf");
            }
        }
        super::DOCUMENT_HEAVY_CALLS.store(0, Ordering::Relaxed);
        let (roots, _) = select_default_roots_from(home);
        let calls = super::DOCUMENT_HEAVY_CALLS.load(Ordering::Relaxed);
        assert_eq!(
            calls, child_count,
            "document-heavy checks must run once per cloud child, got {calls} for {roots:?}"
        );
        let cloud_children: Vec<_> = roots
            .iter()
            .filter(|root| {
                root.parent()
                    .is_some_and(|parent| parent.ends_with("GoogleDrive"))
                    && root.file_name().is_some_and(|name| name != "GoogleDrive")
            })
            .collect();
        let heavy = cloud_children
            .iter()
            .take(child_count / 2)
            .filter(|root| {
                root.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.strip_prefix("child-")
                            .and_then(|value| value.parse::<usize>().ok())
                            .is_some_and(|index| index % 2 == 0)
                    })
            })
            .count();
        assert_eq!(
            heavy,
            child_count / 2,
            "document-heavy cloud children should sort first, got {cloud_children:?}"
        );
    }

    #[test]
    fn select_default_roots_does_not_index_home_as_a_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path();
        for name in ["a.pdf", "b.pdf", "c.pdf"] {
            fs::write(home.join(name), "%PDF").expect("home pdf");
        }
        let (roots, _) = select_default_roots_from(home);
        assert!(
            !roots.iter().any(|root| root == home),
            "home itself must not be a default library root: {roots:?}"
        );
    }

    #[test]
    fn library_search_path_matches_missing_google_drive_child() {
        let home = std::path::PathBuf::from("/nonexistent-jikji-home");
        assert!(is_library_search_path(
            &home.join("GoogleDrive").join("마커"),
            &home
        ));
        assert!(is_library_search_path(&home.join("Documents"), &home));
        assert!(is_library_search_path(
            &home.join("Google Drive").join("논문"),
            &home
        ));
        assert!(!is_library_search_path(&home, &home));
        assert!(!is_library_search_path(
            &std::path::PathBuf::from("/tmp/jikji-x"),
            &home
        ));
    }
}
