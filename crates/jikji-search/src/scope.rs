//! Restricting a search to one folder of an indexed root.
//!
//! An agent often names a sub-folder of a folder that was indexed as a whole. The index of the
//! enclosing root answers it; these helpers keep results to that sub-folder.

use std::path::Path;

use jikji_core::Result;
use jikji_core::storage::{database_path, open_database, root_id};
use rusqlite::params;
use serde_json::Value;

use crate::io::sqlite_error;

/// Rewrites every result path of a `find`/`discover` payload from "relative to the indexed root"
/// to "relative to the requested folder `scope`", so the caller can join it with the folder it
/// asked about. Paths outside `scope` cannot occur (results are scope-filtered) and are left as is.
pub fn rebase_payload_paths(payload: &mut Value, scope: &str) {
    let scope = scope.trim_matches('/');
    if scope.is_empty() {
        return;
    }
    fn rebase(text: &str, scope: &str) -> Option<String> {
        text.strip_prefix(scope)
            .and_then(|rest| rest.strip_prefix('/'))
            .map(str::to_owned)
    }
    fn walk(value: &mut Value, scope: &str) {
        match value {
            Value::Object(map) => {
                for (key, child) in map.iter_mut() {
                    match (key.as_str(), &mut *child) {
                        ("p" | "path", Value::String(text)) => {
                            if let Some(new) = rebase(text, scope) {
                                *text = new;
                            }
                        }
                        ("paths" | "answer_paths" | "supporting_paths", Value::Array(items)) => {
                            for item in items {
                                if let Value::String(text) = item {
                                    if let Some(new) = rebase(text, scope) {
                                        *text = new;
                                    }
                                }
                            }
                        }
                        _ => walk(child, scope),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| walk(item, scope)),
            _ => {}
        }
    }
    walk(payload, scope);
}
/// SQL `LIKE ... ESCAPE '\'` pattern matching every indexed path below `scope`.
pub(crate) fn like_pattern(scope: &str) -> String {
    let mut pattern = String::with_capacity(scope.len() + 2);
    for ch in scope.trim_matches('/').chars() {
        if matches!(ch, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push_str("/%");
    pattern
}

/// Indexed files below `scope` (a folder relative to `root`); 0 when the root is not indexed.
pub fn count_scope_docs(root: &Path, scope: &str) -> Result<usize> {
    let index_path = database_path()?;
    let con = open_database()?;
    crate::sqlite_index::initialize(&con, &index_path)?;
    let Some(id) = root_id(&con, root)? else {
        return Ok(0);
    };
    let count: i64 = con
        .query_row(
            "SELECT COUNT(*) FROM search_docs WHERE root_id=?1 AND path LIKE ?2 ESCAPE '\\'",
            params![id, like_pattern(scope)],
            |row| row.get(0),
        )
        .map_err(|source| sqlite_error(&index_path, source))?;
    Ok(usize::try_from(count).unwrap_or(0))
}

/// Total indexed files of `root`; 0 when the root is not indexed.
pub fn count_root_docs(root: &Path) -> Result<usize> {
    let index_path = database_path()?;
    let con = open_database()?;
    crate::sqlite_index::initialize(&con, &index_path)?;
    let Some(id) = root_id(&con, root)? else {
        return Ok(0);
    };
    let count: i64 = con
        .query_row(
            "SELECT COUNT(*) FROM search_docs WHERE root_id=?1",
            params![id],
            |row| row.get(0),
        )
        .map_err(|source| sqlite_error(&index_path, source))?;
    Ok(usize::try_from(count).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::like_pattern;

    #[test]
    fn like_pattern_escapes_wildcards_and_trims_separators() {
        assert_eq!(like_pattern("a/b"), "a/b/%");
        assert_eq!(like_pattern("/a_b/%c/"), "a\\_b/\\%c/%");
    }
}
