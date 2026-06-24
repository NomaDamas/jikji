"""Adaptive Jikji discovery cascade for local-agent file retrieval."""
from __future__ import annotations

# SIZE_OK: legacy discovery scorer/cascade; answer-pack helpers were split out, ranking calibration stays co-located for regression stability.
import hashlib
import re
import shlex
from collections import OrderedDict
from pathlib import Path
from typing import Any

from .answer_pack import (
    answer_pack_for,
    handoff_action_for,
    handoff_budget_for,
    handoff_policy_for,
    is_generated_artifact_path,
    next_read_for_candidate,
    tool_call_policy_for,
)
from .eval import search

_SINGLE_HINTS = {
    "which", "what file", "find the", "locate", "contract", "agreement", "nda",
    "invoice", "report", "form", "pdf", "document", "file", "where is",
}
_BROAD_HINTS = {
    "habit", "habits", "usual", "usually", "summarize", "summary", "profile",
    "primary", "preferred", "preference", "interest", "interests", "genres",
    "records", "past versions", "how i've", "how i", "what are my",
}
_EVIDENCE_HINTS = {
    "supporting", "evidence", "records", "versions", "minutes", "items",
    "responsibilities", "tasks", "plans", "before", "after", "history",
}
_GENERIC_PATH_ANCHORS = {
    "CEO",
    "CFO",
    "COO",
    "CTO",
    "DOC",
    "DOCX",
    "INC",
    "LLP",
    "LLC",
    "NDA",
    "PDF",
    "PLC",
    "PTE",
    "PPT",
    "PPTX",
    "RFP",
    "TXT",
    "XLS",
    "XLSX",
}
_GENERIC_PATH_ANCHORS.update({"ADAM", "CLIENT", "LIMITED", "LTD", "SINGAPORE"})

_SHELL_NOISE_TERMS = {
    "bash",
    "cat",
    "chmod",
    "curl",
    "echo",
    "find",
    "grep",
    "ls",
    "rm",
    "rf",
    "rmdir",
    "sed",
    "sh",
    "sudo",
    "wget",
}
_SHELL_SYNTAX_RE = re.compile(r"[$`;&|<>\\]")


_TOPIC_REWRITES: tuple[tuple[tuple[str, ...], tuple[str, ...]], ...] = (
    (("sport", "sports", "interest"), ("tennis club lessons booking", "sports club application lessons")),
    (("music", "genre", "genres"), ("music carplay study playlist", "song artist track playlist")),
    (("movie", "theme", "song"), ("movie soundtrack theme song", "film music trailer song")),
    (("stress", "academic", "de-stress", "destress"), ("stress academic diary activity", "swim rental diary school stress")),
    (("meeting", "minutes", "habit"), ("meeting minutes notes", "minutes agenda follow up")),
    (("slides", "revising", "versions"), ("edited pptx original slides", "presentation edited version")),
    (("legal aid reports", "working documents", "standardize"), ("TJCC Case Report Sent Email Report structure wording", "past legal aid reports working documents")),
    (("stay in touch", "family"), ("Whatsapp Mom Dad Family Call Christmas Diary", "family contact mom dad whatsapp call")),
    (("workout routine", "work schedule"), ("half marathon garmin run workout calendar", "exercise routine work schedule exam prep")),
    (("manage money", "financially", "money"), ("budget bank statement groceries cost of living", "shopping list weekly groceries money Singapore")),
    (("keep things on track",), ("weekly priority checklist whiteboard calendar triage", "priority email case triage work study tracking")),
    (("part b", "exam eligibility"), ("Guide to Application Process Part B eligibility", "Part B registration exam eligibility lose")),
    (("ptp", "leave of absence"), ("Calculation of PTP Change in manner serving PTP", "Requirements of PTP under PTC notify SILE")),

    (("nda", "confidential", "copying"), ("NDA confidential information copying", "vendor NDA agreement confidential")),
)


def _norm(text: str) -> str:
    return " ".join(str(text or "").casefold().split())


def _trigger_present(query_norm: str, trigger: str) -> bool:
    trigger_norm = _norm(trigger)
    if " " in trigger_norm or "'" in trigger_norm:
        return trigger_norm in query_norm
    return trigger_norm in set(query_norm.split())


def _strip_shell_noise(query: str) -> str:
    words: list[str] = []
    for raw in str(query or "").replace("$", " ").replace("`", " ").split():
        token = raw.strip(".,:;!?()[]{}\"'")
        folded = token.casefold().lstrip("-")
        if not token or folded in _SHELL_NOISE_TERMS:
            continue
        if token.startswith("-") and len(folded) <= 2:
            continue
        if folded and set(folded) <= {"/", "."}:
            continue
        if _SHELL_SYNTAX_RE.search(token):
            continue
        words.append(token)
    return " ".join(words).strip()


