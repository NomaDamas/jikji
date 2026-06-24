from __future__ import annotations

from pathlib import PurePosixPath
from typing import Any

GENERATED_PATH_NAMES = {".jikji_agent_map.md", "000_JIKJI_AGENT_MAP.md"}
MAX_EVIDENCE_CHARS = 240


def is_generated_artifact_path(path: str) -> bool:
    normalized = str(path or "").replace("\\", "/")
    while normalized.startswith("./"):
        normalized = normalized[2:]
    parts = PurePosixPath(normalized).parts
    return ".jikji" in parts or PurePosixPath(normalized).name in GENERATED_PATH_NAMES


def original_candidates(candidates: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [
        item
        for item in candidates
        if not is_generated_artifact_path(str(item.get("path") or item.get("p") or ""))
    ]


def candidate_path(candidate: dict[str, Any]) -> str:
    return str(candidate.get("path") or candidate.get("p") or "")


def handoff_action_for(confidence: str, *, retry_exhausted: bool = False) -> str:
    if confidence == "low":
        return "raw_fallback_after_retry" if retry_exhausted else "jikji_retry"
    return "direct_use"


def retry_budget_for(action: str) -> str:
    if action == "jikji_retry":
        return "run_exactly_1_more_jikji_query_before_raw_fallback"
    if action == "raw_fallback_after_retry":
        return "raw_fallback_allowed_after_failed_jikji_retry"
    return "zero_extra_discovery_calls"


def handoff_policy_for(query_type: str, confidence: str, *, retry_exhausted: bool = False) -> dict[str, Any]:
    action = handoff_action_for(confidence, retry_exhausted=retry_exhausted)
    if action == "jikji_retry":
        return {
            "agent_budget": retry_budget_for(action),
            "use_payload_directly": False,
            "raw_fallback_allowed": "only_after_one_jikji_retry_fails",
            "verification": "inspect_original_top_1_to_3_only",
        }
    if action == "raw_fallback_after_retry":
        return {
            "agent_budget": retry_budget_for(action),
            "use_payload_directly": False,
            "raw_fallback_allowed": "yes_after_one_jikji_retry_failed",
            "verification": "inspect_original_top_1_to_3_only_or_raw_fallback",
        }
    if query_type == "evidence_set":
        return {
            "agent_budget": retry_budget_for(action),
            "use_payload_directly": True,
            "raw_fallback_allowed": "no",
            "verification": "return_top_5_to_10_or_inspect_original_top_1_to_3_only",
        }
    return {
        "agent_budget": retry_budget_for(action),
        "use_payload_directly": True,
        "raw_fallback_allowed": "no",
        "verification": "return_top_path_or_inspect_original_top_1_only",
    }


def handoff_budget_for(action: str) -> dict[str, Any]:
    if action == "jikji_retry":
        return {
            "answerability": "needs_one_jikji_retry",
            "allowed_agent_tool_calls": 1,
            "allowed_llm_calls": 1,
            "max_jikji_retries": 1,
            "max_raw_fallback_commands": 0,
            "max_verification_reads": 0,
            "raw_fallback_allowed": False,
        }
    if action == "raw_fallback_after_retry":
        return {
            "answerability": "needs_raw_fallback_after_retry",
            "allowed_agent_tool_calls": 3,
            "allowed_llm_calls": 2,
            "max_jikji_retries": 0,
            "max_raw_fallback_commands": 2,
            "max_verification_reads": 3,
            "raw_fallback_allowed": True,
        }
    return {
        "answerability": "answerable_from_payload",
        "allowed_agent_tool_calls": 0,
        "allowed_llm_calls": 0,
        "max_jikji_retries": 0,
        "max_raw_fallback_commands": 0,
        "max_verification_reads": 3,
        "raw_fallback_allowed": False,
    }

POST_FIND_FORBIDDEN_TOOLS = (
    "read_file",
    "search",
    "grep",
    "rg",
    "find",
    "fd",
    "ls",
    "cat",
    "tree",
    "glob",
    "skills_list",
)


def tool_call_policy_for(
    action: str,
    answerability: str,
    *,
    agent_should_not_rerank: bool,
    raw_fallback_allowed: bool,
) -> dict[str, Any]:
    """Enforce 'stop after a sufficient find' so agents do not keep calling tools.

    When ``jikji find`` returns a payload that is answerable on its own the only
    allowed follow-ups are minimal verification or returning the result. Any
    further discovery tool is forbidden unless the handoff contract explicitly
    permits ``jikji_retry`` or ``raw_fallback_after_retry``.
    """
    answerable = (
        action == "direct_use"
        or answerability == "answerable_from_payload"
        or bool(agent_should_not_rerank)
    )
    if answerable and not raw_fallback_allowed:
        return {
            "stop_after_find": True,
            "allowed_followups": ["verify_top_1_path", "return_answer_paths_to_user"],
            "forbidden_tools": list(POST_FIND_FORBIDDEN_TOOLS),
            "rerank_locked": bool(agent_should_not_rerank),
            "reason": "jikji_find_result_is_answerable_from_payload",
            "escape_hatch": "none_unless_handoff_action_allows_jikji_retry_or_raw_fallback_after_retry",
        }
    if action == "jikji_retry":
        allowed_followups = ["run_one_sharper_jikji_find_retry"]
    elif action == "raw_fallback_after_retry":
        allowed_followups = ["verify_top_1_to_3_paths", "raw_fallback_after_retry"]
    else:
        allowed_followups = ["verify_top_1_to_3_paths"]
    return {
        "stop_after_find": False,
        "allowed_followups": allowed_followups,
        "forbidden_tools": [],
        "rerank_locked": bool(agent_should_not_rerank),
        "reason": f"jikji_find_result_requires_{action}",
        "escape_hatch": action,
    }


def next_read_for_candidate(candidate: dict[str, Any]) -> dict[str, str]:
    cache = str(candidate.get("cache") or candidate.get("text_cache_path") or "")
    if cache:
        return {"kind": "cache", "path": cache}
    wiki = str(candidate.get("wiki") or candidate.get("wiki_path") or "")
    if wiki:
        return {"kind": "wiki", "path": wiki}
    path = candidate_path(candidate)
    if path:
        return {"kind": "original", "path": path}
    return {"kind": "none", "path": ""}


def answer_pack_for(query_type: str, confidence: str, candidates: list[dict[str, Any]]) -> dict[str, Any]:
    originals = original_candidates(candidates)
    if query_type == "single_file" and confidence == "high":
        answer_limit = 1
    elif query_type == "single_file" and confidence == "medium_high":
        answer_limit = 3
    elif query_type == "evidence_set":
        answer_limit = 10
    else:
        answer_limit = 5
    answer_paths = [path for item in originals[:answer_limit] if (path := candidate_path(item))]
    supporting_paths = [
        path
        for item in originals[answer_limit:10]
        if (path := candidate_path(item)) and path not in answer_paths
    ]
    direct = bool(answer_paths) and (confidence == "high" or query_type == "evidence_set")
    evidence_pack = [
        {
            "path": candidate_path(item),
            "why": (item.get("reasons") or [])[:5],
            "matched_terms": (item.get("matched_terms") or [])[:8],
            "evidence": [
                str(value)[:MAX_EVIDENCE_CHARS]
                for value in (item.get("evidence") or [])[:2]
            ],
            "next_read": next_read_for_candidate(item),
        }
        for item in originals[: min(len(originals), max(1, min(answer_limit, 5)))]
    ]
    return {
        "answer_paths": answer_paths,
        "supporting_paths": supporting_paths,
        "evidence_pack": evidence_pack,
        "requires_llm_rerank": not direct,
        "agent_should_not_rerank": direct,
        "allowed_llm_calls": 1 if answer_paths else 2,
    }
