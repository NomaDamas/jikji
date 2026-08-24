use std::path::Path;
use std::process::ExitCode;

use jikji_core::PrepareOptions;
use jikji_index::prepare;
use jikji_search::{
    BriefOptions, DiscoverOptions, IndexStatus, SearchOptions, brief_payload,
    compact_brief_payload, discover, search, search_index_status,
};

use crate::args::{BriefArgs, FindArgs, SearchArgs};
use crate::output::{print_json, print_json_compact};
use crate::search_prepare_options::{
    brief_prepare_options, find_prepare_options, search_prepare_options,
};
use crate::search_refresh::start_background_refresh;

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
    let prepare_options = find_prepare_options(&args);
    let mut prepared = maybe_prepare_for_search(
        &args.root,
        args.fresh,
        args.auto_prepare && !args.no_auto_prepare,
        args.stale_after_seconds,
        &prepare_options,
        !args.no_background_refresh,
    )?;
    if prepared.status == IndexStatus::Missing {
        return emit_find_recovery(&args, "missing", None);
    }
    let mut payload = match discover_payload(&args) {
        Ok(payload) => payload,
        Err(error) => return emit_find_recovery(&args, "failure", Some(error.to_string())),
    };
    let empty_result_reindexed = if payload_paths_empty(&payload) && !prepared.foreground_prepared {
        prepare(&args.root, &prepare_options)?;
        payload = discover_payload(&args)?;
        prepared.status = IndexStatus::Ready;
        prepared.foreground_prepared = true;
        true
    } else {
        false
    };
    start_deferred_background_refresh(&mut prepared, &args.root, &prepare_options);
    payload["mode"] = serde_json::json!("find");
    payload["command"] = serde_json::json!("jikji find");
    payload["index_status"] = serde_json::json!(prepared.status.as_str());
    payload["foreground_prepared"] = serde_json::json!(prepared.foreground_prepared);
    payload["background_refresh_started"] = serde_json::json!(prepared.background_refresh_started);
    payload["empty_result_reindexed"] = serde_json::json!(empty_result_reindexed);
    if args.first {
        for key in ["answer_paths", "paths", "candidates", "evidence_pack"] {
            truncate_array_field(&mut payload, key, 1);
        }
    }
    if args.json {
        print_json_compact(&payload)?;
    } else if let Some(paths) = payload["paths"].as_array() {
        for path in paths {
            println!("{}", path.as_str().unwrap_or(""));
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn run_discover(args: FindArgs) -> jikji_core::Result<ExitCode> {
    let prepare_options = find_prepare_options(&args);
    let mut prepared = maybe_prepare_for_search(
        &args.root,
        args.fresh,
        args.auto_prepare && !args.no_auto_prepare,
        args.stale_after_seconds,
        &prepare_options,
        !args.no_background_refresh,
    )?;
    if prepared.status == IndexStatus::Missing {
        return emit_find_recovery(&args, "missing", None);
    }
    let mut payload = match discover_payload(&args) {
        Ok(payload) => payload,
        Err(error) => return emit_find_recovery(&args, "failure", Some(error.to_string())),
    };
    let empty_result_reindexed = if payload_paths_empty(&payload) && !prepared.foreground_prepared {
        prepare(&args.root, &prepare_options)?;
        payload = discover_payload(&args)?;
        prepared.status = IndexStatus::Ready;
        prepared.foreground_prepared = true;
        true
    } else {
        false
    };
    start_deferred_background_refresh(&mut prepared, &args.root, &prepare_options);
    payload["index_status"] = serde_json::json!(prepared.status.as_str());
    payload["foreground_prepared"] = serde_json::json!(prepared.foreground_prepared);
    payload["background_refresh_started"] = serde_json::json!(prepared.background_refresh_started);
    payload["empty_result_reindexed"] = serde_json::json!(empty_result_reindexed);
    if args.json {
        print_json_compact(&payload)?;
    } else {
        println!("{}", payload);
    }
    Ok(ExitCode::SUCCESS)
}

fn payload_paths_empty(payload: &serde_json::Value) -> bool {
    payload
        .get("paths")
        .and_then(serde_json::Value::as_array)
        .is_none_or(Vec::is_empty)
}

fn discover_payload(args: &FindArgs) -> jikji_core::Result<serde_json::Value> {
    discover(
        &args.root,
        &args.query,
        DiscoverOptions {
            top_k: args.top_k,
            retry_exhausted: args.after_jikji_retry,
            retry_proof: args.retry_proof.clone(),
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
    let payload = serde_json::json!({
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

struct PreparedSearchStatus {
    status: IndexStatus,
    foreground_prepared: bool,
    background_refresh_started: bool,
    background_refresh_requested: bool,
}

fn maybe_prepare_for_search(
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
    })
}

fn start_deferred_background_refresh(
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