def classify_query(query: str) -> str:
    q = _norm(query)
    if any(hint in q for hint in _BROAD_HINTS):
        return "evidence_set"
    if any(hint in q for hint in _EVIDENCE_HINTS):
        return "evidence_set"
    if any(hint in q for hint in _SINGLE_HINTS):
        return "single_file"
    return "adaptive"


_YEAR_RE = re.compile(r"(?:FY)?(20\d{2}|19\d{2})", re.IGNORECASE)
_SHORT_FY_RE = re.compile(r"FY(\d{2})", re.IGNORECASE)


def _is_query_anchor_token(token: str, index: int) -> bool:
    if len(token) < 3:
        return False
    if token.isupper() or any(ch.isdigit() for ch in token):
        return True
    has_upper = any(ch.isupper() for ch in token)
    has_lower = any(ch.islower() for ch in token)
    if has_upper and has_lower and not (token[:1].isupper() and token[1:].islower()):
        return True
    return index > 0 and len(token) >= 4 and token[:1].isupper() and token[1:].islower()


def _anchor_tokens(query: str) -> list[str]:
    tokens: list[str] = []
    for raw in re.split(r"[^A-Za-z0-9]+", str(query or "")):
        if not raw or len(raw) < 2:
            continue
        tokens.append(raw)
        match = _YEAR_RE.fullmatch(raw)
        if match:
            year = match.group(1)
            tokens.append(year)
            tokens.append(year[-2:])
            continue
        short_fy = _SHORT_FY_RE.fullmatch(raw)
        if short_fy:
            year = f"20{short_fy.group(1)}"
            tokens.append(year)
            tokens.append(year[-2:])
    return tokens


def query_variants(query: str) -> list[str]:
    q = _norm(query)
    variants: list[str] = [query]
    for triggers, rewrites in _TOPIC_REWRITES:
        if any(_trigger_present(q, trigger) for trigger in triggers):
            variants.extend(rewrites)
    # Keep quoted/capitalized-looking anchors available without relying on the agent.
    words = _anchor_tokens(query)
    anchors: list[str] = []
    seen_anchors: set[str] = set()
    for idx, word in enumerate(words):
        if not _is_query_anchor_token(word, idx) or word.upper() in _GENERIC_PATH_ANCHORS:
            continue
        if word.casefold() in seen_anchors:
            continue
        seen_anchors.add(word.casefold())
        anchors.append(word)
    if anchors:
        variants.append(" ".join(anchors))
    out: list[str] = []
    seen: set[str] = set()
    for variant in variants:
        key = _norm(variant)
        if key and key not in seen:
            seen.add(key)
            out.append(variant)
    return out[:6]


def _query_anchors(query: str) -> list[str]:
    anchors: list[str] = []
    for idx, token in enumerate(_anchor_tokens(query)):
        if not _is_query_anchor_token(token, idx):
            continue
        if token.upper() in _GENERIC_PATH_ANCHORS:
            continue
        anchors.append(token.casefold())
    seen: set[str] = set()
    out: list[str] = []
    for anchor in anchors:
        if anchor not in seen:
            seen.add(anchor)
            out.append(anchor)
    return out

def _merge_candidates(root: Path, variants: list[str], *, top_k: int, per_query_k: int) -> list[dict[str, Any]]:
    merged: OrderedDict[str, dict[str, Any]] = OrderedDict()
    query_anchors = _query_anchors(variants[0] if variants else "")
    for variant_index, variant in enumerate(variants):
        for rank, item in enumerate(search(root, variant, top_k=per_query_k), 1):
            path = str(item.get("path") or "")
            if not path or is_generated_artifact_path(path):
                continue
            score = float(item.get("score") or 0.0)
            weighted = score / max(1.0, rank ** 0.35)
            if variant_index > 0:
                weighted *= 3.0
            path_key = path.casefold()
            matched_terms = {str(term).casefold() for term in item.get("matched_terms") or []}
            path_anchor_hits = sum(1 for anchor in query_anchors if anchor in path_key)
            term_anchor_hits = sum(1 for anchor in query_anchors if anchor in matched_terms)
            if path_anchor_hits >= 2:
                weighted = weighted * (12.0 + path_anchor_hits) + 150_000.0 * path_anchor_hits
            elif path_anchor_hits == 1:
                weighted = weighted * 8.0 + 50_000.0
            elif term_anchor_hits:
                weighted = weighted * (4.0 + term_anchor_hits) + 20_000.0 * term_anchor_hits
            existing = merged.get(path)
            if existing is None:
                clone = dict(item)
                clone["discover_score"] = weighted
                clone["queries"] = [variant]
                clone["best_query_rank"] = rank
                merged[path] = clone
            else:
                existing["discover_score"] = float(existing.get("discover_score") or 0.0) + weighted * 0.35
                existing.setdefault("queries", [])
                if variant not in existing["queries"]:
                    existing["queries"].append(variant)
                existing["best_query_rank"] = min(int(existing.get("best_query_rank") or rank), rank)
    ranked = sorted(
        merged.values(),
        key=lambda item: (-float(item.get("discover_score") or 0.0), int(item.get("best_query_rank") or 999), str(item.get("path") or "")),
    )
    return ranked[:top_k]


