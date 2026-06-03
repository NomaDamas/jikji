from __future__ import annotations

import json
import os
from pathlib import Path

from jikji.agent_index import build_agent_index
from jikji.config import Config


def _jsonl(path):
    return [json.loads(line) for line in path.read_text(encoding="utf-8").split("\n") if line]


def test_prepare_is_non_destructive_and_writes_jikji_artifacts(tmp_path):
    src = tmp_path / "기존" / "회의"
    src.mkdir(parents=True)
    doc = src / "회의록.txt"
    doc.write_text("Jikji smoke", encoding="utf-8")

    result = build_agent_index(tmp_path, Config())

    assert doc.exists()
    assert result.files == 1
    assert (tmp_path / ".jikji" / "agent_map.md").exists()
    assert (tmp_path / "000_JIKJI_AGENT_MAP.md").exists()
    rows = _jsonl(tmp_path / ".jikji" / "file_index.jsonl")
    assert rows[0]["path"] == "기존/회의/회의록.txt"


def test_prepare_prunes_deleted_document_cache(tmp_path):
    doc = tmp_path / "보고서.rtf"
    doc.write_text(r"{\rtf1\ansi Jikji stale body}", encoding="utf-8")
    build_agent_index(tmp_path, Config())
    rows = _jsonl(tmp_path / ".jikji" / "document_index.jsonl")
    assert rows
    cache = tmp_path / rows[0]["text_cache_path"]
    assert cache.exists()

    doc.unlink()
    build_agent_index(tmp_path, Config())

    assert not cache.exists()
    file_rows = _jsonl(tmp_path / ".jikji" / "file_index.jsonl")
    assert any(r.get("status") == "deleted" and r.get("path") == "보고서.rtf" for r in file_rows)


def test_prepare_emits_agent_search_standard_artifacts(tmp_path):
    doc = tmp_path / "보고서.rtf"
    doc.write_text(r"{\rtf1\ansi Jikji standard body}", encoding="utf-8")

    build_agent_index(tmp_path, Config())

    manifest = json.loads((tmp_path / ".jikji" / "manifest.json").read_text(encoding="utf-8"))
    assert manifest["non_destructive"] is True
    assert manifest["cache_key_policy"]
    assert ".jikji/doc_text/" in manifest["owned_paths"]
    assert not (tmp_path / ".jikji" / "search_terms.jsonl").exists()
    assert (tmp_path / ".jikji" / "file_cards.jsonl").exists()
    assert (tmp_path / ".jikji" / "chunk_map.jsonl").exists()
    assert (tmp_path / ".jikji" / "search_index.sqlite").exists()
    assert (tmp_path / ".jikji" / "duplicate_map.jsonl").exists()
    assert (tmp_path / ".jikji" / "autorag_manifest.json").exists()

    rows = _jsonl(tmp_path / ".jikji" / "document_index.jsonl")
    assert rows[0]["file_id"].startswith("sha256:")
    meta = json.loads((tmp_path / rows[0]["doc_meta_path"]).read_text(encoding="utf-8"))
    assert meta["schema_version"] == 1
    assert meta["file_id"] == rows[0]["file_id"]
    card = _jsonl(tmp_path / ".jikji" / "file_cards.jsonl")[0]
    assert "content_terms" in card
    assert "rare_terms" in card
    assert "evidence_previews" in card
    assert "filename_lookup_keys" in card
    chunk = _jsonl(tmp_path / ".jikji" / "chunk_map.jsonl")[0]
    assert chunk["path"] == "보고서.rtf"
    assert "content_terms" in chunk


def test_prepare_skips_sensitive_names_by_default(tmp_path):
    safe = tmp_path / "notes.txt"
    secret = tmp_path / ".env"
    safe.write_text("public", encoding="utf-8")
    secret.write_text("TOKEN=secret", encoding="utf-8")

    build_agent_index(tmp_path, Config(include_hidden=True))

    paths = {row["path"] for row in _jsonl(tmp_path / ".jikji" / "file_index.jsonl")}
    assert "notes.txt" in paths
    assert ".env" not in paths


def test_doctor_json_reports_ok(tmp_path, capsys):
    from jikji.__main__ import main

    (tmp_path / "notes.txt").write_text("hello", encoding="utf-8")
    assert main(["prepare", str(tmp_path), "--json"]) == 0
    capsys.readouterr()

    assert main(["doctor", str(tmp_path), "--json"]) == 0
    out = capsys.readouterr().out
    report = json.loads(out)
    assert report["ok"] is True
    assert report["errors"] == []
    assert report["manifest"]["search_index_schema_version"] == 2


def test_clean_removes_only_jikji_artifacts(tmp_path, capsys):
    from jikji.__main__ import main

    doc = tmp_path / "keep.txt"
    doc.write_text("original file must survive", encoding="utf-8")
    assert main(["prepare", str(tmp_path), "--json"]) == 0
    capsys.readouterr()

    assert (tmp_path / ".jikji").exists()
    assert (tmp_path / "000_JIKJI_AGENT_MAP.md").exists()
    assert main(["clean", str(tmp_path), "--dry-run", "--json"]) == 0
    dry = json.loads(capsys.readouterr().out)
    assert dry["dry_run"] is True
    assert str(tmp_path / ".jikji") in dry["would_remove"]
    assert (tmp_path / ".jikji").exists()
    assert doc.exists()

    assert main(["clean", str(tmp_path), "--json"]) == 0
    cleaned = json.loads(capsys.readouterr().out)
    assert cleaned["ok"] is True
    assert cleaned["preserved_original_files"] is True
    assert not (tmp_path / ".jikji").exists()
    assert not (tmp_path / "000_JIKJI_AGENT_MAP.md").exists()
    assert doc.read_text(encoding="utf-8") == "original file must survive"


def test_clean_refuses_unverified_jikji_dir_without_force(tmp_path, capsys):
    from jikji.__main__ import main

    user_note = tmp_path / ".jikji" / "not-from-jikji.txt"
    user_note.parent.mkdir()
    user_note.write_text("do not remove without force", encoding="utf-8")

    assert main(["clean", str(tmp_path), "--json"]) == 1
    refused = json.loads(capsys.readouterr().out)
    assert refused["reason"] == "missing_manifest"
    assert user_note.exists()


