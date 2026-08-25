# Jikji 로컬 지식베이스 관리 GUI 계획

> 상태: 설계·구현 계획. 이 문서는 `request failed` 문제를 출발점으로, 사용자의 로컬 에이전트가 사용할 개인 지식베이스를 관리하는 제품 계약을 고정한다. 이번 문서 작성 단계에서는 기존 동작을 변경하지 않는다.

## 1. 제품 경계와 현재 구성

### 1.1 사용자 관점

Jikji는 파일을 이동·삭제·개명하지 않고, 사용자가 지정한 로컬 root를 에이전트가 검색할 수 있는 지식베이스로 준비한다.

- Rust binary: `crates/jikji-cli/src/main.rs` → `jikji` 실행 진입점
- Rust prepare/index: `crates/jikji-index/src/{artifacts.rs,scan.rs,doc_cache.rs,doc_media.rs}`
- Rust search/discover: `crates/jikji-search/src/{searcher.rs,discover.rs,brief.rs,graph.rs}`
- 중앙 저장소: `crates/jikji-core/src/storage.rs` → `JIKJI_DATA_DIR/jikji/index.sqlite`
- Rust GUI server: `crates/jikji-cli/src/gui_commands.rs`
- GUI HTTP routing: `crates/jikji-cli/src/gui_commands/{routing.rs,http.rs}`
- MarkerAI proxy: `/home/cheol/projects/github_issue_solver/app/main.py`의 `JIKJI_UPSTREAM`, `/jikji`, `/jikji/{path}`
- Python tree: `python/jikji/`는 CI/parity/golden/reference 용도이며 Rust GUI/index/search 실행 경로에서 호출하지 않는다.

### 1.2 현재 GUI 표면

현재 Rust GUI가 제공하는 주요 경로:

- `GET /` 또는 `/index.html`: embedded HTML SPA
- `GET /api/status`, `/api/root-status`: active root 및 중앙 DB health
- `GET /api/roots`: 중앙 DB indexed roots/통계
- `GET /api/files?path=...`: root-bound explorer entries
- `GET /api/find?q=...`, `GET /api/search?q=...`: Rust discovery/search
- `GET /api/preview?path=...&q=...`: root-bound preview 및 match ranges
- `POST /api/root`: active root 전환/추가
- `POST /api/refresh`: 현재 root refresh
- `POST /api/reindex`: 현재 root foreground reindex
- `POST /api/deep-index`: 현재 root media/archive detailed indexing
- `POST /api/remove-root`, `DELETE /api/remove-root`: 중앙 DB root/cache 삭제, source 보존
- `POST /open`, `POST /reveal`: 관리 token 보호 local opener

Mutation route는 Rust `ManagementToken`과 mutation mutex를 통과해야 한다. 외부 공개 bind는 금지하고 loopback upstream + 인증된 내부 proxy만 사용한다.

## 2. `request failed` 조사와 재현 계약

### 2.1 요청 경로

```text
Browser
  → http://100.90.206.112:8787/jikji[/api/...]
  → MarkerAI Python proxy: /home/cheol/projects/github_issue_solver/app/main.py
  → http://127.0.0.1:18768[/api/...]
  → Rust GUI router
  → central SQLite / indexed root filesystem
```

### 2.2 원인 후보 우선순위

1. **Upstream lifecycle**: `127.0.0.1:18768`가 죽었거나 준비 전이다. Proxy는 502를 반환해야 한다.
2. **Wrong root**: GUI가 빈 `/tmp` 또는 index 없는 root로 실행되어 `/api/status.prepared=false`, search가 실패한다.
3. **Path prefix rewrite**: `/jikji` HTML의 root-relative `fetch`, `href`, `src`, `action`이 `/jikji/`로 rewrite되지 않으면 브라우저가 Markr root의 API를 호출한다.
4. **Proxy method/body/query loss**: POST mutation의 method, query, content-type, body 또는 upstream response content-type이 손실된다.
5. **Authentication**: Rust mutation token이 없거나 stale token이다. expected result는 403이며 generic `request failed`가 아니라 상태 코드/본문을 보여줘야 한다.
6. **Timeout**: 대형 root `find` 또는 reindex가 browser request timeout을 넘긴다. UI는 request id/status polling으로 전환해야 한다.
7. **CORS/cookie boundary**: proxy same-origin route를 사용하지 않고 upstream URL을 직접 호출하면 CORS/loopback 접근 문제가 발생한다.

