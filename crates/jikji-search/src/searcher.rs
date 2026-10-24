use std::path::Path;

use jikji_core::Result;
use jikji_core::storage::{database_path, migrate_legacy, open_database, root_id};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::SEARCH_INDEX_SCHEMA_VERSION;
use crate::io::sqlite_error;
use crate::map_rescore;
use crate::scoring::{ScoreMap, TermMap, score_field_hits, score_filename_hits};
use crate::tokenizer::{query_terms, tokens};

fn restore_body(row_json: &mut Value, blob: Option<Vec<u8>>, size: Option<i64>) -> Result<()> {
    if row_json.get("body_text").is_some() {
        return Ok(());
    }
    let Some(blob) = blob else {
        return Ok(());
    };
    let size = size.ok_or_else(|| {
        jikji_core::io_error(
            "search_docs",
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "compressed body size missing",
            ),
        )
    })?;
    let size = usize::try_from(size).map_err(|_| {
        jikji_core::io_error(
            "search_docs",
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "compressed body size invalid",
            ),
        )
    })?;
    let body = jikji_core::compression::decompress_body_text(&blob, size)?;
    if let Value::Object(object) = row_json {
        object.insert("body_text".to_owned(), Value::String(body));
    }
    Ok(())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    pub top_k: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self { top_k: 10 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchCandidate {
    pub path: String,
    pub name: String,
    pub score: f64,
    pub reasons: Vec<String>,
    pub matched_terms: Vec<String>,
    pub matched_intents: Vec<String>,
    pub duplicate_group_id: String,
    pub evidence: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discover_score: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub strategies: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub queries: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_query_rank: Option<usize>,
}

pub fn search(root: &Path, query: &str, options: SearchOptions) -> Result<Vec<SearchCandidate>> {
    migrate_legacy(root)?;
    let index_path = database_path()?;
    let con = open_database()?;
    crate::sqlite_index::initialize(&con, &index_path)?;
    let Some(root_id) = root_id(&con, root)? else {
        return Ok(Vec::new());
    };
    install_root_views(&con, root_id, &index_path)?;
    if !schema_matches(&con)? {
        return Ok(Vec::new());
    }
    let query_terms = query_terms(query);
    if query_terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut scores = ScoreMap::new();
    let mut matched = TermMap::new();
    let mut reasons = TermMap::new();
    score_filename_hits(&con, &query_terms, &mut scores, &mut matched, &mut reasons)?;
    score_field_hits(&con, &query_terms, &mut scores, &mut matched, &mut reasons)?;
    let mut candidates = if scores.is_empty() {
        fallback_scan_docs(&con, query)?
    } else {
        candidates_from_scores(&con, query, scores, matched, reasons, options.top_k)?
    };
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
    });
    candidates.truncate(options.top_k);
    Ok(candidates)
}

fn install_root_views(con: &Connection, root_id: i64, path: &Path) -> Result<()> {
    con.execute_batch(&format!(
        "DROP VIEW IF EXISTS meta;
         DROP VIEW IF EXISTS docs;
         DROP VIEW IF EXISTS terms;
         DROP VIEW IF EXISTS filename_keys;
         DROP VIEW IF EXISTS idf;
         DROP VIEW IF EXISTS field_terms;
         DROP VIEW IF EXISTS field_lengths;
         DROP VIEW IF EXISTS field_idf;
         DROP VIEW IF EXISTS field_avg;
         CREATE TEMP VIEW meta AS SELECT key,value FROM search_meta WHERE root_id={root_id};
         CREATE TEMP VIEW docs AS SELECT id,path,name,ext,duplicate_group_id,row_json,body_blob,body_size FROM search_docs WHERE root_id={root_id};
         CREATE TEMP VIEW terms AS SELECT term,doc_id FROM search_terms WHERE root_id={root_id};
         CREATE TEMP VIEW filename_keys AS SELECT key,doc_id FROM search_filename_keys WHERE root_id={root_id};
         CREATE TEMP VIEW idf AS SELECT term,value FROM search_idf WHERE root_id={root_id};
         CREATE TEMP VIEW field_terms AS SELECT term,field,doc_id,tf FROM search_field_terms WHERE root_id={root_id};
         CREATE TEMP VIEW field_lengths AS SELECT doc_id,field,length FROM search_field_lengths WHERE root_id={root_id};
         CREATE TEMP VIEW field_idf AS SELECT term,value FROM search_field_idf WHERE root_id={root_id};
         CREATE TEMP VIEW field_avg AS SELECT field,value FROM search_field_avg WHERE root_id={root_id};"
    )).map_err(|source| sqlite_error(path, source))
}