def test_prepare_recovers_stale_lock(tmp_path):
    index_dir = tmp_path / ".jikji"
    index_dir.mkdir()
    (index_dir / ".lock").write_text('{"pid": 99999999, "started_at": "2000-01-01T00:00:00Z"}', encoding="utf-8")
    (tmp_path / "notes.txt").write_text("hello", encoding="utf-8")

    result = build_agent_index(tmp_path, Config())

    assert result.files == 1
    assert not (index_dir / ".lock").exists()


def test_prepare_recovers_old_lock_even_if_pid_exists(tmp_path):
    index_dir = tmp_path / ".jikji"
    index_dir.mkdir()
    (index_dir / ".lock").write_text(
        json.dumps({"pid": os.getpid(), "started_at": "2000-01-01T00:00:00+00:00"}),
        encoding="utf-8",
    )
    (tmp_path / "notes.txt").write_text("hello", encoding="utf-8")

    result = build_agent_index(tmp_path, Config())

    assert result.files == 1
    assert not (index_dir / ".lock").exists()


def test_prepare_recovers_old_empty_lock(tmp_path):
    index_dir = tmp_path / ".jikji"
    index_dir.mkdir()
    lock = index_dir / ".lock"
    lock.write_text("", encoding="utf-8")
    old = 946684800  # 2000-01-01
    os.utime(lock, (old, old))
    (tmp_path / "notes.txt").write_text("hello", encoding="utf-8")

    result = build_agent_index(tmp_path, Config())

    assert result.files == 1
    assert not lock.exists()


def test_eval_generate_and_run_scores_local_search(tmp_path, capsys):
    from jikji.__main__ import main

    docs = tmp_path / "projects" / "apollo"
    docs.mkdir(parents=True)
    target = docs / "mission-notes.txt"
    target.write_text(
        "Apollo lunar telemetry rendezvous checklist uniqueanchor",
        encoding="utf-8",
    )
    (tmp_path / "finance-report.md").write_text("budget forecast margin", encoding="utf-8")

    assert main(["prepare", str(tmp_path), "--json"]) == 0
    capsys.readouterr()
    assert main(["eval-generate", str(tmp_path), "--cases", "20", "--json"]) == 0
    generated = json.loads(capsys.readouterr().out)
    assert generated["cases"] > 0
    assert (tmp_path / ".jikji" / "eval" / "eval_set.jsonl").exists()
    scenarios = set(generated["scenarios"])
    assert "filename_exact" in scenarios
    assert "lexical_content" in scenarios

    assert main(["eval", str(tmp_path), "--json"]) == 0
    report = json.loads(capsys.readouterr().out)
    assert report["metrics"]["cases"] == generated["cases"]
    assert report["metrics"]["hit_at_1"] > 0
    assert (tmp_path / ".jikji" / "eval" / "eval_report.json").exists()

    assert main(["bench-analyze", str(tmp_path), "--top-k", "10", "--json"]) == 0
    analysis = json.loads(capsys.readouterr().out)
    assert analysis["summary"]["cases"] == generated["cases"]
    assert (tmp_path / ".jikji" / "eval" / "bench_analysis.json").exists()

    realistic = tmp_path / "realistic.jsonl"
    assert main([
        "eval-generate-realistic",
        str(tmp_path),
        "--cases",
        "10",
        "--out",
        str(realistic),
        "--json",
    ]) == 0
    realistic_payload = json.loads(capsys.readouterr().out)
    assert realistic_payload["cases"] > 0
    assert realistic.exists()

    holdout = tmp_path / "holdout.jsonl"
    assert main([
        "eval-generate-holdout",
        str(tmp_path),
        "--cases",
        "12",
        "--out",
        str(holdout),
        "--json",
    ]) == 0
    holdout_payload = json.loads(capsys.readouterr().out)
    assert holdout_payload["locked_holdout"] is True
    assert holdout_payload["cases"] > 0
    assert holdout_payload["sha256"]
    assert holdout.exists()
    profile = json.loads(holdout.with_suffix(".profile.json").read_text(encoding="utf-8"))
    assert profile["anti_overfit_protocol"]["do_not_inspect_cases_while_tuning"] is True
    assert profile["generator"] == "jikji-holdout-scorer-blind"

    assert main(["search", str(tmp_path), "uniqueanchor", "--top-k", "3", "--json"]) == 0
    search_report = json.loads(capsys.readouterr().out)
    assert search_report["index_status"] == "ready"
    assert search_report["candidates"]
    assert search_report["candidates"][0]["path"] == "projects/apollo/mission-notes.txt"


def test_search_and_brief_support_japanese_cjk_content(tmp_path, capsys):
    from jikji.__main__ import main

    doc = tmp_path / "travel" / "tokyo-guide.md"
    doc.parent.mkdir()
    doc.write_text(
        "東京観光メモ。浅草寺と上野公園の集合場所を確認するための資料です。",
        encoding="utf-8",
    )
    (tmp_path / "travel" / "kyoto-guide.md").write_text(
        "京都観光メモ。伏見稲荷と嵐山の予定を整理する資料です。",
        encoding="utf-8",
    )

    assert main(["prepare", str(tmp_path), "--json"]) == 0
    capsys.readouterr()

    assert main(["search", str(tmp_path), "浅草寺 集合場所", "--top-k", "3", "--json"]) == 0
    search_report = json.loads(capsys.readouterr().out)
    assert search_report["candidates"]
    assert search_report["candidates"][0]["path"] == "travel/tokyo-guide.md"

    assert main(["brief", str(tmp_path), "浅草寺 集合場所", "--top-k", "3", "--json"]) == 0
    brief = json.loads(capsys.readouterr().out)
    assert brief["schema_version"] == 1
    assert brief["agent_policy"]
    assert brief["commands"]["repeat_ranked_search"].startswith("jikji search ")
    assert brief["candidates"][0]["path"] == "travel/tokyo-guide.md"
    assert "Never move" in " ".join(brief["agent_policy"])


