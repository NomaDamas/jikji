use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use jikji_core::Result;
use jikji_core::storage::{database_path, ensure_root, open_database};
use rusqlite::{Transaction, params};
use serde_json::Value;

use crate::SEARCH_INDEX_SCHEMA_VERSION;
use crate::index_rows::{IndexRow, fielded_terms, row_terms};
use crate::io::sqlite_error;

pub(crate) fn write_sqlite(root: &Path, rows: &[IndexRow]) -> Result<()> {
    let path = database_path()?;
    let mut connection = open_database()?;
    initialize(&connection, &path)?;
    let root_id = ensure_root(&connection, root)?;
    let tx = connection
        .transaction()
        .map_err(|source| sqlite_error(&path, source))?;
    clear_root(&tx, root_id, &path)?;
    let mut stats = SqliteStats::default();
    for (index, row) in rows.iter().enumerate() {
        let doc_id = i64::try_from(index + 1).unwrap_or(i64::MAX);
        insert_doc(&tx, root_id, doc_id, row, &path)?;
        insert_terms(&tx, root_id, doc_id, row, &mut stats, &path)?;
        insert_filename_keys(&tx, root_id, doc_id, row, &path)?;
        insert_field_terms(&tx, root_id, doc_id, row, &mut stats, &path)?;
    }
    insert_stats(&tx, root_id, rows, stats, &path)?;
    tx.commit().map_err(|source| sqlite_error(&path, source))
}
pub(crate) fn initialize(connection: &rusqlite::Connection, path: &Path) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS search_meta(root_id INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,key TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(root_id,key));
         CREATE TABLE IF NOT EXISTS search_docs(root_id INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,id INTEGER NOT NULL,path TEXT NOT NULL,name TEXT NOT NULL,ext TEXT NOT NULL,duplicate_group_id TEXT NOT NULL,row_json TEXT NOT NULL,body_blob BLOB,body_size INTEGER,PRIMARY KEY(root_id,id));
         CREATE TABLE IF NOT EXISTS search_terms(root_id INTEGER NOT NULL,term TEXT NOT NULL,doc_id INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS search_filename_keys(root_id INTEGER NOT NULL,key TEXT NOT NULL,doc_id INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS search_idf(root_id INTEGER NOT NULL,term TEXT NOT NULL,value REAL NOT NULL,PRIMARY KEY(root_id,term));
         CREATE TABLE IF NOT EXISTS search_field_terms(root_id INTEGER NOT NULL,term TEXT NOT NULL,field TEXT NOT NULL,doc_id INTEGER NOT NULL,tf INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS search_field_lengths(root_id INTEGER NOT NULL,doc_id INTEGER NOT NULL,field TEXT NOT NULL,length INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS search_field_idf(root_id INTEGER NOT NULL,term TEXT NOT NULL,value REAL NOT NULL,PRIMARY KEY(root_id,term));
         CREATE TABLE IF NOT EXISTS search_field_avg(root_id INTEGER NOT NULL,field TEXT NOT NULL,value REAL NOT NULL,PRIMARY KEY(root_id,field));
         CREATE INDEX IF NOT EXISTS search_terms_lookup ON search_terms(root_id,term);
         CREATE INDEX IF NOT EXISTS search_filename_lookup ON search_filename_keys(root_id,key);
         CREATE INDEX IF NOT EXISTS search_field_terms_lookup ON search_field_terms(root_id,term);
         CREATE INDEX IF NOT EXISTS search_field_docs_lookup ON search_field_terms(root_id,doc_id);
         CREATE INDEX IF NOT EXISTS search_field_lengths_lookup ON search_field_lengths(root_id, doc_id, field);"
    ).map_err(|source| sqlite_error(path, source))?;
    let mut columns = BTreeSet::new();
    let mut statement = connection
        .prepare("PRAGMA table_info(search_docs)")
        .map_err(|source| sqlite_error(path, source))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|source| sqlite_error(path, source))?;
    for row in rows {
        columns.insert(row.map_err(|source| sqlite_error(path, source))?);
    }
    if !columns.contains("body_blob") {
        connection
            .execute("ALTER TABLE search_docs ADD COLUMN body_blob BLOB", [])
            .map_err(|source| sqlite_error(path, source))?;
    }
    if !columns.contains("body_size") {
        connection
            .execute("ALTER TABLE search_docs ADD COLUMN body_size INTEGER", [])
            .map_err(|source| sqlite_error(path, source))?;
    }
    Ok(())
}

fn clear_root(tx: &Transaction<'_>, root_id: i64, path: &Path) -> Result<()> {
    for table in [
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
        tx.execute(&format!("DELETE FROM {table} WHERE root_id=?1"), [root_id])
            .map_err(|source| sqlite_error(path, source))?;
    }
    Ok(())
}

#[derive(Default)]
struct SqliteStats {
    df: BTreeMap<String, usize>,
    field_df: BTreeMap<String, usize>,
    field_len_totals: BTreeMap<String, usize>,
    term_rows: usize,
}

fn insert_doc(
    tx: &Transaction<'_>,
    root_id: i64,
    doc_id: i64,
    row: &IndexRow,
    path: &Path,
) -> Result<()> {
    let mut metadata = row.row_json.clone();
    if let Value::Object(object) = &mut metadata {
        object.remove("body_text");
    }
    let body_blob = jikji_core::compression::compress_body_text(&row.body)?;
    tx.execute(
        "INSERT INTO search_docs(root_id,id,path,name,ext,duplicate_group_id,row_json,body_blob,body_size) VALUES(?,?,?,?,?,?,?,?,?)",
        params![root_id, doc_id, row.path, row.name, row.ext, row.duplicate_group_id, serde_json::to_string(&metadata).unwrap_or_default(), body_blob, i64::try_from(row.body.len()).unwrap_or(i64::MAX)],
    ).map_err(|source| sqlite_error(path, source))?;
    Ok(())
}

fn insert_terms(
    tx: &Transaction<'_>,
    root_id: i64,
    doc_id: i64,
    row: &IndexRow,
    stats: &mut SqliteStats,
    path: &Path,
) -> Result<()> {
    for term in row_terms(row) {
        stats.term_rows += 1;
        *stats.df.entry(term.clone()).or_insert(0) += 1;
        tx.execute(
            "INSERT INTO search_terms(root_id,term,doc_id) VALUES(?,?,?)",
            params![root_id, term, doc_id],
        )
        .map_err(|source| sqlite_error(path, source))?;
    }
    Ok(())
}

fn insert_filename_keys(
    tx: &Transaction<'_>,
    root_id: i64,
    doc_id: i64,
    row: &IndexRow,
    path: &Path,
) -> Result<()> {
    for key in &row.filename_keys {
        tx.execute(
            "INSERT INTO search_filename_keys(root_id,key,doc_id) VALUES(?,?,?)",
            params![root_id, key, doc_id],
        )
        .map_err(|source| sqlite_error(path, source))?;
    }
    Ok(())
}

fn insert_field_terms(
    tx: &Transaction<'_>,
    root_id: i64,
    doc_id: i64,
    row: &IndexRow,
    stats: &mut SqliteStats,
    path: &Path,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    for (field, counts) in fielded_terms(row) {
        let length = counts.iter().map(|(_, count)| *count).sum::<usize>();
        *stats.field_len_totals.entry(field.to_owned()).or_insert(0) += length;
        tx.execute(
            "INSERT INTO search_field_lengths(root_id,doc_id,field,length) VALUES(?,?,?,?)",
            params![
                root_id,
                doc_id,
                field,
                i64::try_from(length).unwrap_or(i64::MAX)
            ],
        )
        .map_err(|source| sqlite_error(path, source))?;
        for (term, tf) in counts {
            seen.insert(term.clone());
            tx.execute(
                "INSERT INTO search_field_terms(root_id,term,field,doc_id,tf) VALUES(?,?,?,?,?)",
                params![
                    root_id,
                    term,
                    field,
                    doc_id,
                    i64::try_from(tf).unwrap_or(i64::MAX)
                ],
            )
            .map_err(|source| sqlite_error(path, source))?;
        }
    }
    for term in seen {
        *stats.field_df.entry(term).or_insert(0) += 1;
    }
    Ok(())
}

fn insert_stats(
    tx: &Transaction<'_>,
    root_id: i64,
    rows: &[IndexRow],
    stats: SqliteStats,
    path: &Path,
) -> Result<()> {
    let total = rows.len().max(1) as f64;
    for (term, freq) in stats.df {
        let value = ((1.0 + total) / (1.0 + freq as f64)).ln() + 1.0;
        tx.execute(
            "INSERT INTO search_idf(root_id,term,value) VALUES(?,?,?)",
            params![root_id, term, value],
        )
        .map_err(|source| sqlite_error(path, source))?;
    }
    for (term, freq) in stats.field_df {
        let value = ((total - freq as f64 + 0.5) / (freq as f64 + 0.5) + 1.0).ln();
        tx.execute(
            "INSERT INTO search_field_idf(root_id,term,value) VALUES(?,?,?)",
            params![root_id, term, value],
        )
        .map_err(|source| sqlite_error(path, source))?;
    }
    for (field, total_len) in stats.field_len_totals {
        tx.execute(
            "INSERT INTO search_field_avg(root_id,field,value) VALUES(?,?,?)",
            params![root_id, field, total_len as f64 / total],
        )
        .map_err(|source| sqlite_error(path, source))?;
    }
    for (key, value) in [
        ("schema_version", SEARCH_INDEX_SCHEMA_VERSION.to_string()),
        ("rows", rows.len().to_string()),
        ("terms", stats.term_rows.to_string()),
    ] {
        tx.execute(
            "INSERT INTO search_meta(root_id,key,value) VALUES(?,?,?)",
            params![root_id, key, value],
        )
        .map_err(|source| sqlite_error(path, source))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn initialize_creates_field_lengths_lookup_index() {
        let connection = Connection::open_in_memory().expect("memory sqlite");
        connection
            .execute_batch("CREATE TABLE roots(id INTEGER PRIMARY KEY);")
            .expect("roots table");
        initialize(&connection, Path::new("memory.sqlite")).expect("initialize");
        let names = connection
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='search_field_lengths'")
            .expect("prepare")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("index names");
        assert!(
            names
                .iter()
                .any(|name| name == "search_field_lengths_lookup"),
            "{names:?}"
        );
        let plan: String = connection
            .query_row(
                "EXPLAIN QUERY PLAN SELECT length FROM search_field_lengths WHERE root_id=?1 AND doc_id=?2 AND field=?3",
                params![1_i64, 1_i64, "body"],
                |row| row.get(3),
            )
            .expect("explain");
        assert!(plan.contains("search_field_lengths_lookup"), "{plan}");
    }
}