### 2.3 재현 절차와 관찰 항목

```bash
systemctl --user is-active github-issue-solver
ss -ltnp | grep ':18768'
curl -i http://100.90.206.112:8787/jikji
curl -i http://100.90.206.112:8787/jikji/api/status
curl -i 'http://100.90.206.112:8787/jikji/api/find?q=Rust'
curl -i 'http://100.90.206.112:8787/jikji/api/preview?path=README.md&q=Rust'
```

브라우저 자동화는 다음을 반드시 기록한다.

- request URL/method/status/duration
- response content-type와 첫 JSON error fields
- browser console error 및 failed request URL
- upstream Rust log와 Markr proxy log의 correlation id
- `status.prepared`, `status.root`, `status.artifacts.database`, `roots[].statistics`

### 2.4 표준 오류 형식

모든 proxy/Rust API 오류는 다음 형태를 목표로 한다.

```json
{
  "error": {
    "code": "UPSTREAM_UNAVAILABLE|AUTH_REQUIRED|ROOT_NOT_INDEXED|PATH_OUTSIDE_ROOT|INDEX_FAILED|PREVIEW_UNAVAILABLE|TIMEOUT",
    "message": "사용자가 이해할 수 있는 설명",
    "request_id": "req_...",
    "retryable": true,
    "details": {"upstream_status": 502}
  }
}
```

현재 plain `{"error":"..."}` 응답을 위 envelope로 통일하는 것은 구현 backlog다. 기존 클라이언트 호환을 위해 migration 기간에는 `error` string도 읽을 수 있어야 한다.

## 3. 사용자 시나리오

### S1. 최초 온보딩

**Given** Jikji가 설치됐고 indexed root가 없다.  
**When** 사용자가 GUI를 연다.  
**Then** root picker, “폴더 추가”, empty state, `prepare` 안내가 보인다.

상태: `NO_ROOT → ROOT_SELECTED → INDEXING → READY|FAILED`.

### S2. 기존 root 선택 및 health 확인

- root 목록에서 root를 선택한다.
- indexed files/folders/documents/chunks, last successful index, stale age, DB path, parser failures를 표시한다.
- `checking`은 최초 request 중에만 허용하고, 성공/실패/timeout 중 하나로 종료한다.

### S3. 파일 탐색

- 좌측 explorer에서 folder를 확장한다.
- path/name/size/mtime/type/status를 표시한다.
- directory navigation은 `/api/files?path=`로만 수행하고 canonical root 밖으로 이동하지 않는다.

### S4. 검색과 본문 미리보기

- 상단 search box는 `/api/find?q=`에 연결된다.
- result row는 path/name/score/evidence/parse status를 보여준다.
- result 선택 시 `/api/preview?path=&q=`를 호출한다.
- `matches[]`의 UTF-16 offsets로 `<mark>` highlight를 만들고 서버 문자열은 DOM `textContent`로 삽입한다.
- binary/unsupported는 “미리보기 불가”와 metadata를 표시한다.
- no result는 empty state, request error는 error state로 구분한다.

### S5. 전체 refresh

- 사용자는 refresh를 누른다.
- 작은 root는 foreground 결과를 기다리고, 큰 root는 job id를 받아 progress/status polling을 사용한다.
- 기존 index는 refresh 실패 시 보존한다.

### S6. 선택 root reindex

- root selector에서 folder를 선택하거나 add root dialog로 명시한다.
- reindex는 source files를 변경하지 않는다.
- 완료 후 statistics와 last indexed를 갱신한다.

### S7. root 제거

- 확인 dialog에서 “중앙 index/cache만 삭제, 원본 파일은 삭제하지 않음”을 명시한다.
- 성공 후 root 목록에서 제거하고 active root가 없으면 onboarding empty state로 간다.
- path traversal/other root 삭제를 차단한다.

### S8. 상세 media/archive indexing

- 사용자가 root별 “Deep index”를 명시적으로 켠다.
- media OCR/ASR engine 설정, archive limits, 예상 자원 비용을 보여준다.
- entry count/bytes/time limit을 표시한다.
- 완료 후 `deep_index.state=completed`; 실패 시 partial result와 retry action을 표시한다.
- 기본 prepare에서는 media/archive body가 없고 filename/metadata만 검색되어야 한다.