def test_search_supports_later_cjk_phrase_in_long_unspaced_span(tmp_path, capsys):
    from jikji.__main__ import main

    target = tmp_path / "research" / "long-cjk.md"
    target.parent.mkdir()
    long_prefix = "研究計画確認事項" * 12
    target.write_text(
        f"{long_prefix}後半重要語資料保管場所最終確認",
        encoding="utf-8",
    )
    (tmp_path / "research" / "other-cjk.md").write_text(
        "研究計画確認事項" * 14,
        encoding="utf-8",
    )

    assert main(["prepare", str(tmp_path), "--json"]) == 0
    capsys.readouterr()

    assert main(["search", str(tmp_path), "後半重要語", "--top-k", "3", "--json"]) == 0
    report = json.loads(capsys.readouterr().out)
    assert report["candidates"]
    assert report["candidates"][0]["path"] == "research/long-cjk.md"


def test_search_supports_chinese_cjk_content(tmp_path, capsys):
    from jikji.__main__ import main

    doc = tmp_path / "notes" / "beijing.md"
    doc.parent.mkdir()
    doc.write_text("北京会议记录包含预算审批和项目交付时间表。", encoding="utf-8")
    (tmp_path / "notes" / "shanghai.md").write_text("上海会议记录包含场地安排。", encoding="utf-8")

    assert main(["prepare", str(tmp_path), "--json"]) == 0
    capsys.readouterr()

    assert main(["search", str(tmp_path), "预算审批 项目交付", "--top-k", "3", "--json"]) == 0
    report = json.loads(capsys.readouterr().out)
    assert report["candidates"]
    assert report["candidates"][0]["path"] == "notes/beijing.md"


