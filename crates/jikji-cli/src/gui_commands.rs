use std::net::{TcpListener, TcpStream};
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use jikji_core::PrepareOptions;
use jikji_index::prepare;
use serde_json::json;

use crate::args::GuiArgs;
use crate::output::print_json;

mod access;
mod http;
mod jobs;
mod preview;
mod routing;
mod scenarios;
mod token;

use access::{AccessGate, Gate, security_headers};
use routing::{GuiState, route_request};
use token::ManagementToken;

const INDEX_HTML: &str = include_str!("gui_commands/assets/index.html");
pub(crate) fn run_gui(args: GuiArgs) -> jikji_core::Result<ExitCode> {
    if !is_loopback_host(&args.host) {
        return Err(invalid_input("GUI host must be loopback"));
    }
    if args.background && !args.serve_child {
        return spawn_background(args);
    }
    if args.prepare {
        prepare(&args.root, &PrepareOptions::default())?;
    }
    let root = args
        .root
        .canonicalize()
        .map_err(|source| jikji_core::io_error(&args.root, source))?;
    let token = match args.manage_token {
        Some(value) => ManagementToken::new(value),
        None => ManagementToken::generate()?,
    };
    let access_token = access_token(args.access_token.clone())?;
    // Warm the central SQLite BEFORE binding the port. This way, when the
    // hub ready check (TCP connect) succeeds, the expensive first open_database
    // + register_root (which can take 10s+ on a large index with 50k+ docs) has
    // already completed. Clients that connect immediately after "ready" will
    // see warm-DB response times (~1s for 18 roots) instead of queuing behind
    // the open.
    let _ = jikji_core::storage::open_database();
    let _ = jikji_core::storage::register_root(&root);
    let listener = TcpListener::bind((args.host.as_str(), args.port))
        .map_err(|source| jikji_core::io_error("<gui-bind>", source))?;
    let address = listener
        .local_addr()
        .map_err(|source| jikji_core::io_error("<gui-addr>", source))?;
    let url = format!("http://{}:{}", address.ip(), address.port());
    if args.json && !args.serve_child {
        print_json(&json!({
            "url": url,
            "root": root,
            "background": false,
            "manage_token": token.as_str(),
            "access_token": access_token.as_str()
        }))?;
    } else if !args.serve_child {
        println!("Jikji GUI: {url}");
        println!("Access token: {}", access_token.as_str());
    }
    let gate = AccessGate::new(access_token, args.allow_host);
    serve_loop(listener, GuiState::new(root, token), gate)
}