### S9. 자동 갱신

- 기본 stale TTL은 24시간, 설정 가능하다.
- stale search는 기존 결과를 먼저 반환하고 background refresh를 시작한다.
- 응답에는 `index_status`, `background_refresh_started`, `empty_result_reindexed`를 포함한다.
- refresh lock 중복 실행을 차단한다.

### S10. 여러 에이전트/프로젝트

- 한 중앙 DB에 여러 canonical root가 격리되어 저장된다.
- active root 전환은 UI state와 API state를 일치시킨다.
- Hermes/Codex/Claude/OpenCode용 skill은 모두 `jikji find ROOT "query" --json`을 first action으로 요구한다.

### S11. 실패/복구

- DB missing: one Jikji retry → verified failure 후 bounded raw fallback
- 403: token 입력/재시도 안내
- 502: upstream 재시작 안내와 retry button
- timeout: job status로 이동, 중복 요청 금지
- malformed preview/binary: safe fallback

## 4. 화면·상태 명세

### 화면 구조

- `header`: Jikji branding, active root selector, search input, manage token state
- `health cards`: Indexed roots, files, documents, chunks, last indexed, health
- `left pane`: explorer tree/list
- `center pane`: find result list, result count/confidence/evidence
- `right pane`: preview metadata/content/highlights/download/reveal
- `footer controls`: refresh, reindex, deep-index, remove root
- `dialog`: add root, confirm destructive-index metadata action, token/error details

### 상태 전이

```text
INITIAL
 → LOADING_STATUS
 → READY | EMPTY_NO_ROOT | ERROR_RETRYABLE | ERROR_AUTH | ERROR_FATAL
READY
 → SEARCHING → RESULTS | EMPTY_RESULTS | SEARCH_ERROR
READY
 → REFRESHING → READY | INDEX_ERROR
READY
 → PREVIEWING → PREVIEW_READY | PREVIEW_UNAVAILABLE
READY
 → REMOVING → EMPTY_NO_ROOT | REMOVE_ERROR
```

각 async operation은 `idle/loading/success/empty/error`의 명시 state를 가지며 “checking”을 terminal state로 사용하지 않는다.

## 5. API 계약

### Root/health

```http
GET /api/status
GET /api/roots
POST /api/root?path=/abs/root&token=...
```

성공 payload 핵심:

```json
{
  "root": "/abs/root",
  "prepared": true,
  "storage": "central_sqlite",
  "statistics": {"files": 100, "folders": 12, "documents": 80, "chunks": 300},
  "last_indexed_at": 1780000000,
  "health": "ready"
}
```

### Search/preview

```http
GET /api/find?q=contract&top_k=20
GET /api/preview?path=docs/contract.pdf&q=contract
```

Preview response:

```json
{
  "path": "docs/contract.pdf",
  "content": "...contract...",
  "matches": [{"start": 10, "end": 18}],
  "match_unit": "utf16",
  "supported": true
}
```

### Management

```http
POST /api/refresh?token=...
POST /api/reindex?path=/abs/root&token=...
POST /api/deep-index?path=/abs/root&token=...
DELETE /api/remove-root?path=/abs/root&token=...
```

Mutation API는 token, canonical root boundary, serialized mutation lock을 요구한다. 원본 source file은 절대 삭제하지 않는다.

## 6. 파일·모듈별 개발 계획

### 6.1 Rust core/storage

- `crates/jikji-core/src/storage.rs`
  - `open_database`, `ensure_root`, `root_id`, `replace_artifacts`, `load_artifacts`, `delete_root`
  - migration version table와 transaction 경계
  - 주석 규칙: root key의 canonical 불변식, transaction atomicity, source preservation 이유를 함수 위에 기록
- 계획 추가: `StorageError`를 `NotFound/Busy/Corrupt/OutsideRoot`로 분해하고 request id를 상위로 전달

### 6.2 Rust index/search

- `crates/jikji-index/src/artifacts.rs`: prepare/reindex orchestration
- `crates/jikji-index/src/doc_cache.rs`: parser dispatch/cache/incremental reuse
- `crates/jikji-index/src/doc_media.rs`: optional media engine/deep archive
- `crates/jikji-index/src/doctor.rs`: health report
- `crates/jikji-search/src/searcher.rs`: SQLite search/filename/body scoring
- `crates/jikji-search/src/discover.rs`: answer pack/handoff/fallback
- `crates/jikji-cli/src/search_commands.rs`: stale-while-refresh/empty-result reindex

