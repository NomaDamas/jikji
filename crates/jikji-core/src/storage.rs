use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use rusqlite::{Connection, OptionalExtension, Transaction, params, params_from_iter};
use serde::Serialize;
use serde_json::Value;

use crate::{JIKJI_DIR, Result, io_error, json_error};
const DATABASE_SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootStatistics {
    pub files: usize,
    pub folders: usize,
    pub documents: usize,
    pub chunks: usize,
    pub parse_errors: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndexedRoot {
    pub root: PathBuf,
    pub updated_at: i64,
    pub artifact_count: usize,
    pub statistics: RootStatistics,
    pub deep_index: Option<Value>,
}

static INITIALIZED_DATABASES: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn canonical_root(root: &Path) -> Result<PathBuf> {
    root.canonicalize().map_err(|source| io_error(root, source))
}

pub fn data_dir() -> Result<PathBuf> {
    if let Some(path) = env::var_os("JIKJI_DATA_DIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    #[cfg(target_os = "windows")]
    let base = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| {
        env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".local/share"))
    });
    base.ok_or_else(|| {
        io_error(
            "JIKJI_DATA_DIR",
            Error::new(ErrorKind::NotFound, "OS user data directory is unavailable"),
        )
    })
}

pub fn database_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("jikji/index.sqlite"))
}

pub fn open_database() -> Result<Connection> {
    let path = database_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    }
    let connection = Connection::open(&path).map_err(|source| sqlite_error(&path, source))?;
    connection
        .busy_timeout(std::time::Duration::from_secs(30))
        .map_err(|source| sqlite_error(&path, source))?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|source| sqlite_error(&path, source))?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(|source| sqlite_error(&path, source))?;
    let initialized = &INITIALIZED_DATABASES;
    let mut initialized = initialized
        .lock()
        .map_err(|_| io_error(&path, Error::other("database initialization lock poisoned")))?;
    if initialized.insert(path.clone()) {
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|source| sqlite_error(&path, source))?;
        initialize(&connection, &path)?;
    }
    Ok(connection)
}

pub fn root_key(root: &Path) -> Result<String> {
    Ok(canonical_root(root)?.to_string_lossy().into_owned())
}

pub fn ensure_root(connection: &Connection, root: &Path) -> Result<i64> {
    if let Some(id) = lookup_root_id_by_keys(connection, &unresolved_root_keys(root))? {
        return Ok(id);
    }
    let canonical = registration_root_key(root)?;
    connection.execute(
        "INSERT INTO roots(canonical_root, updated_at) VALUES(?1, unixepoch()) ON CONFLICT(canonical_root) DO UPDATE SET updated_at=unixepoch()",
        [&canonical],
    ).map_err(sqlite_error_path)?;
    connection
        .query_row(
            "SELECT id FROM roots WHERE canonical_root=?1",
            [&canonical],
            |row| row.get(0),
        )
        .map_err(sqlite_error_path)
}

pub fn register_root(root: &Path) -> Result<i64> {
    let connection = open_database()?;
    ensure_root(&connection, root)
}

pub fn root_id(connection: &Connection, root: &Path) -> Result<Option<i64>> {
    lookup_root_id_by_keys(connection, &unresolved_root_keys(root))
}

/// Reject a new prepare root when it overlaps a populated searchable root.
/// Exact refreshes of an existing populated root remain allowed.
pub fn ensure_prepare_root_allowed(root: &Path) -> Result<()> {
    let connection = open_database()?;
    let canonical = canonical_root(root)?;
    let has_search_docs: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='search_docs')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(sqlite_error_path)?
        != 0;
    if !has_search_docs {
        return Ok(());
    }
    let exact_populated: bool = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM roots r
                JOIN search_docs d ON d.root_id = r.id
                WHERE r.canonical_root=?1
            )",
            [canonical.to_string_lossy().into_owned()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(sqlite_error_path)?
        != 0;
    if exact_populated {
        return Ok(());
    }
    let mut statement = connection
        .prepare(
            "SELECT r.canonical_root FROM roots r
             WHERE r.id IN (SELECT DISTINCT root_id FROM search_docs)",
        )
        .map_err(sqlite_error_path)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sqlite_error_path)?;
    for row in rows {
        let existing = PathBuf::from(row.map_err(sqlite_error_path)?);
        if existing == canonical {
            continue;
        }
        if existing.starts_with(&canonical) || canonical.starts_with(&existing) {
            return Err(io_error(
                root,
                Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "prepare root overlaps existing indexed root: {}",
                        existing.display()
                    ),
                ),
            ));
        }
    }
    Ok(())
}