/// Console login token: `--access-token`, else `$JIKJI_GUI_ACCESS_TOKEN`, else a random one.
fn access_token(flag: Option<String>) -> jikji_core::Result<ManagementToken> {
    let configured = flag
        .or_else(|| std::env::var("JIKJI_GUI_ACCESS_TOKEN").ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    match configured {
        Some(value) if value.len() < 16 => Err(invalid_input(
            "GUI access token must be at least 16 characters",
        )),
        Some(value) => Ok(ManagementToken::new(value)),
        None => ManagementToken::generate(),
    }
}

fn spawn_background(args: GuiArgs) -> jikji_core::Result<ExitCode> {
    let port = if args.port == 0 {
        reserve_loopback_port(&args.host)?
    } else {
        args.port
    };
    let token = ManagementToken::generate()?;
    let access_token = access_token(args.access_token.clone())?;
    let exe =
        std::env::current_exe().map_err(|source| jikji_core::io_error("<current-exe>", source))?;
    let mut command = Command::new(exe);
    command
        .arg("gui")
        .arg(&args.root)
        .arg("--host")
        .arg(&args.host)
        .arg("--port")
        .arg(port.to_string())
        .arg("--no-open")
        .arg("--serve-child")
        .arg("--manage-token")
        .arg(token.as_str())
        // Passed through the environment so the token never shows up in `ps` output.
        .env("JIKJI_GUI_ACCESS_TOKEN", access_token.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for host in &args.allow_host {
        command.arg("--allow-host").arg(host);
    }
    if args.prepare {
        command.arg("--prepare");
    }
    let child = command
        .spawn()
        .map_err(|source| jikji_core::io_error("<gui-spawn>", source))?;
    let url = format!("http://{}:{port}", args.host);
    wait_until_ready(&args.host, port)?;
    let payload = json!({
        "url": url,
        "pid": child.id(),
        "root": args.root,
        "background": true,
        "manage_token": token.as_str(),
        "access_token": access_token.as_str(),
        "cleanup": cleanup_command(child.id()),
    });
    if args.json {
        print_json(&payload)?;
    } else {
        println!("{url}");
        println!("Access token: {}", access_token.as_str());
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(windows)]
fn cleanup_command(pid: u32) -> String {
    format!("taskkill /PID {pid} /F /T")
}

#[cfg(not(windows))]
fn cleanup_command(pid: u32) -> String {
    format!("kill {pid}")
}

fn serve_loop(
    listener: TcpListener,
    state: GuiState,
    gate: AccessGate,
) -> jikji_core::Result<ExitCode> {
    // Library-root discovery walks HOME / Google Drive FUSE and must not
    // run before accept(). A TCP ready probe succeeds at bind; a synchronous
    // enqueue here delayed the first /api/find by ~14s on a cold Drive cache.
    thread::spawn(|| {
        crate::post_install_commands::enqueue_missing_library_root_prepares();
    });
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let request_state = state.clone();
                let request_gate = gate.clone();
                thread::spawn(move || {
                    let _ = handle_stream(stream, &request_state, &request_gate);
                });
            }
            Err(source) => return Err(jikji_core::io_error("<gui-accept>", source)),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn handle_stream(stream: TcpStream, state: &GuiState, gate: &AccessGate) -> jikji_core::Result<()> {
    let request = http::HttpRequest::read(&stream)?;
    let response = match gate.check(&request) {
        Gate::Allow => route_request(state, &request, INDEX_HTML),
        Gate::Respond(response) => response,
    };
    http::write_response(stream, response, &security_headers())
}

fn reserve_loopback_port(host: &str) -> jikji_core::Result<u16> {
    let listener = TcpListener::bind((host, 0))
        .map_err(|source| jikji_core::io_error("<gui-port>", source))?;
    let port = listener
        .local_addr()
        .map_err(|source| jikji_core::io_error("<gui-port>", source))?
        .port();
    drop(listener);
    Ok(port)
}

fn wait_until_ready(host: &str, port: u16) -> jikji_core::Result<()> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if TcpStream::connect((host, port)).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(invalid_input("GUI child did not become ready"))
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

fn invalid_input(message: impl Into<String>) -> jikji_core::JikjiError {
    jikji_core::io_error(
        "<gui>",
        std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into()),
    )
}

#[cfg(test)]
mod tests {
    use super::INDEX_HTML;
    #[test]
    fn console_has_accessible_shell_before_network_requests() {
        let markup = INDEX_HTML.split("<script>").next().unwrap();
        for id in [
            "main-content",
            "panel-overview",
            "panel-search",
            "panel-index",
            "panel-tools",
            "nav-overview",
            "nav-search",
            "nav-index",
            "nav-tools",
            "manage-token",
            "search-query",
            "roots-panel",
            "files-panel",
            "indexed-panel",
            "settings-dialog",
            "search-split",
            "index-split",
            "search-preview-pane",
            "index-preview-pane",
            "search-preview-modes",
            "index-preview-modes",
            "search-snippet-tab",
            "search-file-tab",
            "index-snippet-tab",
            "index-file-tab",
            "search-preview-back",
            "index-preview-back",
            "logout",
            "confirm-dialog",
            "jobs-strip",
            "cancel-job",
        ] {
            assert!(markup.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        assert!(markup.contains("<main"));
        assert!(markup.contains("<nav"));
        assert!(markup.contains("<header"));
        assert!(markup.contains("aria-current=\"page\""));
        assert!(markup.contains("aria-busy=\"true\""));
        assert!(markup.contains("class=\"skeleton\""));
        assert!(!INDEX_HTML.contains("preview-dialog"));
        assert!(markup.contains("파일을 선택하면 여기에 미리보기가 표시됩니다"));
        assert!(markup.contains("grid-template-columns:minmax(0,3fr) minmax(0,2fr)"));
        for scope in ["search", "index"] {
            assert!(markup.contains(&format!(
                "id=\"{scope}-preview-modes\" class=\"preview-modes\" role=\"tablist\""
            )));
        }
        assert!(INDEX_HTML.find("</main>").unwrap() < INDEX_HTML.find("fetch(").unwrap());
    }
    #[test]
    fn console_preview_helpers_render_safe_typed_content() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let script = INDEX_HTML.split_once("function queryRanges(").unwrap().1;
        let helpers = format!(
            "function queryRanges({}",
            script.split_once("function snippetCard(").unwrap().0
        );
        let harness = r#"
const assert = require('node:assert/strict');
class Node {
  constructor(tag, text='') { this.tagName=tag.toUpperCase(); this.text=text; this.children=[]; this.attributes={}; }
  append(...nodes) { this.children.push(...nodes); }
  setAttribute(key,value) { this.attributes[key]=value; }
  set textContent(value) { this.text=value; this.children=[]; }
  get textContent() { return this.text+this.children.map(n=>n.textContent).join(''); }
}
const document={createTextNode:text=>new Node('#text',text)};
const el=(tag,text='',cls='')=>Object.assign(new Node(tag,text),{className:cls});
const ext=path=>path.split('.').pop().toLowerCase();
const nodes=new Map(),$=id=>{if(!nodes.has(id))nodes.set(id,new Node('button'));return nodes.get(id)};
const state={previews:{search:{item:{path:'manual.pdf'},mode:'file'}}};
"#;
        let assertions = r#"
for (const [path,label] of [['manual.PDF','PDF'],['report.docx','문서 본문'],['memo.hwp','문서 본문'],['notes.txt','텍스트'],['main.py','코드'],['readme.md','마크다운'],['archive.zip',''],['mystery.bin','']]) assert.equal(fileModeLabel(path),label);
for (const [path,label] of [['template.dot','문서 본문'],['template.dotx','문서 본문'],['slides.pot','문서 본문'],['main.lua','코드'],['paper.tex','코드'],['analysis.R','코드'],['docs/manual.rst','텍스트'],['src/Makefile','코드'],['Dockerfile.dev','코드'],['config/.env','코드'],['config/.env.local','코드']]) assert.equal(fileModeLabel(path),label);
updatePreviewTabs('search');
assert.equal($('search-file-tab').textContent,'PDF');
state.previews.search.item={path:'report.docx'};
updatePreviewTabs('search');
assert.equal($('search-file-tab').textContent,'문서 본문');
assert.equal(state.previews.search.mode,'file');
state.previews.search.item={path:'archive.zip'};
updatePreviewTabs('search');
assert.equal($('search-file-tab').hidden,true);
assert.equal(state.previews.search.mode,'snippet');
assert.equal($('search-snippet-tab').tabIndex,0);
assert.equal($('search-preview-content').attributes['aria-labelledby'],'search-snippet-tab');
assert.deepEqual(queryRanges('😀 계약서 계약서','계약서'),[{start:3,end:6},{start:7,end:10}]);
assert.deepEqual(queryRanges('a+b aab','a+b'),[{start:0,end:3}]);
assert.deepEqual(queryRanges('x',''),[]);
assert.deepEqual(queryRanges('😀 계약서 사이 a+b','계약서 a+b'),[{start:3,end:6},{start:10,end:13}]);
assert.deepEqual(queryRanges('계약서 계약','계약 계약서'),[{start:0,end:3},{start:4,end:6}]);
assert.deepEqual(queryRanges('alpha beta alpha','alpha beta'),[{start:0,end:10}]);
assert.deepEqual(queryRanges('anything','   '),[]);
const hostile='<img src=x onerror=alert(1)> 계약서';
const highlighted=highlightText(hostile,'계약서','div');
assert.equal(highlighted.textContent,hostile);
assert.deepEqual(highlighted.children.map(n=>n.tagName),['#TEXT','MARK','#TEXT']);
const ranged=rangeText('😀 계약서',[{start:3,end:6},{start:0,end:99}]);
assert.equal(ranged.textContent,'😀 계약서');
assert.equal(ranged.children[1].textContent,'계약서');
const md=markdownBody('# 제목\n- 계약서\n> 인용\n```js\n<script>alert(1)</script>\n```','계약서');
assert.deepEqual(md.children.map(n=>n.tagName),['H1','UL','BLOCKQUOTE','PRE']);
assert.ok(md.textContent.includes('<script>alert(1)</script>'));
const pane={scrollTop:0,clientHeight:300,querySelector:()=>({getBoundingClientRect:()=>({top:600})}),getBoundingClientRect:()=>({top:100})};
scrollToFirstMatch(pane);
assert.equal(pane.scrollTop,400);
console.log('typed preview, UTF-16 ranges, literal hostile text, Markdown structure, pane-only scrolling: passed');
"#;
        let mut child = Command::new("node")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Node.js is required for console JavaScript behavior tests");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{harness}\n{helpers}\n{assertions}").as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[test]
    fn console_keeps_explorer_regions_dark_and_results_score_free() {
        let (markup, script) = INDEX_HTML.split_once("<script>").unwrap();
        assert!(markup.contains(":root{color-scheme:dark;"));
        assert!(markup.contains("scrollbar-width:thin"));
        assert!(markup.contains("scrollbar-color:var(--border-hover) transparent"));
        assert!(markup.contains("::-webkit-scrollbar-thumb"));
        assert!(markup.contains("--scrollbar-size:8px"));
        assert!(markup.contains("height:calc(100dvh - var(--header))"));
        assert!(markup.contains(
            "#search-results,#files-view,#indexed-view{flex:1;min-height:0;overflow:auto"
        ));
        assert!(markup.contains("#roots-panel{flex:1;min-height:0;overflow:auto"));
        assert!(markup.contains("class=\"preview-title-row\""));
        assert!(markup.contains("class=\"preview-controls\""));
        assert!(!script.contains("점수"));
        assert!(!INDEX_HTML.contains("score-cell"));
        assert!(script.contains("Number(b.score??b.s??0)-Number(a.score??a.s??0)"));
        assert!(script.contains("cell.colSpan=2"));
        assert!(script.contains("parsed_now:\"원본에서 추출\""));
        assert!(!script.contains("신뢰도"));
    }
    #[test]
    fn console_uses_the_api_contract_without_unsafe_dom_or_persistence() {
        let script = INDEX_HTML.split("<script>").nth(1).unwrap();
        for route in [
            "/api/status",
            "/api/roots",
            "/api/files",
            "/api/indexed-files",
            "/api/find",
            "/api/preview",
            "/api/preview/file",
            "/api/settings",
            "/api/snippet",
            "/api/preview/pages",
            "/api/preview/page?",
            "/api/scenarios",
            "/api/scenario-run",
            "/api/doctor",
            "/api/map",
            "/api/graph",
            "/api/brief",
            "/api/clean",
            "/api/root",
            "/api/remove-root",
            "/api/refresh",
            "/api/reindex",
            "/api/reindex-folder",
            "/api/remove-folder",
            "/api/deep-index",
            "/api/deep-index-target",
            "/api/index-selection",
            "/api/exclude-selection",
            "/api/jobs/",
            "/cancel",
            "/download",
            "/reveal",
            "/open",
            "/api/logout",
        ] {
            assert!(script.contains(route), "missing route {route}");
        }
        for unsafe_api in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "eval(",
            "localStorage",
        ] {
            assert!(
                !INDEX_HTML.contains(unsafe_api),
                "forbidden API {unsafe_api}"
            );
        }
        for kind in ["document", "text", "image", "pdf", "audio", "video", "none"] {
            assert!(
                script.contains(&format!("case \"{kind}\":")),
                "missing preview kind {kind}"
            );
        }
        assert!(script.contains("response.status===401"));
        assert!(script.contains("location.reload()"));
        assert!(script.contains("d.binary_url.startsWith(\"/api/preview/file\")"));
        assert!(script.contains("new URL(url,location.origin).origin===location.origin"));
        assert!(script.contains("modeSeq===p[modeKey]"));
        assert!(script.contains("snippetSeq"));
        assert!(script.contains("fileSeq"));
        assert!(
            script.contains("img.loading=index===0?\"eager\":\"lazy\""),
            "first page loads at once, the rest lazily"
        );
        assert!(script.contains("ArrowLeft"));
        assert!(script.contains("ArrowRight"));
        assert!(script.contains("document.createTextNode"));
        assert!(script.contains("el(\"mark\""));
        assert!(script.contains("aria-selected"));
        assert!(script.contains("120000"));
        assert!(script.contains("90000:20000"));
        assert!(!INDEX_HTML.contains("onclick="));
        assert!(script.contains("value.forEach(item=>p.append(key,String(item)))"));
        assert!(
            script.contains("output(\"시나리오 결과\",\"/api/scenario-run\",{id:select.value})")
        );
        assert!(script.contains("Promise.allSettled([loadStatus(),loadRoots()"));
    }
}