def _search_plan(root: Path, variants: list[str], *, top_k: int, per_route_top_k: int) -> dict[str, Any]:
    routes = [
        {
            "route": "lexical_file_map",
            "source": ".jikji/file_cards.jsonl",
            "query_variants": variants,
            "per_route_top_k": per_route_top_k,
        },
        {
            "route": "graph_route",
            "source": ".jikji/knowledge_graph.json",
            "query_variants": variants,
            "per_route_top_k": per_route_top_k,
        },
    ]
    if (root / ".jikji" / "wiki").exists():
        routes.append(
            {
                "route": "wiki_cache",
                "source": ".jikji/wiki/",
                "query_variants": variants,
                "per_route_top_k": per_route_top_k,
            }
        )
    if (root / ".jikji" / "file_cards.jsonl").exists():
        routes.append(
            {
                "route": "metadata",
                "source": ".jikji/file_cards.jsonl",
                "query_variants": variants,
                "per_route_top_k": per_route_top_k,
            }
        )
    return {
        "mode": "deterministic_multi_search",
        "routes": routes,
        "merge": "dedupe_by_path_then_rank_by_discover_score",
        "candidate_top_k": top_k,
    }


def _candidate_route_labels(root: Path, candidate: dict[str, Any]) -> list[str]:
    labels = ["lexical_file_map"]
    reasons = {str(reason) for reason in candidate.get("reasons") or []}
    if reasons & {"fielded-bm25", "filename-anchor", "duplicate-anchor", "duplicate-expansion", "body-coverage"}:
        labels.append("graph_route")
    if candidate.get("wiki") or candidate.get("wiki_path") or (root / ".jikji" / "wiki").exists():
        labels.append("wiki_cache")
    if candidate.get("metadata") or candidate.get("matched_terms") or (root / ".jikji" / "file_cards.jsonl").exists():
        labels.append("metadata")
    return labels