fn unresolved_root_keys(root: &Path) -> Vec<String> {
    let mut keys = Vec::new();
    push_unresolved_key(&mut keys, root);
    if let Ok(absolute) = std::path::absolute(root) {
        push_unresolved_key(&mut keys, &absolute);
    }
    if let Some(target) = read_link_target(root) {
        push_unresolved_key(&mut keys, &target);
        if let Ok(absolute) = std::path::absolute(&target) {
            push_unresolved_key(&mut keys, &absolute);
        }
    }
    // A typed path may differ from the registered (canonical) key only in
    // Unicode normalization: macOS stores NFD names, typed paths are usually
    // NFC. Add both spellings without touching the file system.
    for key in keys.clone() {
        use unicode_normalization::UnicodeNormalization;
        for variant in [key.nfc().collect::<String>(), key.nfd().collect::<String>()] {
            push_unresolved_key(&mut keys, Path::new(&variant));
        }
    }
    keys
}

fn push_unresolved_key(keys: &mut Vec<String>, path: &Path) {
    let raw = path.to_string_lossy();
    let sep = std::path::MAIN_SEPARATOR;
    let mut trimmed = raw.as_ref();
    while trimmed.len() > 1 && trimmed.ends_with(sep) {
        trimmed = &trimmed[..trimmed.len() - 1];
    }
    if !trimmed.is_empty() && !keys.iter().any(|key| key == trimmed) {
        keys.push(trimmed.to_string());
    }
}

fn read_link_target(path: &Path) -> Option<PathBuf> {
    let target = fs::read_link(path).ok()?;
    if target.is_absolute() {
        return Some(target);
    }
    Some(path.parent().unwrap_or(path).join(target))
}

pub fn path_looks_like_cloud_drive(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(is_cloud_drive_component_name)
    })
}

pub fn cloud_drive_path_blocked(path: &Path) -> bool {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir => {
                prefix.pop();
            }
            _ => prefix.push(component.as_os_str()),
        }
        if path_looks_like_cloud_drive(&prefix) {
            return true;
        }
        if let Some(target) = read_link_target(&prefix)
            && path_looks_like_cloud_drive(&target)
        {
            return true;
        }
    }
    false
}

fn is_cloud_drive_component_name(name: &str) -> bool {
    matches!(
        normalize_cloud_drive_component(name).as_str(),
        "googledrive" | "gdrive"
    )
}

fn normalize_cloud_drive_component(name: &str) -> String {
    name.chars()
        .filter(|ch| !matches!(ch, ' ' | '-' | '_'))
        .flat_map(char::to_lowercase)
        .collect()
}

fn registration_root_key(root: &Path) -> Result<String> {
    let keys = unresolved_root_keys(root);
    if keys
        .iter()
        .any(|key| cloud_drive_path_blocked(Path::new(key)))
    {
        if let Some(key) = keys
            .iter()
            .rev()
            .find(|key| Path::new(key.as_str()).is_absolute())
        {
            return Ok(key.clone());
        }
        if let Some(key) = keys.first() {
            return Ok(key.clone());
        }
        return Err(io_error(
            root,
            Error::new(ErrorKind::InvalidInput, "root path is empty"),
        ));
    }
    root_key(root)
}