fn schema_matches(con: &Connection) -> Result<bool> {
    match con.query_row(
        "SELECT value FROM meta WHERE key='schema_version'",
        [],
        |row| row.get::<_, String>(0),
    ) {
        Ok(schema) => Ok(schema == SEARCH_INDEX_SCHEMA_VERSION.to_string()),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(source) => Err(sqlite_error(Path::new("search_index.sqlite"), source)),
    }
}

struct DocRecord {
    path: String,
    name: String,
    duplicate_group_id: String,
    evidence: Vec<String>,
    row_json: Value,
}

fn candidates_from_scores(
    con: &Connection,
    query: &str,
    scores: ScoreMap,
    mut matched: TermMap,
    mut reasons: TermMap,
    top_k: usize,
) -> Result<Vec<SearchCandidate>> {
    let mut ranked: Vec<(i64, f64)> = scores
        .into_iter()
        .filter(|(_, score)| *score > 0.0)
        .collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    let load_limit = top_k.saturating_mul(2).clamp(top_k.max(1), 200);
    ranked.truncate(load_limit);
    let mut out = Vec::with_capacity(ranked.len());
    for (doc_id, base_score) in ranked {
        let doc = load_doc(con, doc_id)?;
        let (map_score, map_reasons, map_terms) =
            map_rescore::rescore(query, &doc.row_json, base_score);
        let score = base_score + map_score;
        let mut doc_reasons = reasons.remove(&doc_id).unwrap_or_default();
        doc_reasons.extend(map_reasons);
        let mut doc_matched = matched.remove(&doc_id).unwrap_or_default();
        doc_matched.extend(map_terms);
        out.push(SearchCandidate {
            path: doc.path,
            name: doc.name,
            score: round3(score),
            reasons: doc_reasons.into_iter().collect(),
            matched_terms: doc_matched.into_iter().take(16).collect(),
            matched_intents: Vec::new(),
            duplicate_group_id: doc.duplicate_group_id,
            strategies: Vec::new(),
            evidence: doc.evidence,
            discover_score: None,
            queries: Vec::new(),
            best_query_rank: None,
        });
    }
    Ok(out)
}
fn load_doc(con: &Connection, doc_id: i64) -> Result<DocRecord> {
    con.query_row(
        "SELECT path,name,duplicate_group_id,row_json,body_blob,body_size FROM docs WHERE id=?",
        params![doc_id],
        |row| {
            let raw: String = row.get(3)?;
            let mut value = serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null);
            restore_body(&mut value, row.get(4)?, row.get(5)?)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            let evidence = value
                .get("evidence")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            Ok(DocRecord {
                path: row.get(0)?,
                name: row.get(1)?,
                duplicate_group_id: row.get(2)?,
                evidence,
                row_json: value,
            })
        },
    )
    .map_err(|source| sqlite_error(Path::new("search_index.sqlite"), source))
}

fn fallback_scan_docs(con: &Connection, query: &str) -> Result<Vec<SearchCandidate>> {
    let query_tokens = tokens(query, 32);
    let mut stmt = con
        .prepare("SELECT path,name,duplicate_group_id,row_json,body_blob,body_size FROM docs")
        .map_err(|source| sqlite_error(Path::new("search_index.sqlite"), source))?;
    let rows = stmt
        .query_map([], |row| {
            let mut value =
                serde_json::from_str::<Value>(&row.get::<_, String>(3)?).unwrap_or(Value::Null);
            restore_body(&mut value, row.get(4)?, row.get(5)?)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                value,
            ))
        })
        .map_err(|source| sqlite_error(Path::new("search_index.sqlite"), source))?;
    let mut out = Vec::new();
    for row in rows {
        let (path, name, duplicate_group_id, value) =
            row.map_err(|source| sqlite_error(Path::new("search_index.sqlite"), source))?;
        let haystack = serde_json::to_string(&value)
            .unwrap_or_default()
            .to_lowercase();
        let hits = query_tokens
            .iter()
            .filter(|term| haystack.contains(term.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if hits.is_empty() {
            continue;
        }
        out.push(SearchCandidate {
            path,
            name,
            score: hits.len() as f64 * 10.0,
            reasons: vec!["body-coverage".to_owned()],
            matched_terms: hits,
            matched_intents: Vec::new(),
            duplicate_group_id,
            evidence: Vec::new(),
            discover_score: None,
            strategies: Vec::new(),
            queries: Vec::new(),
            best_query_rank: None,
        });
    }
    Ok(out)
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}
