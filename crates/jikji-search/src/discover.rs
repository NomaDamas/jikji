use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::answer_pack::{answer_pack_for, handoff_budget, handoff_policy, tool_call_policy};
use crate::discover_contract::{
    confidence_factors, confidence_for, discover_graph_route_paths, judge_slate, next_commands,
    recommended_action, search_plan,
};
use crate::discover_query::{
    anchor_tokens, classify_query, retry_proof_for, strategy_variants, strip_shell_noise,
};
use crate::searcher::{search, SearchCandidate, SearchOptions};
use jikji_core::Result;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverOptions {
    pub top_k: usize,
    pub retry_exhausted: bool,
    pub retry_proof: String,
    pub lite: bool,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self {
            top_k: 20,
            retry_exhausted: false,
            retry_proof: String::new(),
            lite: false,
        }
    }
}
fn run_llm_judge(input: &Value, candidates: &mut Vec<SearchCandidate>) -> Value {
    let Some(command) = std::env::var_os("JIKJI_LLM_JUDGE_COMMAND") else {
        return json!({"status":"unavailable","selected_path":null,"fallback":"merged_candidates"});
    };
    let parts = command
        .to_string_lossy()
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let Some((program, args)) = parts.split_first() else {
        return json!({"status":"unavailable","selected_path":null,"fallback":"merged_candidates"});
    };
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return json!({"status":"failed","error":error.to_string(),"fallback":"merged_candidates"});
        }
    };
    let payload = match serde_json::to_vec(input) {
        Ok(payload) => payload,
        Err(error) => {
            return json!({"status":"failed","error":error.to_string(),"fallback":"merged_candidates"});
        }
    };
    if child.stdin.as_mut().is_none() || child.stdin.as_mut().unwrap().write_all(&payload).is_err()
    {
        return json!({"status":"failed","error":"judge stdin write failed","fallback":"merged_candidates"});
    }
    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return json!({"status":"failed","error":"judge timed out after 30 seconds","fallback":"merged_candidates"});
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                return json!({"status":"failed","error":error.to_string(),"fallback":"merged_candidates"});
            }
        }
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(error) => {
            return json!({"status":"failed","error":error.to_string(),"fallback":"merged_candidates"});
        }
    };
    if !output.status.success() {
        return json!({"status":"failed","error":format!("judge exited with {}", output.status),"fallback":"merged_candidates"});
    }
    let response: Value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(error) => {
            return json!({"status":"failed","error":error.to_string(),"fallback":"merged_candidates"});
        }
    };
    let selected = response
        .get("path")
        .and_then(Value::as_str)
        .or_else(|| response.as_str());
    if let Some(path) = selected {
        if let Some(index) = candidates
            .iter()
            .position(|candidate| candidate.path == path)
        {
            let chosen = candidates.remove(index);
            candidates.insert(0, chosen);
            return json!({"status":"selected","selected_path":path,"fallback":null});
        }
    }
    json!({"status":"invalid_selection","selected_path":null,"fallback":"merged_candidates"})
}