fn lookup_root_id_by_keys(connection: &Connection, keys: &[String]) -> Result<Option<i64>> {
    for key in keys {
        let found = connection
            .query_row(
                "SELECT id FROM roots WHERE canonical_root=?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error_path)?;
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(None)
}

pub fn indexed_roots() -> Result<Vec<IndexedRoot>> {
    let connection = open_database()?;
    let mut statement = connection
        .prepare("SELECT id, canonical_root, updated_at FROM roots ORDER BY updated_at DESC, canonical_root")
        .map_err(sqlite_error_path)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(sqlite_error_path)?;
    let mut roots = Vec::new();
    for row in rows {
        let (root_id, canonical_root, updated_at) = row.map_err(sqlite_error_path)?;
        roots.push(indexed_root(
            &connection,
            root_id,
            canonical_root,
            updated_at,
        )?);
    }
    Ok(roots)
}

pub fn searchable_root_paths() -> Result<Vec<PathBuf>> {
    let connection = open_database()?;
    let mut statement = connection
        .prepare(
            "SELECT canonical_root FROM roots
             WHERE id IN (SELECT DISTINCT root_id FROM search_docs)
             ORDER BY canonical_root",
        )
        .map_err(sqlite_error_path)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sqlite_error_path)?;
    rows.map(|row| Ok(PathBuf::from(row.map_err(sqlite_error_path)?)))
        .collect()
}

pub fn searchable_root_paths_for_extensions(extensions: &[String]) -> Result<Vec<PathBuf>> {
    let normalized = normalize_search_extensions(extensions);
    if normalized.is_empty() {
        return searchable_root_paths();
    }
    let connection = open_database()?;
    let placeholders = vec!["?"; normalized.len()].join(", ");
    let sql = format!(
        "SELECT DISTINCT r.canonical_root FROM roots r
         JOIN search_docs d ON d.root_id = r.id
         WHERE lower(CASE WHEN d.ext LIKE '.%' THEN d.ext ELSE '.' || d.ext END) IN ({placeholders})
         ORDER BY r.canonical_root"
    );
    let mut statement = connection.prepare(&sql).map_err(sqlite_error_path)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(normalized.iter()), |row| {
            row.get::<_, String>(0)
        })
        .map_err(sqlite_error_path)?;
    rows.map(|row| Ok(PathBuf::from(row.map_err(sqlite_error_path)?)))
        .collect()
}

fn normalize_search_extensions(extensions: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for ext in extensions {
        let trimmed = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        if trimmed.is_empty() {
            continue;
        }
        let dotted = format!(".{trimmed}");
        if !out.contains(&dotted) {
            out.push(dotted);
        }
    }
    out
}

pub fn root_statistics(root: &Path) -> Result<RootStatistics> {
    let connection = open_database()?;
    let Some(root_id) = root_id(&connection, root)? else {
        return Ok(empty_statistics());
    };
    statistics_for_id(&connection, root_id)
}

pub fn root_storage_dir(root: &Path) -> Result<PathBuf> {
    let connection = open_database()?;
    let id = ensure_root(&connection, root)?;
    Ok(data_dir()?.join("jikji/roots").join(id.to_string()))
}

pub fn replace_artifacts(root: &Path, artifacts: &[(&str, Value)]) -> Result<()> {
    let mut connection = open_database()?;
    let tx = connection.transaction().map_err(sqlite_error_path)?;
    let root_id = ensure_root_tx(&tx, root)?;
    let mut kinds = artifacts.iter().map(|(kind, _)| *kind).collect::<Vec<_>>();
    kinds.sort_unstable();
    kinds.dedup();
    for kind in kinds {
        tx.execute(
            "DELETE FROM artifacts WHERE root_id=?1 AND kind=?2",
            params![root_id, kind],
        )
        .map_err(sqlite_error_path)?;
    }
    for (kind, value) in artifacts {
        let raw = serde_json::to_string(value)
            .map_err(|source| json_error(database_path().unwrap_or_default(), source))?;
        tx.execute(
            "INSERT INTO artifacts(root_id, kind, row_json) VALUES(?1, ?2, ?3)",
            params![root_id, kind, raw],
        )
        .map_err(sqlite_error_path)?;
    }
    tx.commit().map_err(sqlite_error_path)
}

