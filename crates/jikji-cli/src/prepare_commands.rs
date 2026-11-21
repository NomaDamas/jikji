use std::process::ExitCode;

use jikji_core::PrepareOptions;
use jikji_core::cloud_content::{
    CloudContentPolicy, approval_key, approve_cloud_content, revoke_cloud_content,
};
use jikji_core::storage::cloud_drive_path_blocked;
use jikji_index::{CleanOptions, clean, prepare, reindex_search};
use serde_json::json;

use crate::args::{CleanArgs, PrepareArgs};
use crate::output::print_json;

pub(crate) fn run_prepare(args: PrepareArgs) -> jikji_core::Result<ExitCode> {
    if args.search_only {
        return run_search_only(args);
    }
    let options = prepare_options_from_args(&args);
    apply_cloud_decision(&args)?;
    let result = prepare(&args.root, &options)?;
    let cloud = cloud_content_status(&result.root, &args.root);
    if options.clean_reindex {
        jikji_core::storage::vacuum_database()?;
    }
    if !args.no_agent_rules {
        jikji_agent::write_routing_blocks(&args.root)?;
    }
    if args.json {
        let mut value = serde_json::to_value(&result)
            .map_err(|source| jikji_core::json_error("prepare result", source))?;
        value["cloud_content"] = cloud;
        print_json(&value)?;
    } else {
        println!("Jikji prepared: {}", result.root.display());
        println!(
            "- files={} folders={} deleted={}",
            result.files, result.folders, result.deleted
        );
        println!("- map={}", result.agent_map.display());
        if cloud["mode"] == "filename_only" {
            println!(
                "- cloud drive: file names only. To search inside files of a folder, ask the user, then run `jikji prepare <folder> --approve-cloud-content`."
            );
        } else if cloud["mode"] == "approved_folder" {
            println!("- cloud drive: contents indexed (approved folder)");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `--approve-cloud-content` / `--revoke-cloud-content`: store the decision before indexing.
fn apply_cloud_decision(args: &PrepareArgs) -> jikji_core::Result<()> {
    if !args.approve_cloud_content && !args.revoke_cloud_content {
        return Ok(());
    }
    let folder = approval_key(&args.root)?;
    if !cloud_drive_path_blocked(&folder) && !cloud_drive_path_blocked(&args.root) {
        return Err(jikji_core::io_error(
            &args.root,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a cloud-drive path: contents of ordinary folders are indexed by default",
            ),
        ));
    }
    if args.approve_cloud_content {
        approve_cloud_content(&folder)?;
    } else {
        revoke_cloud_content(&folder)?;
    }
    Ok(())
}

/// What `prepare` did with contents for this root, for the JSON result.
fn cloud_content_status(root: &std::path::Path, typed: &std::path::Path) -> serde_json::Value {
    let policy = CloudContentPolicy::load(root, &[typed])
        .unwrap_or_else(|_| CloudContentPolicy::load_or_deny(root));
    let mode = policy.mode_for(root);
    let mut value = json!({"mode": mode, "approved_below": policy.approved_below(root)});
    if mode == "filename_only" {
        value["note"] = json!(
            "Cloud drive: only file names, paths, sizes and times were indexed. To search inside files of one folder, ask the user, then run `jikji prepare FOLDER --approve-cloud-content`."
        );
    }
    value
}

fn run_search_only(args: PrepareArgs) -> jikji_core::Result<ExitCode> {
    let stats = reindex_search(&args.root)?;
    if args.json {
        print_json(&json!({
            "root": args.root,
            "search_rows": stats.rows,
            "search_terms": stats.terms,
            "graph_nodes": stats.graph_nodes,
            "graph_edges": stats.graph_edges,
        }))?;
    } else {
        println!("Jikji search reindexed: {}", args.root.display());
        println!("- search_rows={} terms={}", stats.rows, stats.terms);
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn run_deep_index(mut args: PrepareArgs) -> jikji_core::Result<ExitCode> {
    args.enable_media_index = true;
    args.deep_archive_index = true;
    run_prepare(args)
}

pub(crate) fn prepare_options_from_args(args: &PrepareArgs) -> PrepareOptions {
    PrepareOptions {
        include_hidden: args.include_hidden,
        include_sensitive: args.include_sensitive,
        max_files: normalize_max_files(args.max_files),
        exclude_patterns: args.exclude.clone(),
        extensions: Vec::new(),
        max_file_bytes: None,
        max_hash_bytes: args.max_hash_bytes,
        parse_timeout_seconds: args.parse_timeout,
        doc_text_max_chars: args.doc_text_max_chars,
        doc_text_chunk_chars: args.doc_text_chunk_chars,
        enable_media_index: args.enable_media_index,
        media_index_max_mb: args.media_index_max_mb,
        deep_archive_index: args.deep_archive_index,
        archive_max_entries: args.archive_max_entries,
        archive_max_entry_bytes: args.archive_max_entry_bytes,
        archive_max_total_bytes: args.archive_max_total_bytes,
        clean_reindex: args.clean_reindex,
        skip_root_map: args.no_root_map,
    }
}

pub(crate) fn normalize_max_files(max_files: Option<usize>) -> Option<usize> {
    max_files.filter(|limit| *limit > 0)
}

pub(crate) fn run_clean(args: CleanArgs) -> jikji_core::Result<ExitCode> {
    let result = clean(
        &args.root,
        CleanOptions {
            dry_run: args.dry_run,
            force: args.force,
        },
    )?;
    if args.json {
        print_json(&result)?;
    } else if result.ok {
        let label = if result.dry_run {
            "WOULD_REMOVE"
        } else {
            "REMOVED"
        };
        let paths = if result.dry_run {
            &result.would_remove
        } else {
            &result.removed
        };
        for path in paths {
            println!("{label} {}", path.display());
        }
    } else if let Some(error) = &result.error {
        eprintln!("{error}");
    }
    Ok(if result.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