### 6.3 Rust GUI backend

- `crates/jikji-cli/src/gui_commands.rs`: embedded SPA, lifecycle, loopback bind
- `crates/jikji-cli/src/gui_commands/http.rs`: request parsing, percent decoding, response headers
- `crates/jikji-cli/src/gui_commands/routing.rs`: route table, token guard, path resolution, proxy-facing API
- 계획 추가: `gui_commands/jobs.rs`에 long-running operation registry를 분리하고 polling endpoint를 추가

### 6.4 Frontend

현재 embedded `INDEX_HTML`의 JS/CSS를 차후 다음 논리 컴포넌트로 분리한다.

- `ui/app-shell`: global state, request id, toast/error
- `ui/root-selector`: roots/status/add/switch
- `ui/explorer-pane`: files/folders
- `ui/search-pane`: find query/results/evidence
- `ui/preview-pane`: safe text rendering/highlight ranges
- `ui/index-controls`: refresh/reindex/deep/remove confirmation
- `ui/api-client`: same-origin calls, timeout, error envelope

Rust에 번들되는 정적 asset을 유지해 Python/Node runtime 의존성을 만들지 않는다.

### 6.5 MarkerAI integration

- `/home/cheol/projects/github_issue_solver/app/main.py`
  - `JIKJI_UPSTREAM = http://127.0.0.1:18768`
  - `/jikji` HTML proxy
  - `/jikji/{path}` method/query/body/content-type proxy
  - root-relative asset/API rewrite
- root menu `SERVICES`에 `Jikji 파일 인덱스`를 `/jikji`로 추가
- 배포: `systemctl --user restart github-issue-solver`
- 인증: Markr session boundary + Rust mutation manage token; do not log tokens
- next plan: pass authenticated user identity into Rust via short-lived signed handoff token instead of manual manage token entry

### 6.6 Python disposition

`python/jikji/`를 지금 이동하지 않는다. CI, parity, golden capture, Python evaluator, package layout tests가 active reference를 요구한다. Rust GUI/index/search는 Python을 실행하지 않는다. 아카이브는 다음 선행 작업 후 별도 change set으로 한다.

1. parity evaluator Rust migration and golden regeneration
2. `.github/workflows/ci.yml` Python lane removal/replacement
3. `tests/parity/test_monorepo_layout.py`, `uv.lock`, `pyproject.toml` migration
4. legacy Python package deprecation notice and release policy

## 7. 구현 순서와 의존성

1. **Diagnose**: request id/error envelope/proxy logs; reproduce authenticated browser flow
2. **Contract**: freeze API schemas/state machine/error codes
3. **Storage**: migrations/jobs/status tables
4. **Backend**: roots/files/search/preview/mutations
5. **Frontend**: loading/error/empty/highlight/confirmation states
6. **Integration**: Marker proxy/menu/auth handoff
7. **Verification**: API integration, browser E2E, security, performance, rollback
8. **Release**: versioned binary, systemd/service config, deployment smoke

## 8. 테스트·관측·완료 기준

### Tests

- unit: storage migrations, root boundary, UTF-16 match ranges, query parser, error mapping
- integration: Rust route against temp central DB and temp roots; every mutation token/error branch
- E2E: start loopback Rust GUI + proxy; browser click menu → `/jikji`; search → select result → preview mark ranges; refresh/reindex/deep/remove
- security: traversal, symlink, malformed query, invalid token, oversized preview, concurrent mutation
- performance: prepare/find/search/deep/stale medians against baseline and absolute thresholds

### Observability

Every request logs `request_id`, route, root key (hashed), status, duration, error code; never log source contents, tokens, or full paths outside debug mode. UI displays request id in error state for support.

### Done gate

- No permanent `checking` state
- Every requested route has a success and failure test
- Browser flow succeeds through Marker proxy
- Central DB root isolation proven
- Rust default execution has no Python process
- Benchmark thresholds pass twice on same corpus
- Rollback steps documented

## 9. Open questions/backlog

- MarkerAI authenticated browser session and signed identity handoff contract
- Which roots should be offered by default on first install?
- Whether preview should expose full content or bounded windows for large files
- Job persistence across Rust process restart
- OCR/ASR engine packaging and model lifecycle
- Multi-user tenancy and per-user root visibility