pub fn load_artifacts(root: &Path, kind: &str) -> Result<Vec<Value>> {
    migrate_legacy(root)?;
    let connection = open_database()?;
    let Some(root_id) = root_id(&connection, root)? else {
        return Ok(Vec::new());
    };
    let mut statement = connection
        .prepare("SELECT row_json FROM artifacts WHERE root_id=?1 AND kind=?2 ORDER BY ordinal")
        .map_err(sqlite_error_path)?;
    let rows = statement
        .query_map(params![root_id, kind], |row| row.get::<_, String>(0))
        .map_err(sqlite_error_path)?;
    rows.map(|row| {
        let raw = row.map_err(sqlite_error_path)?;
        serde_json::from_str(&raw)
            .map_err(|source| json_error(database_path().unwrap_or_default(), source))
    })
    .collect()
}

pub fn load_artifact(root: &Path, kind: &str) -> Result<Option<Value>> {
    Ok(load_artifacts(root, kind)?.into_iter().next())
}

pub fn load_artifact_by_path(root: &Path, kind: &str, path: &str) -> Result<Option<Value>> {
    migrate_legacy(root)?;
    let connection = open_database()?;
    let Some(root_id) = root_id(&connection, root)? else {
        return Ok(None);
    };

    let mut statement = connection
        .prepare(
            "SELECT row_json FROM artifacts WHERE root_id=?1 AND kind=?2 AND json_extract(row_json, '$.path')=?3 ORDER BY ordinal LIMIT 1",
        )
        .map_err(sqlite_error_path)?;

    let raw = statement
        .query_row(params![root_id, kind, path], |row| row.get::<_, String>(0))
        .optional()
        .map_err(sqlite_error_path)?;

    raw.map(|raw| {
        serde_json::from_str(&raw)
            .map_err(|source| json_error(database_path().unwrap_or_default(), source))
    })
    .transpose()
}

pub fn load_artifact_paths(root: &Path, kind: &str) -> Result<HashSet<String>> {
    migrate_legacy(root)?;
    let connection = open_database()?;
    let Some(root_id) = root_id(&connection, root)? else {
        return Ok(HashSet::new());
    };

    let mut statement = connection
        .prepare(
            "SELECT json_extract(row_json, '$.path') FROM artifacts WHERE root_id=?1 AND kind=?2",
        )
        .map_err(sqlite_error_path)?;

    let paths = statement
        .query_map(params![root_id, kind], |row| {
            row.get::<_, Option<String>>(0)
        })
        .map_err(sqlite_error_path)?
        .filter_map(|value| value.ok().flatten().filter(|path| !path.is_empty()))
        .collect();
    Ok(paths)
}

pub fn load_artifacts_matching_terms(
    root: &Path,
    kind: &str,
    terms: &[String],
    limit: usize,
) -> Result<Vec<Value>> {
    if terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    migrate_legacy(root)?;
    let connection = open_database()?;
    let Some(root_id) = root_id(&connection, root)? else {
        return Ok(Vec::new());
    };

    let mut sql = String::from("SELECT row_json FROM artifacts WHERE root_id=?1 AND kind=?2 AND (");
    for index in 0..terms.len() {
        if index > 0 {
            sql.push_str(" OR ");
        }
        sql.push_str(&format!("instr(lower(row_json), ?{}) > 0", index + 3));
    }
    sql.push_str(&format!(") ORDER BY ordinal LIMIT ?{}", terms.len() + 3));

    let mut values = vec![
        rusqlite::types::Value::Integer(root_id),
        rusqlite::types::Value::Text(kind.to_owned()),
    ];
    for term in terms {
        values.push(rusqlite::types::Value::Text(term.to_lowercase()));
    }
    values.push(rusqlite::types::Value::Integer(limit as i64));

    let mut statement = connection.prepare(&sql).map_err(sqlite_error_path)?;
    let rows = statement
        .query_map(params_from_iter(values), |row| row.get::<_, String>(0))
        .map_err(sqlite_error_path)?;
    rows.map(|row| {
        let raw = row.map_err(sqlite_error_path)?;
        serde_json::from_str(&raw)
            .map_err(|source| json_error(database_path().unwrap_or_default(), source))
    })
    .collect()
}
pub fn store_artifact(root: &Path, kind: &str, value: Value) -> Result<()> {
    replace_artifacts(root, &[(kind, value)])
}

