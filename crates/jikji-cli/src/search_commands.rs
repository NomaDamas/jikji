use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::args::{BriefArgs, FindArgs, SearchArgs};
use crate::output::{print_json, print_json_compact};
use crate::search_prepare_options::{
    brief_prepare_options, find_prepare_options, search_prepare_options,
};
use crate::search_refresh::start_background_refresh;
use jikji_core::PrepareOptions;
use jikji_core::storage::{indexed_roots_within, resolve_root_scope};
use jikji_index::prepare;
use jikji_search::{
    BriefOptions, DiscoverOptions, IndexStatus, SearchOptions, TreeCheck, brief_payload,
    compact_brief_payload, count_root_docs, count_scope_docs, discover, search,
    search_index_status, search_index_status_with,
};

pub(crate) fn run_search(args: SearchArgs) -> jikji_core::Result<ExitCode> {
    let prepare_options = search_prepare_options(&args);
    let mut prepared = maybe_prepare_for_search(
        &args.root,
        args.fresh,
        args.auto_prepare && !args.no_auto_prepare,
        args.stale_after_seconds,
        &prepare_options,
        !args.no_background_refresh,
    )?;
    if prepared.status == IndexStatus::Missing {
        print_missing_index(&args.root);
        return Ok(ExitCode::from(1));
    }
    let mut candidates = search(&args.root, &args.query, SearchOptions { top_k: args.top_k })?;
    let empty_result_reindexed = if candidates.is_empty() && !prepared.foreground_prepared {
        prepare(&args.root, &prepare_options)?;
        candidates = search(&args.root, &args.query, SearchOptions { top_k: args.top_k })?;
        prepared.status = IndexStatus::Ready;
        prepared.foreground_prepared = true;
        true
    } else {
        false
    };
    start_deferred_background_refresh(&mut prepared, &args.root, &prepare_options);
    let payload = serde_json::json!({
        "root": args.root.display().to_string(),
        "query": args.query,
        "top_k": args.top_k,
        "index_status": prepared.status.as_str(),
        "foreground_prepared": prepared.foreground_prepared,
        "background_refresh_started": prepared.background_refresh_started,
        "empty_result_reindexed": empty_result_reindexed,
        "candidates": candidates,
    });
    if args.json {
        print_json(&payload)?;
    } else {
        print_search_candidates(&payload);
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn run_brief(args: BriefArgs) -> jikji_core::Result<ExitCode> {
    let prepare_options = brief_prepare_options(&args);
    let mut prepared = maybe_prepare_for_search(
        &args.root,
        args.fresh,
        args.auto_prepare && !args.no_auto_prepare,
        args.stale_after_seconds,
        &prepare_options,
        !args.no_background_refresh,
    )?;
    if prepared.status == IndexStatus::Missing {
        print_missing_index(&args.root);
        return Ok(ExitCode::from(1));
    }
    let mut candidates = search(&args.root, &args.query, SearchOptions { top_k: args.top_k })?;
    let empty_result_reindexed = if candidates.is_empty() && !prepared.foreground_prepared {
        prepare(&args.root, &prepare_options)?;
        candidates = search(&args.root, &args.query, SearchOptions { top_k: args.top_k })?;
        prepared.status = IndexStatus::Ready;
        prepared.foreground_prepared = true;
        true
    } else {
        false
    };
    start_deferred_background_refresh(&mut prepared, &args.root, &prepare_options);
    let options = BriefOptions {
        top_k: args.top_k,
        foreground_prepared: prepared.foreground_prepared,
        background_refresh_started: prepared.background_refresh_started,
    };
    let payload = if args.compact {
        compact_brief_payload(
            &args.root,
            &args.query,
            prepared.status.as_str(),
            options,
            &candidates,
        )?
    } else {
        brief_payload(
            &args.root,
            &args.query,
            prepared.status.as_str(),
            options,
            &candidates,
        )
    };
    let mut payload = payload;
    payload["empty_result_reindexed"] = serde_json::json!(empty_result_reindexed);
    if args.json && args.compact {
        print_json_compact(&payload)?;
    } else if args.json {
        print_json(&payload)?;
    } else {
        println!("{}", payload);
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn run_find(args: FindArgs) -> jikji_core::Result<ExitCode> {
    run_find_like(args, true)
}

pub(crate) fn run_discover(args: FindArgs) -> jikji_core::Result<ExitCode> {
    run_find_like(args, false)
}

/// Where a `find` request is answered from when its folder is not itself an indexed root.
struct EnclosingIndex {
    /// Registered root whose index is searched.
    root: PathBuf,
    /// The requested folder relative to `root`.
    scope: String,
}

fn enclosing_index(requested: &Path) -> Option<EnclosingIndex> {
    match resolve_root_scope(requested) {
        Ok(Some(found)) if !found.scope.is_empty() => Some(EnclosingIndex {
            root: found.root,
            scope: found.scope,
        }),
        _ => None,
    }
}

fn note(code: &str, message: String) -> serde_json::Value {
    serde_json::json!({"code": code, "message": message})
}

fn run_find_like(mut args: FindArgs, find_mode: bool) -> jikji_core::Result<ExitCode> {
    args.query = jikji_core::nfc_text(&args.query);
    let prepare_options = find_prepare_options(&args);
    let enclosing = enclosing_index(&args.root);
    let mut notes = Vec::new();
    let mut prepared = if let Some(enclosing) = &enclosing {
        // A sub-folder of an indexed root is answered from that root's index. The enclosing
        // (possibly very large) root is never walked, prepared or refreshed on its behalf.
        notes.push(note(
            "searched_enclosing_index",
            format!(
                "{} is not an indexed root; searched the index of {} restricted to {}. Paths are relative to the requested folder.",
                args.root.display(),
                enclosing.root.display(),
                enclosing.scope
            ),
        ));
        if args.fresh {
            notes.push(note(
                "fresh_ignored",
                "--fresh was ignored: it would reindex the whole enclosing root. Run `jikji refresh` on that root if you want that.".to_owned(),
            ));
        }
        let status =
            search_index_status_with(&enclosing.root, args.stale_after_seconds, TreeCheck::Skip);
        PreparedSearchStatus {
            status: status.status,
            foreground_prepared: false,
            background_refresh_started: false,
            background_refresh_requested: false,
            freshness_checked: false,
        }
    } else {
        maybe_prepare_for_search(
            &args.root,
            args.fresh,
            args.auto_prepare && !args.no_auto_prepare,
            args.stale_after_seconds,
            &prepare_options,
            !args.no_background_refresh,
        )?
    };
    if prepared.status == IndexStatus::Missing {
        return emit_find_recovery(&args, "missing", None);
    }
    let discover_payload = |args: &FindArgs| {
        discover_payload(
            args,
            enclosing.as_ref().map(|found| found.root.clone()),
            enclosing.as_ref().map(|found| found.scope.clone()),
        )
    };
    let mut payload = match discover_payload(&args) {
        Ok(payload) => payload,
        Err(error) => return emit_find_recovery(&args, "failure", Some(error.to_string())),
    };
    // An empty result is retried after one reindex only when the folder was compared with the index
    // and found changed. An unchanged folder has nothing new to find, and a folder too large (or
    // on a mount) to compare must not block the search for minutes: both get an explanation instead.
    let reindex_allowed = enclosing.is_none()
        && !args.query.trim().is_empty()
        && !prepared.foreground_prepared
        && prepared.freshness_checked
        && prepared.status == IndexStatus::ChangedUsingPreviousIndex;
    let empty_result_reindexed = if reindex_allowed && payload_paths_empty(&payload) {
        prepare(&args.root, &prepare_options)?;
        payload = discover_payload(&args)?;
        prepared.status = IndexStatus::Ready;
        prepared.foreground_prepared = true;
        prepared.freshness_checked = true;
        true
    } else {
        false
    };
    start_deferred_background_refresh(&mut prepared, &args.root, &prepare_options);
    // What kind of file each result is and whether its text was searched: read from the index,
    // while the paths are still relative to the indexed root.
    let index_root = enclosing.as_ref().map_or(&args.root, |found| &found.root);
    let mut profiles =
        jikji_search::profile_results(index_root, &payload, &args.query).unwrap_or_default();
    let coverage = if payload_paths_empty(&payload) && !args.query.trim().is_empty() {
        let scope = enclosing.as_ref().map(|found| found.scope.as_str());
        jikji_search::coverage(index_root, scope).ok()
    } else {
        None
    };
    if let Some(enclosing) = &enclosing {
        jikji_search::rebase_payload_paths(&mut payload, &enclosing.scope);
        payload["index_root"] = serde_json::json!(enclosing.root.display().to_string());
        payload["scope"] = serde_json::json!(enclosing.scope);
    }
    if let Some(paths) = payload["paths"].as_array() {
        for (profile, path) in profiles.iter_mut().zip(paths) {
            profile["path"] = path.clone();
        }
    }
    add_search_guidance(
        &mut payload,
        &args,
        enclosing.as_ref(),
        &prepared,
        &SearchEvidence {
            profiles: &profiles,
            coverage: coverage.as_ref(),
        },
        &mut notes,
    );
    if find_mode {
        payload["mode"] = serde_json::json!("find");
        payload["command"] = serde_json::json!("jikji find");
    }
    payload["jikji_version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
    payload["index_status"] = serde_json::json!(prepared.status.as_str());
    payload["foreground_prepared"] = serde_json::json!(prepared.foreground_prepared);
    payload["background_refresh_started"] = serde_json::json!(prepared.background_refresh_started);
    payload["empty_result_reindexed"] = serde_json::json!(empty_result_reindexed);
    if find_mode && args.first {
        for key in [
            "answer_paths",
            "paths",
            "candidates",
            "evidence_pack",
            "results",
        ] {
            truncate_array_field(&mut payload, key, 1);
        }
    }
    if args.json {
        print_json_compact(&payload)?;
    } else if !find_mode {
        println!("{}", payload);
    } else if let Some(paths) = payload["paths"].as_array() {
        for path in paths {
            println!("{}", path.as_str().unwrap_or(""));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Stored facts about the returned files, gathered from the index for the plan.
struct SearchEvidence<'a> {
    profiles: &'a [serde_json::Value],
    coverage: Option<&'a serde_json::Value>,
}

/// Adds the explanations an agent needs to act on a result: what was searched, how fresh the
/// index is, why nothing came back, and what the search cannot do.
fn add_search_guidance(
    payload: &mut serde_json::Value,
    args: &FindArgs,
    enclosing: Option<&EnclosingIndex>,
    prepared: &PreparedSearchStatus,
    evidence: &SearchEvidence<'_>,
    notes: &mut Vec<serde_json::Value>,
) {
    if !prepared.freshness_checked && !prepared.foreground_prepared {
        notes.push(note(
            "freshness_not_checked",
            "Files changed since the index was built may be missing from these results (the folder tree was not compared with the index: sub-folder search, cloud mount, or a tree too large to walk in time). Use a normal file read for files you just edited.".to_owned(),
        ));
    }
    let plan = crate::request_plan::plan(
        &args.query,
        &args.root,
        evidence.profiles,
        evidence.coverage,
        &jikji_search::query_words(&args.query),
    );
    notes.extend(
        plan.notes
            .iter()
            .map(|planned| note(planned.code, planned.message.clone())),
    );
    payload["request_plan"] = plan.json;
    payload["results"] = serde_json::Value::Array(evidence.profiles.to_vec());
    if let Some(coverage) = evidence.coverage {
        payload["coverage"] = coverage.clone();
    }
    if args.query.trim().is_empty() {
        payload["empty_reason"] = serde_json::json!("empty_query");
        notes.push(note(
            "empty_query",
            "The query is empty. Give at least one word from a file name, folder name or the text inside the file.".to_owned(),
        ));
    } else if payload_paths_empty(payload) {
        let (searched, indexed) = match enclosing {
            Some(found) => (
                format!("{} (under {})", found.scope, found.root.display()),
                count_scope_docs(&found.root, &found.scope).unwrap_or(0),
            ),
            None => (
                args.root.display().to_string(),
                count_root_docs(&args.root).unwrap_or(0),
            ),
        };
        let (code, message) = if !args.root.exists() {
            (
                "path_not_found",
                format!(
                    "{} does not exist; check the folder path.",
                    args.root.display()
                ),
            )
        } else if indexed == 0 {
            let next = match enclosing {
                Some(found) => format!(
                    " The enclosing index of {} holds nothing from this folder (it was built before the folder existed, or skips it). Search it with grep or direct reads for now; `jikji refresh {}` would add it but reprocesses that whole root, so ask the user first.",
                    found.root.display(),
                    found.root.display()
                ),
                None => String::new(),
            };
            (
                "scope_has_no_indexed_files",
                format!(
                    "{searched} has no indexed files: it may be empty, hold only ignored/hidden/sensitive files, or be newer than the index.{next}"
                ),
            )
        } else {
            (
                "no_match_in_index",
                format!(
                    "Searched {indexed} indexed files in {searched}; none matched these words. Try fewer or different words (names and document text are matched by word, not by phrase)."
                ),
            )
        };
        payload["empty_reason"] = serde_json::json!(code);
        payload["indexed_files_searched"] = serde_json::json!(indexed);
        notes.push(note(code, message));
    }
    payload["notes"] = serde_json::Value::Array(std::mem::take(notes));
}

fn payload_paths_empty(payload: &serde_json::Value) -> bool {
    payload
        .get("paths")
        .and_then(serde_json::Value::as_array)
        .is_none_or(Vec::is_empty)
}

fn discover_payload(
    args: &FindArgs,
    index_root: Option<PathBuf>,
    scope: Option<String>,
) -> jikji_core::Result<serde_json::Value> {
    discover(
        &args.root,
        &args.query,
        DiscoverOptions {
            top_k: args.top_k,
            retry_exhausted: args.after_jikji_retry,
            retry_proof: args.retry_proof.clone(),
            index_root,
            scope,
            ..Default::default()
        },
    )
}

fn emit_find_recovery(
    args: &FindArgs,
    index_status: &str,
    error: Option<String>,
) -> jikji_core::Result<ExitCode> {
    let proof = recovery_proof(args, index_status);
    let retry_verified = args.after_jikji_retry && args.retry_proof == proof;
    let (action, answerability, raw_allowed, retries, raw_commands) = if retry_verified {
        (
            "raw_fallback_after_retry",
            "needs_raw_fallback_after_retry",
            true,
            0,
            2,
        )
    } else {
        ("jikji_retry", "needs_one_jikji_retry", false, 1, 0)
    };
    let retry_command = format!(
        "jikji find {} {:?} --json --after-jikji-retry --retry-proof {}",
        args.root.display(),
        args.query,
        proof
    );
    let mut payload = serde_json::json!({
        "mode": "find",
        "command": "jikji find",
        "root": args.root.display().to_string(),
        "query": args.query,
        "index_status": index_status,
        "error": error,
        "paths": [],
        "answer_paths": [],
        "candidates": [],
        "handoff_action": action,
        "answerability": answerability,
        "retry_proof": if retry_verified { "" } else { proof.as_str() },
        "next_commands": if retry_verified { Vec::<String>::new() } else { vec![retry_command] },
        "max_jikji_retries": retries,
        "max_raw_fallback_commands": raw_commands,
        "raw_fallback_allowed": raw_allowed,
        "tool_call_policy": {
            "stop_after_find": false,
            "allowed_followups": if retry_verified { vec!["bounded_raw_fallback"] } else { vec!["exactly_one_jikji_retry"] },
            "forbidden_tools": if retry_verified { Vec::<&str>::new() } else { vec!["grep", "rg", "glob", "find", "fd", "ls", "tree", "cat"] },
            "reason": if retry_verified { "exactly_one_jikji_retry_failed" } else { "jikji_retry_required_before_raw_fallback" }
        }
    });
    let (reason, message) = recovery_reason(&args.root, index_status, error.as_deref());
    payload["reason"] = serde_json::json!(reason);
    payload["message"] = serde_json::json!(message);
    payload["jikji_version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
    let mut next_steps = Vec::new();
    if reason == "not_indexed" {
        let below = indexed_roots_within(&args.root, 8).unwrap_or_default();
        if !below.is_empty() {
            let listed = below
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            payload["indexed_roots_inside"] = serde_json::json!(listed);
            next_steps.push(format!(
                "Search one of the indexed folders inside it instead: {}",
                listed.join(", ")
            ));
        }
        next_steps.push(format!(
            "If this folder should be searchable, ask the user before indexing it: jikji prepare {} --json (find never indexes by itself).",
            args.root.display()
        ));
    } else if reason == "path_not_found" {
        next_steps.push("Check the folder path (it must exist; use an absolute path or a path relative to the current directory).".to_owned());
    }
    payload["next_steps"] = serde_json::json!(next_steps);
    if args.json {
        print_json_compact(&payload)?;
        Ok(ExitCode::SUCCESS)
    } else {
        print_missing_index(&args.root);
        Ok(ExitCode::from(1))
    }
}

fn recovery_proof(args: &FindArgs, index_status: &str) -> String {
    format!(
        "jikji-retry:{}:{}:{}:{}",
        index_status,
        args.root.display(),
        args.top_k,
        args.query
    )
}

pub(crate) struct PreparedSearchStatus {
    pub(crate) status: IndexStatus,
    pub(crate) foreground_prepared: bool,
    pub(crate) background_refresh_started: bool,
    pub(crate) background_refresh_requested: bool,
    /// Whether the folder tree was compared with the index (see `SearchIndexStatus`).
    pub(crate) freshness_checked: bool,
}

pub(crate) fn maybe_prepare_for_search(
    root: &Path,
    fresh: bool,
    auto_prepare: bool,
    stale_after_seconds: i64,
    options: &PrepareOptions,
    background_refresh: bool,
) -> jikji_core::Result<PreparedSearchStatus> {
    let status = search_index_status(root, stale_after_seconds);
    if fresh || (status.should_prepare && auto_prepare) {
        prepare(root, options)?;
        let next = search_index_status(root, stale_after_seconds);
        return Ok(PreparedSearchStatus {
            status: if status.should_prepare {
                IndexStatus::Ready
            } else {
                next.status
            },
            foreground_prepared: true,
            background_refresh_started: false,
            background_refresh_requested: false,
            freshness_checked: true,
        });
    }
    let background_refresh_requested = matches!(
        status.status,
        IndexStatus::ChangedUsingPreviousIndex | IndexStatus::StaleUsingPreviousIndex
    ) && !fresh
        && background_refresh;
    Ok(PreparedSearchStatus {
        status: status.status,
        foreground_prepared: false,
        background_refresh_started: false,
        background_refresh_requested,
        freshness_checked: status.freshness_checked,
    })
}

fn recovery_reason(root: &Path, index_status: &str, error: Option<&str>) -> (&'static str, String) {
    if index_status == "failure" {
        return (
            "search_failed",
            format!(
                "The search could not run: {}",
                error.unwrap_or("unknown error")
            ),
        );
    }
    if !root.exists() {
        return (
            "path_not_found",
            format!("{} does not exist.", root.display()),
        );
    }
    (
        "not_indexed",
        format!(
            "No Jikji index covers {} (neither the folder nor any folder above it is indexed).",
            root.display()
        ),
    )
}

pub(crate) fn start_deferred_background_refresh(
    prepared: &mut PreparedSearchStatus,
    root: &Path,
    options: &PrepareOptions,
) {
    if prepared.background_refresh_requested {
        prepared.background_refresh_started = start_background_refresh(root, options);
    }
}

fn print_missing_index(root: &Path) {
    eprintln!(
        "No Jikji search index found under {}. Run: jikji prepare {}",
        root.display(),
        root.display()
    );
}

fn print_search_candidates(payload: &serde_json::Value) {
    for (idx, item) in payload["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        println!(
            "{:02} {:>8} {}",
            idx + 1,
            item["score"],
            item["path"].as_str().unwrap_or("")
        );
    }
}

fn truncate_array_field(payload: &mut serde_json::Value, key: &str, limit: usize) {
    if let Some(array) = payload
        .get_mut(key)
        .and_then(serde_json::Value::as_array_mut)
    {
        array.truncate(limit);
    }
}