pub fn discover(root: &Path, query: &str, options: DiscoverOptions) -> Result<Value> {
    let request = DiscoverRequest::from(root, query, &options);
    let (mut candidates, strategies) = if request.retrieval_query.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        merge_candidates(root, &request.variants, options.top_k)?
    };
    let route_paths = if options.lite {
        HashSet::new()
    } else {
        discover_graph_route_paths(root)
    };
    let judge_input = json!({
        "original_query": query,
        "strategies": strategies,
        "merged_candidates": judge_slate(&candidates, &route_paths),
    });
    let judge_result = if options.lite {
        json!({"status":"skipped","selected_path":null,"fallback":"merged_candidates"})
    } else {
        run_llm_judge(&judge_input, &mut candidates)
    };
    let confidence = confidence_for(&request.query_type, &candidates);
    let action = handoff_action(confidence, request.verified_retry);
    let answer_pack = answer_pack_for(&request.query_type, confidence, &candidates);
    let budget = handoff_budget(action);
    let raw_fallback = budget["raw_fallback_allowed"].as_bool().unwrap_or(false);
    let should_not_rerank = answer_pack["agent_should_not_rerank"]
        .as_bool()
        .unwrap_or(false);
    Ok(json!({
        "mode": "discover",
        "answer_pack_version": 1,
        "root": root.display().to_string(),
        "query": query,
        "query_type": request.query_type,
        "confidence": confidence,
        "confidence_score": if confidence == "high" { 0.9 } else if confidence == "medium_high" { 0.6 } else { 0.0 },
        "confidence_factors": confidence_factors(&candidates, confidence),
        "recommended_action": recommended_action(&request.query_type, confidence),
        "handoff_action": action,
        "handoff_policy": handoff_policy(&request.query_type, action),
        "retry_proof": if confidence == "low" && !request.verified_retry { request.retry_command_proof.clone() } else { String::new() },
        "next_commands": next_commands(root, &request.retry_query, confidence, options.top_k, &request.retry_command_proof, request.verified_retry),
        "paths": candidate_paths(&candidates),
        "answer_paths": answer_pack["answer_paths"].clone(),
        "supporting_paths": answer_pack["supporting_paths"].clone(),
        "requires_llm_rerank": answer_pack["requires_llm_rerank"].clone(),
        "agent_should_not_rerank": answer_pack["agent_should_not_rerank"].clone(),
        "answerability": budget["answerability"].clone(),
        "tool_call_policy": tool_call_policy(action, should_not_rerank, raw_fallback),
        "allowed_agent_tool_calls": budget["allowed_agent_tool_calls"].clone(),
        "allowed_llm_calls": budget["allowed_llm_calls"].clone(),
        "max_jikji_retries": budget["max_jikji_retries"].clone(),
        "max_raw_fallback_commands": budget["max_raw_fallback_commands"].clone(),
        "max_verification_reads": budget["max_verification_reads"].clone(),
        "raw_fallback_allowed": budget["raw_fallback_allowed"].clone(),
        "query_variants": request.variants.iter().map(|(_, query)| query).collect::<Vec<_>>(),
        "strategy_metadata": strategy_metadata(&request.variants),
        "strategy_results": strategies,
        "llm_search_plan": {
            "mode": "one_call_multi_search_judge",
            "calls_per_cycle": 1,
            "judge": "choose_best_file_from_merged_candidate_slate",
            "rewrite_cycle": "none",
            "candidate_top_k": options.top_k,
            "token_accounting": "query_variants_plus_merged_candidate_slate",
        },
        "search_plan": search_plan(
            root,
            &request.variants.iter().map(|(_, query)| query.clone()).collect::<Vec<_>>(),
            options.top_k,
        ),
        "judge_candidate_slate": judge_slate(&candidates, &route_paths),
        "llm_judge_input": judge_input,
        "llm_judge_result": judge_result,
        "evidence_pack": answer_pack["evidence_pack"].clone(),
        "candidates": compact_candidates(&candidates),
    }))
}

struct DiscoverRequest {
    retrieval_query: String,
    query_type: String,
    variants: Vec<(String, String)>,
    retry_query: String,
    retry_command_proof: String,
    verified_retry: bool,
}

impl DiscoverRequest {
    fn from(root: &Path, query: &str, options: &DiscoverOptions) -> Self {
        let retrieval_query = strip_shell_noise(query);
        let query_type = classify_query(&retrieval_query);
        let variants = if retrieval_query.is_empty() {
            vec![("lexical".to_owned(), String::new())]
        } else if options.lite {
            vec![("lexical".to_owned(), retrieval_query.clone())]
        } else {
            strategy_variants(&retrieval_query)
        };
        let retry_query = variants
            .get(1)
            .map(|(_, query)| query.clone())
            .unwrap_or_else(|| retrieval_query.clone());
        let current_proof = retry_proof_for(root, &retrieval_query, options.top_k);
        let retry_command_proof = retry_proof_for(root, &retry_query, options.top_k);
        Self {
            retrieval_query,
            query_type,
            variants,
            retry_query,
            retry_command_proof,
            verified_retry: options.retry_exhausted && options.retry_proof == current_proof,
        }
    }
}

fn merge_candidates(
    root: &Path,
    variants: &[(String, String)],
    top_k: usize,
) -> Result<(Vec<SearchCandidate>, Vec<Value>)> {
    let mut merged = BTreeMap::<String, SearchCandidate>::new();
    let original_query = variants.first().map_or("", |(_, query)| query);
    let anchors = anchor_tokens(original_query);
    let mut strategy_results = Vec::new();
    for (variant_idx, (strategy, variant)) in variants.iter().enumerate() {
        let results = search(
            root,
            variant,
            SearchOptions {
                top_k: top_k.max(20) * 3,
            },
        )?;
        strategy_results.push(json!({
            "strategy": strategy,
            "query": variant,
            "top_k": results.iter().take(top_k.max(1)).map(|candidate| json!({
                "path": candidate.path,
                "name": candidate.name,
                "score": candidate.score,
                "reasons": candidate.reasons,
                "matched_terms": candidate.matched_terms,
                "evidence": candidate.evidence,
            })).collect::<Vec<_>>(),
        }));
        for (rank, item) in results.into_iter().enumerate() {
            merge_candidate(
                &mut merged,
                item,
                variant,
                variant_idx,
                rank,
                &anchors,
                original_query,
            );
        }
    }
    let mut out = merged.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        right
            .discover_score
            .unwrap_or(right.score)
            .partial_cmp(&left.discover_score.unwrap_or(left.score))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.best_query_rank.cmp(&right.best_query_rank))
            .then_with(|| left.path.cmp(&right.path))
    });
    out.truncate(top_k.max(1));
    Ok((out, strategy_results))
}