pub fn clear_artifact(root: &Path, kind: &str) -> Result<()> {
    let connection = open_database()?;
    let Some(root_id) = root_id(&connection, root)? else {
        return Ok(());
    };
    connection
        .execute(
            "DELETE FROM artifacts WHERE root_id=?1 AND kind=?2",
            params![root_id, kind],
        )
        .map_err(sqlite_error_path)?;
    Ok(())
}
/// Remove only central-index rows below a root-relative path; source files remain untouched.
/// Rebuilding the retained rows in one transaction preserves per-root isolation and atomicity.
pub fn remove_artifacts_under(root: &Path, prefix: &str) -> Result<usize> {
    let mut connection = open_database()?;
    let Some(root_id) = root_id(&connection, root)? else {
        return Ok(0);
    };
    let normalized = prefix.trim_matches('/');
    let tx = connection.transaction().map_err(sqlite_error_path)?;
    let mut statement = tx
        .prepare("SELECT kind, row_json FROM artifacts WHERE root_id=?1 ORDER BY kind, ordinal")
        .map_err(sqlite_error_path)?;
    let rows = statement
        .query_map([root_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sqlite_error_path)?;
    let mut retained = Vec::new();
    let mut removed = 0usize;
    for row in rows {
        let (kind, raw) = row.map_err(sqlite_error_path)?;
        let value: Value = serde_json::from_str(&raw)
            .map_err(|source| json_error(database_path().unwrap_or_default(), source))?;
        let path = value
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let matches = normalized.is_empty()
            || path == normalized
            || path.starts_with(&format!("{normalized}/"));
        if matches {
            removed += 1;
        } else {
            retained.push((kind, raw));
        }
    }
    drop(statement);
    tx.execute("DELETE FROM artifacts WHERE root_id=?1", [root_id])
        .map_err(sqlite_error_path)?;
    for (kind, raw) in retained {
        tx.execute(
            "INSERT INTO artifacts(root_id, kind, row_json) VALUES(?1, ?2, ?3)",
            rusqlite::params![root_id, kind, raw],
        )
        .map_err(sqlite_error_path)?;
    }
    tx.commit().map_err(sqlite_error_path)?;
    Ok(removed)
}
pub fn delete_root(root: &Path) -> Result<bool> {
    let canonical = root_key(root)?;
    delete_root_by_key(&canonical)
}

pub fn delete_root_by_key(canonical: &str) -> Result<bool> {
    let mut connection = open_database()?;
    let root_id = connection
        .query_row(
            "SELECT id FROM roots WHERE canonical_root=?1",
            [canonical],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(sqlite_error_path)?;
    let Some(root_id) = root_id else {
        return Ok(false);
    };
    remove_root_cache(root_id)?;
    let tx = connection.transaction().map_err(sqlite_error_path)?;
    for table in [
        "artifacts",
        "search_meta",
        "search_docs",
        "search_terms",
        "search_filename_keys",
        "search_idf",
        "search_field_terms",
        "search_field_lengths",
        "search_field_idf",
        "search_field_avg",
    ] {
        let exists = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sqlite_error_path)?;
        if exists != 0 {
            tx.execute(&format!("DELETE FROM {table} WHERE root_id=?1"), [root_id])
                .map_err(sqlite_error_path)?;
        }
    }
    tx.execute("DELETE FROM roots WHERE id=?1", [root_id])
        .map_err(sqlite_error_path)?;
    tx.commit().map_err(sqlite_error_path)?;
    Ok(true)
}

pub fn migrate_legacy(root: &Path) -> Result<bool> {
    let connection = open_database()?;
    if root_id(&connection, root)?.is_some() {
        return Ok(false);
    }
    drop(connection);
    let legacy = root.join(JIKJI_DIR);
    if !legacy.is_dir() {
        return Ok(false);
    }
    let mut rows = Vec::<(String, Value)>::new();
    for (name, kind) in [
        ("manifest.json", "manifest"),
        ("knowledge_graph.json", "graph"),
        ("corpus_profile.json", "corpus_profile"),
        ("intent_taxonomy.json", "intent_taxonomy"),
        ("autorag_manifest.json", "autorag_manifest"),
    ] {
        let path = legacy.join(name);
        if path.is_file() {
            let raw = fs::read_to_string(&path).map_err(|source| io_error(&path, source))?;
            rows.push((
                kind.to_owned(),
                serde_json::from_str(&raw).map_err(|source| json_error(&path, source))?,
            ));
        }
    }
    for (name, kind) in [
        ("file_index.jsonl", "files"),
        ("folder_index.jsonl", "folders"),
        ("document_index.jsonl", "documents"),
        ("file_cards.jsonl", "cards"),
        ("chunk_map.jsonl", "chunks"),
        ("graph_routes.jsonl", "graph_routes"),
        ("folder_profile.jsonl", "folder_profiles"),
        ("duplicate_map.jsonl", "duplicates"),
        ("parse_errors.jsonl", "parse_errors"),
    ] {
        let path = legacy.join(name);
        if path.is_file() {
            let raw = fs::read_to_string(&path).map_err(|source| io_error(&path, source))?;
            for line in raw.lines().filter(|line| !line.trim().is_empty()) {
                rows.push((
                    kind.to_owned(),
                    serde_json::from_str(line).map_err(|source| json_error(&path, source))?,
                ));
            }
        }
    }
    if rows.is_empty() && !legacy.join("search_index.sqlite").is_file() {
        return Ok(false);
    }
    let refs = rows
        .iter()
        .map(|(kind, value)| (kind.as_str(), value.clone()))
        .collect::<Vec<_>>();
    replace_artifacts(root, &refs)?;
    Ok(true)
}

fn indexed_root(
    connection: &Connection,
    root_id: i64,
    canonical_root: String,
    updated_at: i64,
) -> Result<IndexedRoot> {
    let artifact_count = connection
        .query_row(
            "SELECT COUNT(*) FROM artifacts WHERE root_id=?1",
            [root_id],
            |row| row.get::<_, usize>(0),
        )
        .map_err(sqlite_error_path)?;
    let deep_index = connection
        .query_row(
            "SELECT row_json FROM artifacts WHERE root_id=?1 AND kind='deep_index_status' ORDER BY ordinal DESC LIMIT 1",
            [root_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(sqlite_error_path)?
        .map(|raw| {
            serde_json::from_str(&raw)
                .map_err(|source| json_error(database_path().unwrap_or_default(), source))
        })
        .transpose()?;
    Ok(IndexedRoot {
        root: PathBuf::from(canonical_root),
        updated_at,
        artifact_count,
        statistics: statistics_for_id(connection, root_id)?,
        deep_index,
    })
}

fn statistics_for_id(connection: &Connection, root_id: i64) -> Result<RootStatistics> {
    let mut statistics = empty_statistics();
    for kind in ["files", "folders", "documents", "chunks", "parse_errors"] {
        let values = load_artifacts_by_id(connection, root_id, kind)?;
        let count = values
            .iter()
            .filter(|value| value.get("status").and_then(Value::as_str) != Some("deleted"))
            .count();
        match kind {
            "files" => statistics.files = count,
            "folders" => statistics.folders = count,
            "documents" => statistics.documents = count,
            "chunks" => statistics.chunks = count,
            "parse_errors" => statistics.parse_errors = count,
            _ => unreachable!(),
        }
    }
    Ok(statistics)
}

fn load_artifacts_by_id(connection: &Connection, root_id: i64, kind: &str) -> Result<Vec<Value>> {
    let mut statement = connection
        .prepare("SELECT row_json FROM artifacts WHERE root_id=?1 AND kind=?2 ORDER BY ordinal")
        .map_err(sqlite_error_path)?;
    let rows = statement
        .query_map(params![root_id, kind], |row| row.get::<_, String>(0))
        .map_err(sqlite_error_path)?;
    rows.map(|row| {
        let raw = row.map_err(sqlite_error_path)?;
        serde_json::from_str(&raw)
            .map_err(|source| json_error(database_path().unwrap_or_default(), source))
    })
    .collect()
}

fn empty_statistics() -> RootStatistics {
    RootStatistics {
        files: 0,
        folders: 0,
        documents: 0,
        chunks: 0,
        parse_errors: 0,
    }
}

fn remove_root_cache(root_id: i64) -> Result<()> {
    let cache = data_dir()?.join("jikji/roots").join(root_id.to_string());
    let metadata = match fs::symlink_metadata(&cache) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error(&cache, source)),
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(&cache).map_err(|source| io_error(&cache, source))
    } else {
        fs::remove_dir_all(&cache).map_err(|source| io_error(&cache, source))
    }
}

fn initialize(connection: &Connection, path: &Path) -> Result<()> {
    connection
        .execute_batch(&format!(
            r#"
        CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS roots(
            id INTEGER PRIMARY KEY,
            canonical_root TEXT NOT NULL UNIQUE,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS artifacts(
            root_id INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
            ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL,
            row_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS artifacts_root_kind ON artifacts(root_id, kind, ordinal);
        CREATE INDEX IF NOT EXISTS artifacts_root_kind_path ON artifacts(root_id, kind, json_extract(row_json, '$.path'));
        INSERT INTO metadata(key,value) VALUES('schema_version','{DATABASE_SCHEMA_VERSION}')
             ON CONFLICT(key) DO UPDATE SET value=excluded.value;
    "#
        ))
        .map_err(|source| sqlite_error(path, source))
}

fn ensure_root_tx(tx: &Transaction<'_>, root: &Path) -> Result<i64> {
    if let Some(id) = lookup_root_id_by_keys(tx, &unresolved_root_keys(root))? {
        return Ok(id);
    }
    let canonical = registration_root_key(root)?;
    tx.execute("INSERT INTO roots(canonical_root, updated_at) VALUES(?1, unixepoch()) ON CONFLICT(canonical_root) DO UPDATE SET updated_at=unixepoch()", [&canonical]).map_err(sqlite_error_path)?;
    tx.query_row(
        "SELECT id FROM roots WHERE canonical_root=?1",
        [&canonical],
        |row| row.get(0),
    )
    .map_err(sqlite_error_path)
}

fn sqlite_error(path: &Path, source: rusqlite::Error) -> crate::JikjiError {
    io_error(path, Error::other(source))
}

fn sqlite_error_path(source: rusqlite::Error) -> crate::JikjiError {
    sqlite_error(
        &database_path().unwrap_or_else(|_| PathBuf::from("index.sqlite")),
        source,
    )
}

#[cfg(test)]
mod cloud_drive_tests {
    use super::{cloud_drive_path_blocked, path_looks_like_cloud_drive};
    use std::path::Path;

    #[test]
    fn path_looks_like_cloud_drive_accepts_rclone_and_google_names() {
        assert!(path_looks_like_cloud_drive(Path::new(
            "/home/cheol/GoogleDrive/마커"
        )));
        assert!(path_looks_like_cloud_drive(Path::new(
            "/home/cheol/Google Drive/마커"
        )));
        assert!(path_looks_like_cloud_drive(Path::new(
            "/home/cheol/google-drive/마커"
        )));
        assert!(path_looks_like_cloud_drive(Path::new("/mnt/gdrive/docs")));
        assert!(path_looks_like_cloud_drive(Path::new("/mnt/GDrive/docs")));
        assert!(!path_looks_like_cloud_drive(Path::new(
            "/home/cheol/Drive/마커"
        )));
        assert!(!path_looks_like_cloud_drive(Path::new(
            "/home/cheol/Documents/notes"
        )));
    }

    #[cfg(unix)]
    #[test]
    fn cloud_drive_path_blocked_follows_one_parent_symlink() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let tmp =
            std::env::temp_dir().join(format!("jikji-cloud-drive-{}-{nonce}", std::process::id()));
        let real = tmp.join("GoogleDrive");
        let link = tmp.join("Drive");
        std::fs::create_dir_all(real.join("마커")).expect("real drive");
        std::os::unix::fs::symlink(&real, &link).expect("parent symlink");
        assert!(cloud_drive_path_blocked(&link.join("마커")));
        assert!(!cloud_drive_path_blocked(&tmp.join("Documents/notes")));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