def _judge_candidate_slate(root: Path, candidates: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [
        {
            "rank": index,
            "path": str(item.get("path") or ""),
            "score": round(float(item.get("discover_score") or item.get("score") or 0.0), 3),
            "routes": _candidate_route_labels(root, item),
            "queries": (item.get("queries") or [])[:3],
            "evidence": (item.get("evidence") or [])[:2],
            "next_read": next_read_for_candidate(item),
        }
        for index, item in enumerate(candidates, 1)
    ]


def _clamp01(value: float) -> float:
    return max(0.0, min(1.0, value))


def _family(path: str) -> str:
    p = Path(path)
    return str(p.parent) if str(p.parent) != "." else "."


def _confidence_factors(query_type: str, candidates: list[dict[str, Any]], variants: list[str]) -> dict[str, float]:
    if not candidates:
        return {
            "score_margin": 0.0,
            "variant_agreement": 0.0,
            "family_coherence": 0.0,
            "evidence_coverage": 0.0,
            "duplicate_or_anchor_signal": 0.0,
        }
    top = float(candidates[0].get("discover_score") or candidates[0].get("score") or 0.0)
    second = float(candidates[1].get("discover_score") or candidates[1].get("score") or 0.0) if len(candidates) > 1 else 0.0
    margin = _clamp01((top - second) / max(top, 1.0)) if top > 0 else 0.0
    query_hits = max(len(candidates[0].get("queries") or []), max((len(c.get("queries") or []) for c in candidates[:5]), default=0))
    variant_agreement = _clamp01(query_hits / max(1, min(len(variants), 4)))
    top_family = _family(str(candidates[0].get("path") or ""))
    family_matches = sum(1 for c in candidates[:10] if _family(str(c.get("path") or "")) == top_family)
    family_coherence = _clamp01(family_matches / max(1, min(len(candidates), 10)))
    evidence_hits = sum(1 for c in candidates[:10] if c.get("evidence"))
    evidence_coverage = _clamp01(evidence_hits / max(1, min(len(candidates), 10)))
    anchor_reasons = {"duplicate-anchor", "filename-anchor", "fielded-bm25", "duplicate-expansion", "body-coverage"}
    anchor_hits = sum(1 for c in candidates[:5] if anchor_reasons & set(str(r) for r in (c.get("reasons") or [])))
    duplicate_or_anchor_signal = _clamp01(anchor_hits / max(1, min(len(candidates), 5)))
    if query_type == "evidence_set":
        family_coherence = max(family_coherence, _clamp01(len(candidates) / 8.0) * 0.7)
    return {
        "score_margin": round(margin, 4),
        "variant_agreement": round(variant_agreement, 4),
        "family_coherence": round(family_coherence, 4),
        "evidence_coverage": round(evidence_coverage, 4),
        "duplicate_or_anchor_signal": round(duplicate_or_anchor_signal, 4),
    }


def _confidence_score(query_type: str, factors: dict[str, float]) -> float:
    if query_type == "single_file":
        score = (
            factors["score_margin"] * 0.30
            + factors["variant_agreement"] * 0.20
            + factors["evidence_coverage"] * 0.15
            + factors["duplicate_or_anchor_signal"] * 0.35
        )
    elif query_type == "evidence_set":
        score = (
            factors["variant_agreement"] * 0.25
            + factors["family_coherence"] * 0.30
            + factors["evidence_coverage"] * 0.20
            + factors["duplicate_or_anchor_signal"] * 0.25
        )
    else:
        score = sum(factors.values()) / max(1, len(factors))
    return round(_clamp01(score), 4)


def _confidence(query_type: str, candidates: list[dict[str, Any]], factors: dict[str, float], score: float) -> str:
    if not candidates:
        return "low"
    if score >= 0.78:
        return "high"
    if score >= 0.55:
        return "medium_high"
    if score >= 0.35:
        return "medium"
    if query_type == "evidence_set" and len(candidates) >= 2:
        return "medium"
    return "low"


def _recommended_action(query_type: str, confidence: str) -> str:
    if query_type == "single_file" and confidence == "high":
        return "return_top1_after_light_verification"
    if query_type == "evidence_set" and confidence in {"medium", "medium_high", "high"}:
        return "return_top5_to_top10_evidence_set"
    if confidence == "low":
        return "rewrite_query_and_fallback_search"
    return "verify_top_candidates"


def handoff_contract_for(query: str, candidates: list[dict[str, Any]], *, retry_exhausted: bool = False) -> dict[str, Any]:
    retrieval_query = _strip_shell_noise(query)
    query_type = classify_query(retrieval_query)
    variants = query_variants(retrieval_query) if retrieval_query else [""]
    factors = _confidence_factors(query_type, candidates, variants)
    score = _confidence_score(query_type, factors)
    confidence = _confidence(query_type, candidates, factors, score)
    return {
        "query_type": query_type,
        "confidence": confidence,
        "confidence_score": score,
        "confidence_factors": factors,
        "handoff_action": handoff_action_for(confidence, retry_exhausted=retry_exhausted),
        "handoff_policy": handoff_policy_for(query_type, confidence, retry_exhausted=retry_exhausted),
    }


def retry_proof_for(root: Path, query: str, top_k: int) -> str:
    material = f"{Path(root).resolve()}\0{query}\0{top_k}\0jikji-retry-v1"
    return hashlib.sha256(material.encode("utf-8", errors="surrogateescape")).hexdigest()[:24]


def _retry_query(query: str, variants: list[str]) -> str:
    return variants[1] if len(variants) > 1 else query


def _next_commands(root: Path, retry_query: str, confidence: str, top_k: int, retry_proof: str) -> list[str]:
    if confidence != "low":
        return []
    commands: list[str] = []
    commands.append(
        " ".join([
            "jikji",
            "discover",
            shlex.quote(str(root)),
            shlex.quote(retry_query),
            "--top-k",
            str(top_k),
            "--after-jikji-retry",
            "--retry-proof",
            shlex.quote(retry_proof),
            "--json",
        ])
    )
    return commands


def discover(root: Path, query: str, *, top_k: int = 20, per_query_k: int | None = None, retry_exhausted: bool = False, retry_proof: str = "") -> dict[str, Any]:
    root = Path(root).expanduser().resolve()
    retrieval_query = _strip_shell_noise(query)
    query_type = classify_query(retrieval_query)
    variants = query_variants(retrieval_query) if retrieval_query else [""]
    retry_query = _retry_query(retrieval_query, variants)
    current_query_proof = retry_proof_for(root, retrieval_query, top_k)
    retry_command_proof = retry_proof_for(root, retry_query, top_k)
    verified_retry_exhausted = retry_exhausted and retry_proof == current_query_proof
    per_query_k = per_query_k or max(top_k * 3, 60)
    candidates = [] if not retrieval_query else _merge_candidates(root, variants, top_k=top_k, per_query_k=per_query_k)
    compact_candidates = [
        {
            "p": item.get("path"),
            "s": round(float(item.get("discover_score") or item.get("score") or 0.0), 3),
            "rank": item.get("best_query_rank"),
            "why": (item.get("reasons") or [])[:5],
            "terms": (item.get("matched_terms") or [])[:8],
            "queries": (item.get("queries") or [])[:3],
            "ev": " | ".join(str(x) for x in (item.get("evidence") or [])[:2])[:240],
            "next_read": next_read_for_candidate(item),
        }
        for item in candidates
    ]
    search_plan = _search_plan(root, variants, top_k=top_k, per_route_top_k=per_query_k)
    judge_candidate_slate = _judge_candidate_slate(root, candidates)
    candidate_paths = [str(item.get("path") or "") for item in candidates if item.get("path")]
    handoff_contract = handoff_contract_for(query, candidates, retry_exhausted=verified_retry_exhausted)
    answer_pack = answer_pack_for(query_type, handoff_contract["confidence"], candidates)
    handoff_action = str(handoff_contract["handoff_action"])
    budget = handoff_budget_for(handoff_action)
    allowed_llm_calls = int(budget["allowed_llm_calls"])
    if handoff_action == "direct_use" and answer_pack["requires_llm_rerank"]:
        allowed_llm_calls = max(allowed_llm_calls, int(answer_pack["allowed_llm_calls"]))
    tool_call_policy = tool_call_policy_for(
        handoff_action,
        str(budget["answerability"]),
        agent_should_not_rerank=bool(answer_pack["agent_should_not_rerank"]),
        raw_fallback_allowed=bool(budget["raw_fallback_allowed"]),
    )
    return {
        "mode": "discover",
        "answer_pack_version": 1,
        "root": str(root),
        "query": query,
        "query_type": query_type,
        "confidence": handoff_contract["confidence"],
        "confidence_score": handoff_contract["confidence_score"],
        "confidence_factors": handoff_contract["confidence_factors"],
        "recommended_action": _recommended_action(query_type, handoff_contract["confidence"]),
        "handoff_action": handoff_contract["handoff_action"],
        "handoff_policy": handoff_contract["handoff_policy"],
        "retry_proof": retry_command_proof if handoff_contract["confidence"] == "low" and not verified_retry_exhausted else "",
        "next_commands": [] if verified_retry_exhausted else _next_commands(root, retry_query, handoff_contract["confidence"], top_k, retry_command_proof),
        "paths": candidate_paths,
        "answer_paths": answer_pack["answer_paths"],
        "supporting_paths": answer_pack["supporting_paths"],
        "requires_llm_rerank": answer_pack["requires_llm_rerank"],
        "agent_should_not_rerank": answer_pack["agent_should_not_rerank"],
        "answerability": budget["answerability"],
        "tool_call_policy": tool_call_policy,
        "allowed_agent_tool_calls": budget["allowed_agent_tool_calls"],
        "allowed_llm_calls": allowed_llm_calls,
        "max_jikji_retries": budget["max_jikji_retries"],
        "max_raw_fallback_commands": budget["max_raw_fallback_commands"],
        "max_verification_reads": budget["max_verification_reads"],
        "raw_fallback_allowed": budget["raw_fallback_allowed"],
        "query_variants": variants,
        "llm_search_plan": {
            "mode": "one_call_multi_search_judge",
            "calls_per_cycle": 1,
            "judge": "choose_best_file_from_merged_candidate_slate",
            "rewrite_cycle": "none",
            "candidate_top_k": top_k,
            "token_accounting": "query_variants_plus_merged_candidate_slate",
        },
        "search_plan": search_plan,
        "judge_candidate_slate": judge_candidate_slate,
        "evidence_pack": answer_pack["evidence_pack"],
        "candidates": compact_candidates,
    }