fn handoff_action(confidence: &str, verified_retry: bool) -> &'static str {
    if confidence == "low" {
        if verified_retry {
            "raw_fallback_after_retry"
        } else {
            "jikji_retry"
        }
    } else {
        "direct_use"
    }
}

fn strategy_metadata(variants: &[(String, String)]) -> Vec<Value> {
    variants.iter().enumerate().map(|(index, (strategy, query))| {
        json!({"strategy": strategy, "query": query, "rank": index + 1})
    }).collect()
}

fn compact_candidates(candidates: &[SearchCandidate]) -> Vec<Value> {
    candidates
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            json!({
                "p": item.path,
                "s": item.discover_score.unwrap_or(item.score),
                "rank": item.best_query_rank.unwrap_or(idx + 1),
                "why": item.reasons.iter().take(5).collect::<Vec<_>>(),
                "terms": item.matched_terms.iter().take(8).collect::<Vec<_>>(),
                "queries": item.queries.iter().take(3).collect::<Vec<_>>(),
                "strategies": item.strategies.iter().take(4).collect::<Vec<_>>(),
                "ev": item.evidence.iter().take(2).cloned().collect::<Vec<_>>().join(" | "),
                "next_read": {"kind":"original","path":item.path},
            })
        })
        .collect()
}

fn candidate_paths(candidates: &[SearchCandidate]) -> Vec<String> {
    candidates
        .iter()
        .filter(|candidate| !candidate.path.is_empty())
        .map(|candidate| candidate.path.clone())
        .collect()
}

fn merge_candidate(
    merged: &mut BTreeMap<String, SearchCandidate>,
    item: SearchCandidate,
    variant: &str,
    variant_idx: usize,
    rank: usize,
    anchors: &[String],
    query: &str,
) {
    let weighted = weighted_score(&item, variant_idx, rank, anchors, query);
    merged
        .entry(item.path.clone())
        .and_modify(|existing| {
            existing.discover_score =
                Some(existing.discover_score.unwrap_or(existing.score) + weighted * 0.35);
            if !existing.queries.iter().any(|query| query == variant) {
                existing.queries.push(variant.to_owned());
            }
            let strategy = strategy_name(variant_idx).to_owned();
            if !existing.strategies.iter().any(|item| item == &strategy) {
                existing.strategies.push(strategy);
            }
            existing.best_query_rank =
                Some(existing.best_query_rank.unwrap_or(rank + 1).min(rank + 1));
        })
        .or_insert_with(|| {
            let mut cloned = item;
            cloned.discover_score = Some(weighted);
            cloned.queries = vec![variant.to_owned()];
            cloned.strategies = vec![strategy_name(variant_idx).to_owned()];
            cloned.best_query_rank = Some(rank + 1);
            cloned
        });
}

fn strategy_name(index: usize) -> &'static str {
    match index {
        0 => "lexical",
        1 => "lexical_anchors",
        2 => "semantic",
        _ => "advanced",
    }
}

fn weighted_score(
    item: &SearchCandidate,
    variant_idx: usize,
    rank: usize,
    anchors: &[String],
    query: &str,
) -> f64 {
    let mut weighted = item.score / ((rank + 1) as f64).powf(0.35);
    if variant_idx > 0 {
        weighted *= 3.0;
    }
    let path_folded = item.path.to_lowercase();
    let anchor_hits = anchors
        .iter()
        .filter(|anchor| path_folded.contains(anchor.as_str()))
        .count();
    weighted = if anchor_hits >= 2 {
        weighted * (12.0 + anchor_hits as f64) + 150_000.0 * anchor_hits as f64
    } else if anchor_hits == 1 {
        weighted * 8.0 + 50_000.0
    } else {
        weighted
    };
    weighted
        * generated_search_path_weight(&item.path)
        * extension_hint_weight(query, &item.path)
        * archive_search_path_weight(query, &item.path)
}