def test_brief_streams_candidate_sidecars_without_read_text_materialization(tmp_path, capsys, monkeypatch):
    from jikji.__main__ import main

    doc = tmp_path / "stream" / "target.md"
    doc.parent.mkdir()
    doc.write_text("streaming unique needle", encoding="utf-8")
    assert main(["prepare", str(tmp_path), "--json"]) == 0
    capsys.readouterr()

    original_read_text = Path.read_text

    def guarded_read_text(self, *args, **kwargs):
        if self.name in {"file_cards.jsonl", "folder_profile.jsonl"}:
            raise AssertionError(f"brief must stream {self.name}, not read_text it wholesale")
        return original_read_text(self, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", guarded_read_text)
    assert main(["brief", str(tmp_path), "streaming unique needle", "--top-k", "3", "--json"]) == 0
    brief = json.loads(capsys.readouterr().out)
    assert brief["candidates"][0]["path"] == "stream/target.md"


def test_edith_ground_truth_paths_are_flattened_and_mapped():
    from jikji.edith import _question_doc_paths, _select_eval_cases

    master = [
        {
            "question_id": "CEO-01",
            "entity": "precistec",
            "filename": "contrats/acquired/precistec_client_severneft_supply_2019.pdf",
            "format": "scanned",
            "language": "fr",
        },
        {
            "question_id": "CEO-01",
            "entity": "precistec",
            "filename": "contrats/acquired/precistec_client_enoceanes_msa_2021.pdf",
            "format": "searchable",
            "language": "fr",
        },
    ]
    answer = {
        "question": "Risk map",
        "ground_truth": {
            "sanctions_risk": ["precistec_client_severneft_supply_2019.pdf"],
            "all_review": ["contrats/acquired/precistec_client_enoceanes_msa_2021.pdf"],
            "trap": {"closed.pdf": "not an expected document because value is explanatory text"},
        },
    }

    paths = _question_doc_paths(answer, master)

    assert paths == [
        "contrats/acquired/precistec_client_enoceanes_msa_2021.pdf",
        "contrats/acquired/precistec_client_severneft_supply_2019.pdf",
    ]

    cases, selected_docs, skipped = _select_eval_cases(
        {"CEO-01": answer, "EMPTY": {"question": "none", "ground_truth": {}}},
        master,
        max_cases=4,
        max_docs=10,
    )
    assert skipped == 1
    assert cases[0]["scenario"] == "edith_enterprise_pdf"
    assert cases[0]["expected_source_paths"] == paths
    assert selected_docs == set(paths)


def test_search_auto_prepares_missing_index(tmp_path, capsys):
    from jikji.__main__ import main

    (tmp_path / "notes.txt").write_text("automatic prepare uniqueanchor", encoding="utf-8")

    assert main(["search", str(tmp_path), "uniqueanchor", "--json"]) == 0
    report = json.loads(capsys.readouterr().out)

    assert report["index_status"] == "prepared_now"
    assert report["foreground_prepared"] is True
    assert (tmp_path / ".jikji" / "search_index.sqlite").exists()
    assert report["candidates"][0]["path"] == "notes.txt"


def test_search_stale_index_starts_background_refresh(tmp_path, capsys, monkeypatch):
    from jikji import __main__ as cli

    calls = []

    class FakePopen:
        def __init__(self, cmd, **kwargs):
            calls.append((cmd, kwargs))
            stdout = kwargs.get("stdout")
            if stdout:
                stdout.close()

    (tmp_path / "notes.txt").write_text("stale index uniqueanchor", encoding="utf-8")
    assert cli.main(["prepare", str(tmp_path), "--json"]) == 0
    capsys.readouterr()
    monkeypatch.setattr(cli.subprocess, "Popen", FakePopen)

    assert cli.main(["search", str(tmp_path), "uniqueanchor", "--stale-after-seconds", "0", "--json"]) == 0
    report = json.loads(capsys.readouterr().out)

    assert report["index_status"] == "stale_using_previous_index"
    assert report["background_refresh_started"] is True
    assert calls
    assert "prepare" in calls[0][0]
    assert report["candidates"][0]["path"] == "notes.txt"


def test_hippocamp_import_and_bench_raw_vs_jikji(tmp_path, capsys):
    from jikji.__main__ import main

    root = tmp_path / "Adam_Subset"
    root.mkdir()
    target = root / "contractnli" / "Tazza-CAFFE-Confidentiality-Agreement.rtf"
    target.parent.mkdir()
    target.write_text(r"{\rtf1\ansi confidential information employee representative exception}", encoding="utf-8")
    other = root / "notes.txt"
    other.write_text("unrelated grocery reminder", encoding="utf-8")
    annotation = tmp_path / "Adam_Subset.annotation.json"
    annotation.write_text(json.dumps([
        {
            "id": "1",
            "file_path": ["contractnli/Tazza-CAFFE-Confidentiality-Agreement.rtf"],
            "question": "Which agreement mentions confidential information and employee representatives?",
            "QA_type": "semantic",
            "evidence": [{"evidence_text": "confidential information employee representative"}],
            "answer": "Tazza Caffe confidentiality agreement",
        }
    ]), encoding="utf-8")

    assert main(["prepare", str(root), "--json"]) == 0
    capsys.readouterr()
    assert main(["hippocamp-import", str(root), "--annotation", str(annotation), "--json"]) == 0
    imported = json.loads(capsys.readouterr().out)
    assert imported["cases"] == 1
    assert not (root / ".jikji" / "eval" / "hippocamp_eval_set.jsonl").exists()
    assert main(["bench-run", str(root), "--eval-set", imported["eval_set"], "--json"]) == 0
    bench = json.loads(capsys.readouterr().out)
    assert set(bench["metrics"]) == {"raw", "jikji"}
    assert not str(bench["report"]).startswith(str(root / ".jikji"))
    assert bench["metrics"]["raw"]["cases"] == 1
    assert bench["metrics"]["jikji"]["hit_at_1"] == 1.0


def test_prepare_emits_semantic_hints_for_agent_search(tmp_path):
    company = tmp_path / "Company" / "4_Manufacturing" / "SEA & ANCHOR PTE. LTD. (201503267M) - Singapore Company.pdf"
    company.parent.mkdir(parents=True)
    company.write_text("Date Incorporation 2015 SSIC manufacturing entity", encoding="utf-8")

    build_agent_index(tmp_path, Config())

    rows = _jsonl(tmp_path / ".jikji" / "file_index.jsonl")
    row = rows[0]
    assert "semantic_hints" in row
    hints = {str(x).casefold() for x in row["semantic_hints"]}
    assert "company" in hints
    assert any("manufacturing" in hint for hint in hints)
    assert "folder_terms" in row


def test_prepare_never_indexes_jikji_workspace_even_when_hidden_included(tmp_path):
    (tmp_path / "notes.txt").write_text("hello", encoding="utf-8")
    build_agent_index(tmp_path, Config(include_hidden=True))
    build_agent_index(tmp_path, Config(include_hidden=True))

    paths = {row["path"] for row in _jsonl(tmp_path / ".jikji" / "file_index.jsonl")}
    assert "notes.txt" in paths
    assert not any(path.startswith(".jikji/") for path in paths)


def test_hermes_bench_rejects_eval_leaks(tmp_path):
    from jikji.hermes_bench import assert_no_leak_root

    root = tmp_path / "root"
    root.mkdir()
    leaked_eval = root / ".jikji" / "eval" / "cases.jsonl"
    leaked_eval.parent.mkdir(parents=True)
    leaked_eval.write_text("{}", encoding="utf-8")

    try:
        assert_no_leak_root(root, leaked_eval)
    except RuntimeError as exc:
        assert "no-leak check failed" in str(exc)
    else:  # pragma: no cover
        raise AssertionError("expected no-leak check failure")


def test_hermes_jikji_prompt_is_agent_brief_first(tmp_path):
    from jikji.hermes_bench import _prompt

    prompt = _prompt(tmp_path, "jikji", {"query": "find notes", "id": "case"}, candidate_top_k=5)

    assert "JIKJI BRIEF MODE" in prompt
    assert "JIKJI AGENT BRIEF" in prompt
    assert "Actual brief payload follows" in prompt
    assert '"schema_version": 1' in prompt
    assert "Route order" in prompt
    assert "preserve relative paths exactly" in prompt


def test_hermes_jikji_fast_prompt_is_map_first_no_browse(tmp_path):
    from jikji.hermes_bench import _mode_family, _prompt

    prompt = _prompt(tmp_path, "map-first", {"query": "find notes", "id": "case"}, candidate_top_k=5)

    assert _mode_family("jikji-direct") == "jikji-direct"
    assert _mode_family("skill-direct") == "jikji-direct"
    assert _mode_family("map-first") == "jikji-fast"
    assert _mode_family("jikji-pass-through") == "jikji-fast"
    assert "JIKJI MAP-FIRST FAST PATH" in prompt
    assert "Do not browse, list, grep, cat, or inspect any filesystem path." in prompt
    assert "copy every candidate path into the JSON paths array exactly in the same order" in prompt
    assert "Actual brief payload follows" not in prompt
    assert "Do not invent, summarize, or replace candidates" in prompt


def test_instant_search_index_includes_cached_document_text(tmp_path):
    import sqlite3

    from jikji.search_index import INSTANT_SEARCH_SCHEMA_VERSION, build_instant_search_index

    index_dir = tmp_path / ".jikji"
    cache = index_dir / "doc_text" / "sha256_demo.txt"
    cache.parent.mkdir(parents=True)
    cache.write_text("문서 본문 고유단서테스트 2026-06-03", encoding="utf-8")
    card = {
        "path": "docs/demo.pdf",
        "name": "demo.pdf",
        "ext": ".pdf",
        "sha256": "demo",
        "text_cache_path": ".jikji/doc_text/sha256_demo.txt",
        "evidence_previews": [],
    }

    db = build_instant_search_index(index_dir, [card], [])

    con = sqlite3.connect(db)
    try:
        schema = con.execute("SELECT value FROM meta WHERE key='schema_version'").fetchone()[0]
        assert int(schema) == INSTANT_SEARCH_SCHEMA_VERSION == 2
        source = con.execute("SELECT row_json FROM docs").fetchone()[0]
        assert "고유단서테스트" in source
        terms = {row[0] for row in con.execute("SELECT term FROM terms")}
        assert "고유단서테스트" in terms
    finally:
        con.close()


def test_bench_run_rejects_annotation_leak(tmp_path, capsys):
    from jikji.__main__ import main

    root = tmp_path / "Adam_Subset"
    root.mkdir()
    (root / "target.txt").write_text("answer", encoding="utf-8")
    (root / "Adam_Subset.json").write_text("[]", encoding="utf-8")
    eval_set = tmp_path / "eval.jsonl"
    eval_set.write_text(json.dumps({
        "id": "case",
        "scenario": "leak",
        "query": "answer",
        "expected_paths": ["target.txt"],
    }) + "\n", encoding="utf-8")

    assert main(["prepare", str(root), "--json"]) == 0
    capsys.readouterr()
    try:
        main(["bench-run", str(root), "--eval-set", str(eval_set), "--json"])
    except RuntimeError as exc:
        assert "no-leak check failed" in str(exc)
    else:  # pragma: no cover
        raise AssertionError("expected bench-run leak rejection")


def test_hermes_skill_install_to_explicit_dest(tmp_path):
    from jikji.hermes_bench import install_hermes_skill

    dest = tmp_path / "skills" / "jikji" / "SKILL.md"
    result = install_hermes_skill(dest=dest)

    assert result.installed is True
    assert dest.exists()
    assert "Jikji" in dest.read_text(encoding="utf-8")


def test_agent_skill_install_to_explicit_dest(tmp_path):
    from jikji.agent_skill_install import install_agent_skill

    dest = tmp_path / "codex" / "jikji" / "SKILL.md"
    result = install_agent_skill("codex", dest=dest)

    assert result.agent == "codex"
    assert result.installed is True
    assert dest.exists()
    text = dest.read_text(encoding="utf-8")
    assert "selected automatically" in text
    assert "jikji brief" in text


def test_agent_skill_install_cli_alias_to_explicit_dest(tmp_path, capsys):
    from jikji.__main__ import main

    dest = tmp_path / "claude" / "jikji" / "SKILL.md"

    assert main(["claude-skill-install", "--dest", str(dest), "--no-prepare", "--json"]) == 0
    payload = json.loads(capsys.readouterr().out)

    assert payload["installed_any"] is True
    assert payload["results"][0]["agent"] == "claude"
    assert dest.exists()


def test_agent_skill_install_dest_only_treats_unknown_agent_as_custom(tmp_path, capsys):
    from jikji.__main__ import main

    dest = tmp_path / "unknown-agent" / "skills" / "jikji" / "SKILL.md"

    assert main(["agent-skill-install", "--dest", str(dest), "--no-prepare", "--json"]) == 0
    payload = json.loads(capsys.readouterr().out)

    assert payload["installed_any"] is True
    assert payload["results"][0]["agent"] == "custom"
    assert dest.exists()
    assert "any coding" in dest.read_text(encoding="utf-8")


def test_skill_export_prints_and_writes_universal_skill(tmp_path, capsys):
    from jikji.__main__ import main

    assert main(["skill-export"]) == 0
    printed = capsys.readouterr().out
    assert "Jikji Local File Discovery Skill" in printed
    assert "any coding" in printed

    dest = tmp_path / "agent" / "SKILL.md"
    assert main(["skill-export", "--dest", str(dest), "--no-prepare", "--json"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["installed"] is True
    assert dest.exists()


def test_agent_skill_install_queues_background_prepare_for_explicit_root(tmp_path, capsys, monkeypatch):
    from jikji import __main__ as cli

    calls = []

    class FakePopen:
        pid = 12345

        def __init__(self, cmd, **kwargs):
            calls.append((cmd, kwargs))
            stdout = kwargs.get("stdout")
            if stdout:
                stdout.close()

    root = tmp_path / "Documents"
    root.mkdir()
    (root / "brief.txt").write_text("post install prepare target", encoding="utf-8")
    dest = tmp_path / "agent" / "SKILL.md"
    monkeypatch.setattr(cli.subprocess, "Popen", FakePopen)

    assert cli.main([
        "agent-skill-install",
        "--dest",
        str(dest),
        "--prepare-root",
        str(root),
        "--json",
    ]) == 0
    payload = json.loads(capsys.readouterr().out)

    assert payload["post_install_prepare"]["mode"] == "background"
    assert payload["post_install_prepare"]["started"] is True
    assert payload["post_install_prepare"]["roots"][0]["root"] == str(root.resolve())
    assert calls
    assert "post-install-prepare" in calls[0][0]


def test_agent_skill_install_foreground_prepare_for_explicit_root(tmp_path, capsys):
    from jikji.__main__ import main

    root = tmp_path / "Downloads"
    root.mkdir()
    (root / "download-note.txt").write_text("foreground prepare marker", encoding="utf-8")
    dest = tmp_path / "agent" / "SKILL.md"

    assert main([
        "agent-skill-install",
        "--dest",
        str(dest),
        "--prepare-root",
        str(root),
        "--foreground-prepare",
        "--json",
    ]) == 0
    payload = json.loads(capsys.readouterr().out)

    assert payload["post_install_prepare"]["mode"] == "foreground"
    assert payload["post_install_prepare"]["roots"][0]["ok"] is True
    assert (root / ".jikji" / "search_index.sqlite").exists()


def test_bench_iterate_records_requested_iterations(tmp_path, capsys):
    from jikji.__main__ import main

    root = tmp_path / "corpus"
    root.mkdir()
    target = root / "Company" / "4_Manufacturing" / "SEA ANCHOR Singapore Company.txt"
    target.parent.mkdir(parents=True)
    target.write_text("manufacturing company Date Incorporation after 2015", encoding="utf-8")
    (root / "notes.txt").write_text("unrelated", encoding="utf-8")

    assert main(["prepare", str(root), "--json"]) == 0
    capsys.readouterr()
    eval_set = tmp_path / "external_eval.jsonl"
    eval_set.write_text(json.dumps({
        "id": "case-1",
        "scenario": "company",
        "query": "find the manufacturing company incorporation record",
        "expected_paths": ["Company/4_Manufacturing/SEA ANCHOR Singapore Company.txt"],
    }) + "\n", encoding="utf-8")

    out = tmp_path / "loop.json"
    assert main([
        "bench-iterate",
        str(root),
        "--eval-set",
        str(eval_set),
        "--iterations",
        "3",
        "--out",
        str(out),
        "--json",
    ]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["iterations"] == 3
    report = json.loads(out.read_text(encoding="utf-8"))
    assert report["completed_iterations"] == 3


def test_prepare_searches_eml_ics_sqlite_and_epub_body_text(tmp_path, capsys):
    import sqlite3
    import zipfile

    from jikji.__main__ import main

    eml = tmp_path / "mail.eml"
    eml.write_text(
        "Subject: Alpha Project Handoff\n"
        "From: sender@example.com\n"
        "To: receiver@example.com\n"
        "Content-Type: text/plain; charset=utf-8\n\n"
        "The launch code marker is emailtoken-7742.",
        encoding="utf-8",
    )

    ics = tmp_path / "calendar.ics"
    ics.write_text(
        "BEGIN:VCALENDAR\n"
        "BEGIN:VEVENT\n"
        "SUMMARY:Design sync uniquecalendar991\n"
        "DTSTART:20260526T090000Z\n"
        "LOCATION:Seoul lab\n"
        "DESCRIPTION:Calendar body marker\n"
        "END:VEVENT\n"
        "END:VCALENDAR\n",
        encoding="utf-8",
    )

    db = tmp_path / "notes.sqlite"
    con = sqlite3.connect(db)
    con.execute("CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, body TEXT)")
    con.execute("INSERT INTO notes (title, body) VALUES (?, ?)", ("Research", "sqlitebodytoken-3301 inside row"))
    con.commit()
    con.close()

    epub = tmp_path / "book.epub"
    with zipfile.ZipFile(epub, "w") as zf:
        zf.writestr("mimetype", "application/epub+zip")
        zf.writestr("OEBPS/chapter1.xhtml", "<html><body><h1>Chapter</h1><p>epubtoken-8802 appears here.</p></body></html>")

    assert main(["prepare", str(tmp_path), "--json"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["docs_parsed"] >= 4

    expected = {
        "emailtoken-7742": "mail.eml",
        "uniquecalendar991": "calendar.ics",
        "sqlitebodytoken-3301": "notes.sqlite",
        "epubtoken-8802": "book.epub",
    }
    for query, path in expected.items():
        assert main(["search", str(tmp_path), query, "--top-k", "1", "--json"]) == 0
        report = json.loads(capsys.readouterr().out)
        assert report["candidates"][0]["path"] == path

    rows = _jsonl(tmp_path / ".jikji" / "document_index.jsonl")
    by_path = {row["path"]: row for row in rows}
    for path in expected.values():
        assert by_path[path]["parser_required"] is True
        assert by_path[path]["parse_status"] == "success"
        assert (tmp_path / by_path[path]["text_cache_path"]).exists()


def test_archive_member_names_are_cached_and_searchable(tmp_path, capsys):
    import zipfile

    from jikji.__main__ import main

    archive = tmp_path / "bundle.zip"
    with zipfile.ZipFile(archive, "w") as zf:
        zf.writestr("nested/archive_lookup_marker_9123.txt", "body not extracted")

    assert main(["prepare", str(tmp_path), "--json"]) == 0
    capsys.readouterr()
    rows = _jsonl(tmp_path / ".jikji" / "document_index.jsonl")
    row = next(row for row in rows if row["path"] == "bundle.zip")
    assert row["parse_status"] == "archive_listing"
    assert "archive_lookup_marker_9123" in (tmp_path / row["text_cache_path"]).read_text(encoding="utf-8")

    assert main(["search", str(tmp_path), "archive_lookup_marker_9123", "--top-k", "1", "--json"]) == 0
    report = json.loads(capsys.readouterr().out)
    assert report["candidates"][0]["path"] == "bundle.zip"


def test_optional_media_parsers_do_not_require_external_tools(tmp_path):
    from jikji.parsers.media import parse_audio, parse_image

    png = tmp_path / "empty.png"
    png.write_bytes(b"not a real png")
    wav = tmp_path / "empty.wav"
    wav.write_bytes(b"not a real wav")

    assert isinstance(parse_image(png, 1000), str)
    assert isinstance(parse_audio(wav, 1000), str)


def test_structured_parser_recall_edge_cases(tmp_path):
    import sqlite3
    import zipfile

    from jikji.parsers.archive import _zip_member_name
    from jikji.parsers.structured import parse_eml, parse_ics, parse_sqlite

    eml = tmp_path / "multipart.eml"
    eml.write_text(
        "MIME-Version: 1.0\n"
        "Subject: Multipart\n"
        "Content-Type: multipart/alternative; boundary=abc\n\n"
        "--abc\nContent-Type: text/plain; charset=utf-8\n\nplainonlytoken\n"
        "--abc\nContent-Type: text/html; charset=utf-8\n\n<html><body>htmlonlytoken</body></html>\n"
        "--abc--\n",
        encoding="utf-8",
    )
    eml_text = parse_eml(eml, 4000)
    assert "plainonlytoken" in eml_text
    assert "htmlonlytoken" in eml_text

    ics = tmp_path / "links.ics"
    ics.write_text(
        "BEGIN:VCALENDAR\n"
        "X-WR-CALNAME:Roadmap calendar\n"
        "BEGIN:VEVENT\n"
        "SUMMARY:Link meeting\n"
        "URL:https://example.test/meeting-url-token\n"
        "COMMENT:commenttoken-ics\n"
        "END:VEVENT\n"
        "END:VCALENDAR\n",
        encoding="utf-8",
    )
    ics_text = parse_ics(ics, 4000)
    assert "meeting-url-token" in ics_text
    assert "commenttoken-ics" in ics_text
    assert "Roadmap calendar" in ics_text

    db = tmp_path / "loose.db"
    con = sqlite3.connect(db)
    con.execute("CREATE TABLE items (id INTEGER, note STRING, payload BLOB)")
    con.execute("INSERT INTO items VALUES (?, ?, ?)", (7, "stringaffinitytoken", b"blobtoken"))
    con.commit()
    con.close()
    db_text = parse_sqlite(db, 4000)
    assert "stringaffinitytoken" in db_text
    assert "blobtoken" not in db_text

    info = zipfile.ZipInfo("회의록.hwp".encode("cp949").decode("cp437"))
    info.flag_bits = 0
    assert _zip_member_name(info) == "회의록.hwp"


def test_image_ocr_uses_absolute_path_for_dash_prefixed_names(tmp_path, monkeypatch):
    from jikji.parsers.media import parse_image

    calls = tmp_path / "calls.txt"
    fake = tmp_path / "tesseract"
    fake.write_text(
        "#!/bin/sh\n"
        "printf '%s\n' \"$1\" > \"$JIKJI_FAKE_TESS_CALLS\"\n"
        "printf 'dash OCR token'\n",
        encoding="utf-8",
    )
    fake.chmod(0o755)
    monkeypatch.setenv("PATH", f"{tmp_path}:{os.environ.get('PATH', '')}")
    monkeypatch.setenv("JIKJI_FAKE_TESS_CALLS", str(calls))
    image = tmp_path / "-scan.png"
    image.write_bytes(b"not a real image; fake tesseract ignores it")

    parsed = parse_image(image, 1000)

    assert "dash OCR token" in parsed
    first_arg = calls.read_text(encoding="utf-8").strip()
    assert first_arg == str(image.resolve())
    assert not first_arg.startswith("-")


def test_edith_suite_no_docs_is_metadata_only(tmp_path, monkeypatch):
    from jikji import edith

    metadata = tmp_path / "metadata"
    metadata.mkdir()
    (metadata / "MASTER_INDEX.csv").write_text(
        "filename,format,language\nEntity/doc.pdf,searchable,en\n",
        encoding="utf-8",
    )
    (metadata / "ANSWER_KEY.json").write_text(
        json.dumps({
            "q1": {
                "question": "Which enterprise document mentions the target policy?",
                "ground_truth": ["doc.pdf"],
            }
        }),
        encoding="utf-8",
    )

    monkeypatch.setattr(edith, "fetch_edith_metadata", lambda dest: metadata)

    def fail_benchmark(*args, **kwargs):  # noqa: ARG001
        raise AssertionError("metadata-only EDiTh suite must not run corpus benchmark")

    monkeypatch.setattr(edith, "run_benchmark", fail_benchmark)

    result = edith.run_edith_suite(
        tmp_path / "bench",
        max_cases=1,
        max_docs=1,
        download_docs=False,
    )

    assert result.metrics["metadata_only"]["cases"] == 1
    assert result.materialized.extracted_docs == 0
    assert not result.materialized.corpus_root.exists()
    report = json.loads(result.report_path.read_text(encoding="utf-8"))
    assert report["benchmark_report"] is None
    assert report["metrics"]["metadata_only"]["download_docs"] is False


def test_edith_stream_extraction_respects_byte_budget(tmp_path, monkeypatch):
    import io
    import tarfile

    import pytest

    from jikji import edith

    tar_bytes = io.BytesIO()
    payload = b"%PDF-1.4\nselected benchmark fixture\n"
    with tarfile.open(fileobj=tar_bytes, mode="w:gz") as archive:
        info = tarfile.TarInfo("by_entity/Entity/doc.pdf")
        info.size = len(payload)
        archive.addfile(info, io.BytesIO(payload))
    archive_blob = tar_bytes.getvalue()

    monkeypatch.setattr(edith.urllib.request, "urlopen", lambda *args, **kwargs: io.BytesIO(archive_blob))

    with pytest.raises(edith.EdithDownloadLimitExceeded):
        edith._stream_extract_selected_docs(
            tmp_path / "capped",
            {"Entity/doc.pdf"},
            max_download_bytes=1,
        )

    extracted = edith._stream_extract_selected_docs(
        tmp_path / "ok",
        {"Entity/doc.pdf"},
        max_download_bytes=len(archive_blob) + 1024,
    )

    assert extracted.found == {"Entity/doc.pdf": "Entity/doc.pdf"}
    assert extracted.byte_limit == len(archive_blob) + 1024
    assert extracted.bytes_read <= extracted.byte_limit
    assert (tmp_path / "ok" / "Entity" / "doc.pdf").read_bytes() == payload


def test_publicdata_case_generation_uses_messy_paths_and_content_clues(tmp_path):
    from jikji.publicdata_bench import _case_templates, _messy_relpath, _rare_terms

    docs = [
        {
            "title": "서울시 공원 이용 현황",
            "description": "공원별 방문객과 시설 정보를 제공하는 데이터",
            "bench_path": "test/받은자료/기관별/새 폴더/001_서울시_공원_이용_현황_원본.xlsx",
            "xlsx_text": ["공원명", "방문객수", "희귀단서공원A"],
            "source_url": "https://example.test/a",
        },
        {
            "title": "서울시 도서관 대출 통계",
            "description": "도서관별 대출 권수와 운영 정보를 제공하는 데이터",
            "bench_path": "test/공공데이터/임시보관/확인필요/002_서울시_도서관_대출_통계_검토용.xlsx",
            "xlsx_text": ["도서관명", "대출권수", "희귀단서도서관B"],
            "source_url": "https://example.test/b",
        },
    ]

    rel = _messy_relpath({"title": "서울시 테스트 데이터"}, "test", 1, __import__("random").Random(7))
    assert rel.startswith("test/")
    assert rel.endswith(".xlsx")
    assert "서울시_테스트_데이터" in rel

    rare = _rare_terms(docs)
    assert "희귀단서공원A" in rare[docs[0]["bench_path"]]

    cases = _case_templates(docs, max_cases=2)
    assert len(cases) == 2
    assert cases[0]["expected_paths"] == [docs[0]["bench_path"]]
    assert cases[0]["scenario"] == "filename_vague"
    assert cases[1]["scenario"] == "content_lexical"
    assert "희귀단서도서관B" in cases[1]["query"]


def test_workspacebench_eval_case_uses_file_dependencies():
    from jikji.workspacebench import build_eval_case

    metadata = {
        "absolute_id": 107,
        "persona": "Operations Manager",
        "task": "Create the strategy report from regional order files.",
        "task_diff": "hard",
        "output_files": ["Global_Product_Strategy.md"],
        "tested_capabilities": ["Workspace Exploration"],
        "file_dep_graph": [
            {"from": "USCA_orders.csv", "to": "Global_Product_Strategy.md"},
            {"from": "product_info.csv", "to": "Global_Product_Strategy.md"},
        ],
        "data_manifest": [
            {"filename": "USCA_orders.csv", "stored_relpath": "data/a_USCA_orders.csv"},
            {"filename": "product_info.csv", "stored_relpath": "data/b_product_info.csv"},
            {"filename": "unused_notes.txt", "stored_relpath": "data/c_unused_notes.txt"},
        ],
    }

    case = build_eval_case("task_107", metadata)

    assert case["id"] == "workspacebench-107"
    assert case["scenario"] == "workspace_task_supporting_files"
    assert "Operations Manager" in case["query"]
    assert "Create the strategy report" in case["query"]
    assert case["expected_paths"] == [
        "task_107/data/a_USCA_orders.csv",
        "task_107/data/b_product_info.csv",
    ]
    assert case["expected_count"] == 2


def test_workspacebench_eval_case_falls_back_to_manifest_when_graph_absent():
    from jikji.workspacebench import build_eval_case

    metadata = {
        "absolute_id": 100,
        "persona": "Logistics Manager",
        "task": "Integrate four host scripts.",
        "data_manifest": [
            {"filename": "host_script_1.docx", "stored_relpath": "data/a_host_script_1.docx"},
            {"filename": "host_script_2.docx", "stored_relpath": "data/b_host_script_2.docx"},
        ],
    }

    case = build_eval_case("task_100", metadata)

    assert case["expected_paths"] == [
        "task_100/data/a_host_script_1.docx",
        "task_100/data/b_host_script_2.docx",
    ]


def test_workspacebench_eval_case_rejects_unsafe_manifest_paths():
    import pytest

    from jikji.workspacebench import build_eval_case

    metadata = {
        "absolute_id": 101,
        "task": "Use the attached file.",
        "data_manifest": [
            {"filename": "escape.txt", "stored_relpath": "../escape.txt"},
        ],
    }

    with pytest.raises(ValueError, match="unsafe Workspace-Bench path"):
        build_eval_case("task_101", metadata)


def test_hardbench_token_filter_rejects_parser_noise():
    from jikji.hardbench import _tokens

    text = "상세정보 츀츥츥츥 겭삹겳 theusercanfreelyusethepublicworkregardlessofitscommercialusewithoutfee 계약서"

    tokens = _tokens(text, min_len=2)

    assert "상세정보" in tokens
    assert "계약서" in tokens
    assert "츀츥츥츥" not in tokens
    assert "겭삹겳" not in tokens
    assert "theusercanfreelyusethepublicworkregardlessofitscommercialusewithoutfee" not in tokens


def test_hardbench_doc_type_label_is_natural_korean():
    from jikji.hardbench import _doc_type_label

    assert _doc_type_label("training") == "교육·워크숍 발표자료"
    assert _doc_type_label("manual") == "지침·매뉴얼·해설서"
    assert _doc_type_label("unknown") == "참고자료"


def test_hardbench_extreme_masks_filename_and_builds_decoy_queries():
    import random

    from jikji.hardbench import _case_templates, _messy_relpath

    doc = {
        "filename": "정확한_원래_파일명_상담사례집.pdf",
        "page_title": "원래 제목 상담사례집",
        "ext": ".pdf",
        "doc_type": "casebook",
        "text_excerpt": "본문 고유 문장입니다. 교육부2019년도우수 담당자 사례 발표 내용이 포함됩니다.",
    }
    rel = _messy_relpath(doc, "test", 1, random.Random(7), difficulty="extreme")

    assert "정확한_원래_파일명" not in rel
    assert rel.endswith(".pdf")
    assert any(generic in rel for generic in ("붙임_", "참고_", "검토본_", "회의자료_", "원본_", "최종본_"))

    row = dict(doc)
    row["bench_path"] = rel
    cases = _case_templates([row], max_cases=4, seed=11, difficulty="extreme")
    queries = "\n".join(case["query"] for case in cases)

    assert cases
    assert any(needle in queries for needle in ("txt 메모나 링크 파일은 제외", "메모 파일도 같이 걸릴 수", "메모/링크", "후보목록"))
    assert "정확한_원래_파일명" not in queries
    assert {case["scenario"] for case in cases}.issubset({
        "body_phrase_no_filename",
        "decoy_note_resistant",
        "weak_folder_memory",
        "multi_body_disambiguation",
    })


def test_hardbench_local_source_sampler_balances_extensions(tmp_path):
    from jikji.hardbench import _local_source_docs

    source = tmp_path / "source"
    (source / "a").mkdir(parents=True)
    (source / "b").mkdir(parents=True)
    for idx in range(4):
        (source / "a" / f"doc{idx}.pdf").write_bytes(b"%PDF-1.4\n" + b"a" * 2048)
    for idx in range(3):
        (source / "b" / f"doc{idx}.hwp").write_bytes(b"\xd0\xcf\x11\xe0" + b"b" * 2048)
    (source / "b" / "sheet.xlsx").write_bytes(b"PK\x03\x04" + b"c" * 2048)

    docs = _local_source_docs(
        source,
        target_docs=5,
        seed=123,
        max_file_bytes=10_000,
        max_total_bytes=0,
    )

    exts = {doc["ext"] for doc in docs}
    assert len(docs) == 5
    assert {".pdf", ".hwp", ".xlsx"}.issubset(exts)
    assert all(doc["source_file"] for doc in docs)