fn generated_search_path_weight(path: &str) -> f64 {
    if generated_search_path(path) {
        0.02
    } else {
        1.0
    }
}

fn archive_search_path_weight(query: &str, path: &str) -> f64 {
    if query_has_hint(&query.to_lowercase(), &["zip", "tar", "tgz", "7z", "rar"]) {
        return 1.0;
    }
    let lower = path.to_lowercase();
    if lower.ends_with(".zip")
        || lower.ends_with(".tar")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.gz")
        || lower.ends_with(".7z")
        || lower.ends_with(".rar")
    {
        0.05
    } else {
        1.0
    }
}

fn extension_hint_weight(query: &str, path: &str) -> f64 {
    let hints = extension_hints_from_query(query);
    if hints.is_empty() {
        return 1.0;
    }
    let ext = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if hints.iter().any(|hint| hint == &ext) {
        8.0
    } else {
        0.25
    }
}

fn extension_hints_from_query(query: &str) -> Vec<&'static str> {
    let q = query.to_lowercase();
    let mut hints = Vec::new();
    if query_has_hint(&q, &["발표자료", "파워포인트", "pptx", "ppt"]) {
        hints.extend(["pptx", "ppt"]);
    }
    if query_has_hint(&q, &["한글", "hwp", "hwpx"]) {
        hints.extend(["hwp", "hwpx"]);
    }
    if query_has_hint(&q, &["워드", "docx", "doc"]) {
        hints.extend(["docx", "doc"]);
    }
    if query_has_hint(&q, &["pdf"]) {
        hints.push("pdf");
    }
    if query_has_hint(&q, &["markdown", "마크다운", "md"]) {
        hints.push("md");
    }
    hints.sort_unstable();
    hints.dedup();
    hints
}

fn query_has_hint(query: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| {
        if !needle.is_ascii() {
            query.contains(needle)
        } else {
            query
                .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                .any(|token| token.eq_ignore_ascii_case(needle))
        }
    })
}

fn generated_search_path(path: &str) -> bool {
    path.replace('\\', "/").split('/').any(|part| {
        matches!(
            part,
            "target"
                | "node_modules"
                | "__pycache__"
                | ".venv"
                | "site-packages"
                | "dist-packages"
                | "dist"
                | "build"
        ) || part.ends_with(".egg-info")
            || part
                .strip_prefix("tent.")
                .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit()))
    })
}

#[cfg(test)]
mod lite_tests {
    use super::*;
    use std::path::Path;

    fn options(lite: bool) -> DiscoverOptions {
        DiscoverOptions {
            top_k: 3,
            retry_exhausted: false,
            retry_proof: String::new(),
            lite,
        }
    }

    #[test]
    fn lite_discover_uses_single_lexical_variant() {
        let request = DiscoverRequest::from(Path::new("/tmp"), "invoice hwp", &options(true));
        assert_eq!(
            request.variants,
            vec![("lexical".to_owned(), "invoice hwp".to_owned())]
        );
    }

    #[test]
    fn full_discover_expands_invoice_query() {
        let request = DiscoverRequest::from(Path::new("/tmp"), "invoice hwp", &options(false));
        assert!(
            request.variants.len() > 1,
            "expected expanded variants, got {:?}",
            request.variants
        );
    }

    #[test]
    fn generated_search_path_detects_build_and_worktree_copies() {
        assert!(generated_search_path(
            "target/doc/jikji/search_commands/fn.start_background_refresh.html"
        ));
        assert!(generated_search_path(
            "tent.1/crates/jikji-agent/Cargo.toml"
        ));
        assert!(!generated_search_path(
            "crates/jikji-search/tests/search_find_parity.rs"
        ));
    }

    #[test]
    fn presentation_query_prefers_pptx_over_other_extensions() {
        assert_eq!(
            extension_hints_from_query("정의서 발표자료"),
            vec!["ppt", "pptx"]
        );
        assert!(extension_hint_weight("정의서 발표자료", "목표모델 정의서.pptx") > 1.0);
        assert!(extension_hint_weight("정의서 발표자료", "요구사항정의서.hwp") < 1.0);
        assert!(!query_has_hint("admin", &["md"]));
    }

    #[test]
    fn archive_paths_are_downweighted_unless_query_asks() {
        assert!(archive_search_path_weight("종료감리 문서산출물", "산출물.zip") < 1.0);
        assert_eq!(archive_search_path_weight("산출물 zip", "산출물.zip"), 1.0);
        assert_eq!(archive_search_path_weight("종료감리", "산출물.pptx"), 1.0);
    }
}
