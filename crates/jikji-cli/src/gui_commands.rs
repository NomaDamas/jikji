use std::net::{TcpListener, TcpStream};
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use jikji_core::PrepareOptions;
use jikji_index::prepare;
use serde_json::json;

use crate::args::GuiArgs;
use crate::output::print_json;

mod http;
mod routing;
mod token;

use routing::{GuiState, route_request};
use token::ManagementToken;

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <meta name="color-scheme" content="dark">
  <title>Jikji Library</title>
  <style>
    :root {
      --ink: oklch(92% 0.018 190); --muted: oklch(70% 0.025 190);
      --paper: oklch(16% 0.025 205); --panel: oklch(21% 0.028 205);
      --line: oklch(34% 0.035 205); --soft: oklch(27% 0.04 195);
      --accent: oklch(76% 0.14 190); --accent-dark: oklch(84% 0.12 190);
      --good: oklch(72% 0.13 145); --bad: oklch(74% 0.16 25);
      --r1: 4px; --r2: 8px; --r3: 12px;
      --shadow: 0 8px 24px oklch(5% 0.02 205 / .32);
      font-family: "Aptos", "Segoe UI", sans-serif; color: var(--ink); background: var(--paper);
    }
    * { box-sizing: border-box; }
    body { margin: 0; min-height: 100vh; line-height: 1.5; }
    button, input, select { font: inherit; color: inherit; }
    button, input, select, .file-row { min-height: 40px; }
    button { border: 1px solid var(--line); border-radius: var(--r1); background: var(--panel); padding: 8px 12px; cursor: pointer; font-weight: 650; }
    button:hover { border-color: var(--accent); color: var(--accent-dark); }
    button.primary { border-color: var(--accent); background: var(--accent); color: var(--panel); }
    button.danger { color: var(--bad); }
    button:disabled { cursor: wait; opacity: .55; }
    :focus-visible { outline: 3px solid oklch(69% 0.13 65); outline-offset: 2px; }
    .topbar { display: flex; align-items: center; gap: 16px; padding: 16px 24px; border-bottom: 1px solid var(--line); background: var(--panel); }
    .brand { display: flex; align-items: baseline; gap: 10px; min-width: 190px; }
    .brand strong { font-family: Georgia, serif; font-size: 24px; letter-spacing: -.04em; }
    .brand span, .eyebrow { color: var(--muted); font-size: 12px; letter-spacing: .08em; text-transform: uppercase; }
    .sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0,0,0,0); white-space: nowrap; border: 0; }
    .search-form { display: flex; gap: 8px; flex: 1; max-width: 760px; }
    input, select { width: 100%; border: 1px solid var(--line); border-radius: var(--r1); background: var(--paper); padding: 8px 12px; }
    .token-wrap { display: flex; align-items: center; gap: 8px; margin-left: auto; }
    .token-wrap input { width: 170px; }
    .stats { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 12px; padding: 16px 24px; }
    .stat { min-width: 0; min-height: 72px; padding: 12px 16px; border: 1px solid var(--line); border-radius: var(--r2); background: var(--panel); }
    .stat b { display: block; margin-top: 4px; font-family: Georgia, serif; font-size: 20px; font-weight: 700; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .workspace { display: grid; grid-template-columns: minmax(240px, 1fr) minmax(340px, 1.45fr) minmax(320px, 1.35fr); min-height: calc(100vh - 137px); border-top: 1px solid var(--line); }
    .pane { min-width: 0; background: var(--panel); }
    .pane + .pane { border-left: 1px solid var(--line); }
    .pane-head { display: flex; align-items: center; gap: 8px; min-height: 56px; padding: 8px 16px; border-bottom: 1px solid var(--line); }
    .pane-head h2 { margin: 0; font-family: Georgia, serif; font-size: 18px; }
    .pane-head .spacer { flex: 1; }
    .compact { padding: 6px 9px; }
    .root-select { margin: 12px 16px 4px; width: calc(100% - 32px); }
    .tree-path { padding: 8px 16px; color: var(--muted); font-size: 13px; overflow-wrap: anywhere; }
    .list { margin: 0; padding: 0 8px 16px; list-style: none; }
    .file-row, .result { width: 100%; border: 0; border-radius: var(--r1); background: transparent; text-align: left; }
    .file-row { display: grid; grid-template-columns: 24px 1fr auto; align-items: center; gap: 8px; padding: 6px 8px; font-weight: 500; }
    .file-row:hover, .file-row[aria-current="true"], .result:hover, .result[aria-current="true"] { background: var(--soft); color: var(--ink); }
    .file-row small { color: var(--muted); font-variant-numeric: tabular-nums; }
    .results-meta { padding: 12px 16px 0; color: var(--muted); font-size: 13px; }
    .result { display: block; margin-top: 8px; padding: 12px; border: 1px solid transparent; }
    .result strong { display: block; overflow-wrap: anywhere; }
    .result .path, .result .evidence { color: var(--muted); font-size: 13px; overflow-wrap: anywhere; }
    .result .score { float: right; color: var(--accent-dark); font-variant-numeric: tabular-nums; }
    .preview { padding: 16px; }
    .preview-meta { display: flex; flex-wrap: wrap; gap: 8px 16px; margin-bottom: 12px; color: var(--muted); font-size: 13px; }
    pre { margin: 0; max-height: calc(100vh - 250px); overflow: auto; border: 1px solid var(--line); border-radius: var(--r2); background: var(--paper); padding: 16px; white-space: pre-wrap; overflow-wrap: anywhere; font: 13px/1.65 "Cascadia Mono", monospace; tab-size: 2; }
    mark { border-radius: 2px; background: oklch(68% 0.13 88); color: oklch(18% 0.03 75); }
    .state { margin: 16px; padding: 24px 16px; border: 1px dashed var(--line); border-radius: var(--r2); color: var(--muted); text-align: center; }
    .state strong { display: block; color: var(--ink); margin-bottom: 4px; }
    .error { margin: 0; padding: 10px 24px; background: oklch(27% 0.07 25); color: oklch(84% 0.1 25); border-bottom: 1px solid oklch(45% 0.1 25); }
    .error[hidden], .toast[hidden] { display: none; }
    .toast { position: fixed; right: 20px; bottom: 20px; z-index: 4; max-width: 360px; padding: 12px 16px; border-radius: var(--r2); color: var(--panel); background: var(--ink); box-shadow: var(--shadow); }
    dialog { width: min(440px, calc(100% - 32px)); border: 1px solid var(--line); border-radius: var(--r3); background: var(--panel); color: var(--ink); box-shadow: var(--shadow); }
    dialog::backdrop { background: oklch(25% 0.025 65 / .35); }
    dialog h2 { font-family: Georgia, serif; }
    dialog menu { display: flex; justify-content: flex-end; gap: 8px; padding: 16px 0 0; }
    .busy::after { content: ""; display: inline-block; width: 12px; height: 12px; margin-left: 8px; border: 2px solid currentColor; border-right-color: transparent; border-radius: 50%; animation: spin .7s cubic-bezier(.4,0,.2,1) infinite; }
    @keyframes spin { to { transform: rotate(360deg); } }
    @media (max-width: 980px) {
      .topbar { flex-wrap: wrap; } .search-form { order: 3; max-width: none; flex-basis: 100%; }
      .stats { grid-template-columns: repeat(2, 1fr); }
      .workspace { grid-template-columns: minmax(220px, .8fr) minmax(360px, 1.2fr); }
      .preview-pane { grid-column: 1 / -1; border-left: 0 !important; border-top: 1px solid var(--line); }
      pre { max-height: 480px; }
    }
    @media (max-width: 640px) {
      .topbar, .stats { padding-left: 12px; padding-right: 12px; }
      .brand { min-width: 0; } .brand span { display: none; } .token-wrap { margin-left: 0; flex: 1; }
      .token-wrap input { width: 100%; } .stats { grid-template-columns: 1fr 1fr; }
      .workspace { display: block; } .pane + .pane { border-left: 0; border-top: 1px solid var(--line); }
      .pane { min-height: 320px; } .search-form button { padding-inline: 16px; }
    }
    @media (prefers-reduced-motion: reduce) { .busy::after { animation: none; } }
  </style>
</head>
<body>
  <header class="topbar">
    <div class="brand"><strong>Jikji</strong><span>Local index library</span></div>
    <form class="search-form" id="search-form" role="search">
      <label class="sr-only" for="query">Find indexed files</label>
      <input id="query" name="q" type="search" placeholder="Find a contract, note, person…" autocomplete="off" required>
      <button class="primary" id="search-button" type="submit">Find</button>
    </form>
    <div class="token-wrap">
      <label class="eyebrow" for="manage-token">Manage token</label>
      <input id="manage-token" type="password" autocomplete="off" spellcheck="false" aria-describedby="token-help" placeholder="Required for changes">
    </div>
  </header>
  <p id="token-help" hidden>The token stays in this tab and is sent only to local mutation routes.</p>
  <div class="error" id="global-error" role="alert" hidden></div>
  <section class="stats" aria-label="Index health">
    <div class="stat"><span class="eyebrow">Index health</span><b id="health">Checking…</b></div>
    <div class="stat"><span class="eyebrow">Indexed files</span><b id="file-count">—</b></div>
    <div class="stat"><span class="eyebrow">Root size</span><b id="root-size">—</b></div>
    <div class="stat"><span class="eyebrow">Last indexed</span><b id="last-indexed">—</b></div>
  </section>
  <main class="workspace" aria-label="Jikji index browser">
    <nav class="pane explorer-pane" aria-labelledby="explorer-title">
      <div class="pane-head"><h2 id="explorer-title">Library</h2><span class="spacer"></span><button class="compact" id="refresh" type="button">Refresh</button></div>
      <label class="sr-only" for="root-select">Indexed root</label><select class="root-select" id="root-select"><option>Loading roots…</option></select>
      <div class="tree-path" id="tree-path">/</div>
      <ul class="list" id="file-list" aria-live="polite"><li class="state"><strong>Loading library</strong>Reading indexed files…</li></ul>
    </nav>
    <section class="pane" aria-labelledby="results-title">
      <div class="pane-head"><h2 id="results-title">Find results</h2><span class="spacer"></span><span class="eyebrow" id="confidence"></span></div>
      <div class="results-meta" id="results-meta">Search uses Jikji Find and links directly to indexed evidence.</div>
      <ol class="list" id="results" aria-live="polite"><li class="state"><strong>Ready to find</strong>Enter a phrase, filename, topic, or person above.</li></ol>
    </section>
    <aside class="pane preview-pane" aria-labelledby="preview-title">
      <div class="pane-head"><h2 id="preview-title">Content preview</h2><span class="spacer"></span><button class="compact" id="download" type="button" disabled>Download</button><button class="compact" id="reveal" type="button" disabled>Reveal</button></div>
      <div class="preview" id="preview"><div class="state"><strong>No file selected</strong>Select a file or find result to inspect highlighted content.</div></div>
    </aside>
  </main>
  <footer class="pane-head" aria-label="Index management">
    <span class="eyebrow">Folder controls</span><span class="spacer"></span>
    <button id="reindex" type="button">Reindex</button><button id="deep-index" type="button">Deep index</button><button class="danger" id="remove-root" type="button">Remove root</button>
  </footer>
  <div class="toast" id="toast" role="status" aria-live="polite" hidden></div>
  <dialog id="confirm-dialog" aria-labelledby="confirm-title" aria-describedby="confirm-copy">
    <h2 id="confirm-title">Confirm action</h2><p id="confirm-copy"></p>
    <menu><button id="confirm-cancel" type="button">Cancel</button><button class="danger" id="confirm-ok" type="button">Confirm</button></menu>
  </dialog>
  <script>
  (() => {
    "use strict";
    const $ = (id) => document.getElementById(id);
    const state = { root: "", folder: "", selected: "", query: "", roots: [], confirmAction: null };
    const number = new Intl.NumberFormat();
    const setText = (id, value) => { $(id).textContent = value == null || value === "" ? "—" : String(value); };
    const bytes = (value) => { const n = Number(value); if (!Number.isFinite(n)) return "—"; const u = ["B","KB","MB","GB","TB"]; let i=0,x=n; while(x>=1024&&i<u.length-1){x/=1024;i++;} return `${x>=10||i===0?x.toFixed(0):x.toFixed(1)} ${u[i]}`; };
    const date = (value) => { if (!value) return "—"; const d = new Date(typeof value === "number" && value < 1e12 ? value * 1000 : value); return Number.isNaN(d.valueOf()) ? String(value) : d.toLocaleString(); };
    const errorMessage = (error) => error instanceof Error ? error.message : String(error);
    function showError(error) { const el=$("global-error"); el.textContent=errorMessage(error); el.hidden=false; }
    function clearError() { $("global-error").hidden=true; $("global-error").textContent=""; }
    function toast(message) { const el=$("toast"); el.textContent=message; el.hidden=false; clearTimeout(toast.timer); toast.timer=setTimeout(()=>el.hidden=true,3200); }
    function params(values) { const out=new URLSearchParams(); Object.entries(values).forEach(([k,v])=>{if(v!==undefined&&v!==null&&v!=="")out.set(k,String(v));}); return out; }
    async function api(path, values={}, options={}) {
      const url = `${path}?${params(values)}`;
      const response = await fetch(url, { method: options.method || "GET", headers: { "Accept": "application/json" }, cache: "no-store" });
      const contentType = response.headers.get("content-type") || "";
      const payload = contentType.includes("json") ? await response.json() : await response.text();
      if (!response.ok) throw new Error(payload && payload.error ? payload.error : `Request failed (${response.status})`);
      return payload;
    }
    function token() { const value=$("manage-token").value.trim(); if (!value) { $("manage-token").focus(); throw new Error("Enter the management token printed when Jikji GUI started."); } return value; }
    function listState(target, title, copy) { target.replaceChildren(); const li=document.createElement("li"); li.className="state"; const strong=document.createElement("strong"); strong.textContent=title; li.append(strong,document.createTextNode(copy)); target.append(li); }
    function statistics(payload) { return payload.statistics || payload.stats || payload.manifest?.statistics || {}; }
    function updateStats(payload) {
      const stats=statistics(payload), manifest=payload.manifest || {};
      setText("health", payload.prepared ? "Ready" : "Needs indexing");
      setText("file-count", number.format(stats.files ?? stats.file_count ?? manifest.file_count ?? 0));
      setText("root-size", bytes(stats.bytes ?? stats.total_bytes ?? manifest.total_bytes));
      setText("last-indexed", date(stats.updated_at ?? stats.indexed_at ?? manifest.generated_at));
    }
    async function loadRoots() {
      const data=await api("/api/roots"); state.roots=Array.isArray(data.roots)?data.roots:[]; state.root=data.active_root || state.root || state.roots[0]?.root || "";
      const select=$("root-select"); select.replaceChildren();
      if (!state.roots.length) { const option=document.createElement("option"); option.textContent="No indexed roots"; select.append(option); select.disabled=true; return; }
      select.disabled=false; state.roots.forEach(item=>{const option=document.createElement("option"); option.value=item.root; option.textContent=item.root; option.selected=item.root===state.root; select.append(option);});
    }
    async function loadStatus() { const data=await api("/api/status"); state.root=data.root || state.root; updateStats(data); }
    async function loadFiles(folder="") {
      state.folder=folder; setText("tree-path", folder || "/"); listState($("file-list"),"Loading folder","Reading indexed entries…");
      try { const data=await api("/api/files",{path:folder}); const entries=Array.isArray(data.entries)?data.entries:[]; const list=$("file-list"); list.replaceChildren();
        if (folder) { const li=document.createElement("li"), up=document.createElement("button"); up.className="file-row"; up.type="button"; up.append(document.createTextNode("↰"),document.createTextNode("Parent folder"),document.createTextNode("")); up.addEventListener("click",()=>loadFiles(folder.split("/").slice(0,-1).join("/"))); li.append(up); list.append(li); }
        entries.forEach(entry=>{const li=document.createElement("li"),button=document.createElement("button"),icon=document.createElement("span"),name=document.createElement("span"),size=document.createElement("small"); button.type="button"; button.className="file-row"; button.dataset.path=entry.path; icon.textContent=entry.type==="directory"||entry.type==="folder"?"▸":"·"; name.textContent=entry.name || entry.path; size.textContent=entry.type==="directory"||entry.type==="folder"?"":bytes(entry.size); button.append(icon,name,size); button.addEventListener("click",()=>entry.type==="directory"||entry.type==="folder"?loadFiles(entry.path):loadPreview(entry.path)); li.append(button); list.append(li); });
        if (!entries.length && !folder) listState(list,"Library is empty","Reindex this root to discover files.");
      } catch(error) { listState($("file-list"),"Could not load files",errorMessage(error)); showError(error); }
    }
    function previewText(data) {
      const container=$("preview"); container.replaceChildren(); const meta=document.createElement("div"); meta.className="preview-meta";
      [data.path,data.type,bytes(data.size),data.encoding].filter(Boolean).forEach(value=>{const span=document.createElement("span"); span.textContent=value; meta.append(span);}); container.append(meta);
      if (data.supported===false) { const box=document.createElement("div"); box.className="state"; const strong=document.createElement("strong"); strong.textContent="Preview unavailable"; box.append(strong,document.createTextNode(data.reason || "This file type cannot be shown safely.")); container.append(box); return; }
      const pre=document.createElement("pre"), content=String(data.content || ""), matches=Array.isArray(data.matches)?data.matches.slice().sort((a,b)=>a.start-b.start):[]; let cursor=0;
      matches.forEach(match=>{const start=Math.max(cursor,Number(match.start)||0), end=Math.min(content.length,Number(match.end)||0); if(end<=start)return; pre.append(document.createTextNode(content.slice(cursor,start))); const mark=document.createElement("mark"); mark.textContent=content.slice(start,end); pre.append(mark); cursor=end;}); pre.append(document.createTextNode(content.slice(cursor))); container.append(pre);
    }
    async function loadPreview(path) { state.selected=path; document.querySelectorAll("[data-path]").forEach(el=>el.setAttribute("aria-current",String(el.dataset.path===path))); $("download").disabled=false; $("reveal").disabled=false; $("preview").replaceChildren(); const loading=document.createElement("div"); loading.className="state busy"; loading.textContent="Loading preview"; $("preview").append(loading);
      try { previewText(await api("/api/preview",{path,q:state.query})); } catch(error) { $("preview").replaceChildren(); const box=document.createElement("div"); box.className="state"; box.textContent=errorMessage(error); $("preview").append(box); showError(error); }
    }
    function renderResults(data) { const candidates=Array.isArray(data.candidates)?data.candidates:[]; const list=$("results"); list.replaceChildren(); setText("confidence",data.confidence?`${data.confidence} confidence`:""); setText("results-meta",`${candidates.length} result${candidates.length===1?"":"s"} for “${state.query}”`);
      if(!candidates.length){listState(list,"No indexed match","Try a shorter phrase, a filename fragment, or reindex the root.");return;}
      candidates.forEach((item,index)=>{const li=document.createElement("li"),button=document.createElement("button"),score=document.createElement("span"),title=document.createElement("strong"),path=document.createElement("div"),evidence=document.createElement("div"); button.type="button";button.className="result";button.dataset.path=item.path;score.className="score";score.textContent=Number.isFinite(Number(item.score))?Number(item.score).toFixed(2):`#${index+1}`;title.textContent=item.name||item.path;path.className="path";path.textContent=item.path;evidence.className="evidence";evidence.textContent=item.preview_snippet || (Array.isArray(item.evidence)?item.evidence[0]:"") || (Array.isArray(item.reasons)?item.reasons.join(" · "):"");button.append(score,title,path,evidence);button.addEventListener("click",()=>loadPreview(item.path));li.append(button);list.append(li);});
    }
    async function find(event) { event.preventDefault(); const q=$("query").value.trim(); if(!q)return; state.query=q; clearError(); const button=$("search-button"); button.disabled=true; button.classList.add("busy"); listState($("results"),"Finding evidence","Searching the active index…"); try { renderResults(await api("/api/find",{q,top_k:40})); } catch(error) { listState($("results"),"Search failed",errorMessage(error)); showError(error); } finally { button.disabled=false;button.classList.remove("busy"); } }
    async function switchRoot() { const path=$("root-select").value; if(!path)return; try { const data=await api("/api/root",{path,token:token()},{method:"POST"}); state.root=data.root||path;state.folder="";state.selected="";updateStats(data);await loadFiles();toast("Active root changed."); } catch(error){showError(error);} }
    function confirmAction(title,copy,label,action){setText("confirm-title",title);setText("confirm-copy",copy);setText("confirm-ok",label);state.confirmAction=action;$("confirm-dialog").showModal();}
    async function mutation(path, values, label) { clearError(); const button=$(label); button.disabled=true;button.classList.add("busy"); try { const data=await api(path,{...values,token:token()},{method:"POST"}); if(data.prepared!==undefined)updateStats(data);await Promise.all([loadRoots(),loadFiles()]);toast(`${button.textContent.trim()} complete.`); } catch(error){showError(error);} finally {button.disabled=false;button.classList.remove("busy");} }
    $("refresh").addEventListener("click",()=>mutation("/api/refresh",{},"refresh"));
    $("reindex").addEventListener("click",()=>mutation("/api/reindex",{},"reindex"));
    $("deep-index").addEventListener("click",()=>mutation("/api/deep-index",{},"deep-index"));
    $("remove-root").addEventListener("click",()=>confirmAction("Remove indexed root?",`This removes ${state.root} from Jikji's central index. Source files are not deleted.`,"Remove root",()=>mutation("/api/remove-root",{path:state.root},"remove-root")));
    $("confirm-cancel").addEventListener("click",()=>$("confirm-dialog").close()); $("confirm-ok").addEventListener("click",()=>{const action=state.confirmAction;$("confirm-dialog").close();state.confirmAction=null;if(action)action();});
    $("download").addEventListener("click",()=>{if(state.selected)location.assign(`/download?${params({path:state.selected})}`);});
    $("reveal").addEventListener("click",async()=>{try{await api("/reveal",{path:state.selected,token:token()},{method:"POST"});toast("Opened in your file manager.");}catch(error){showError(error);}});
    Promise.all([loadRoots(),loadStatus()]).then(()=>loadFiles()).catch(error=>{showError(error);listState($("file-list"),"Jikji is unavailable",errorMessage(error));});
  })();
  </script>
</body>
</html>"##;

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
            "manage_token": token.as_str()
        }))?;
    } else if !args.serve_child {
        println!("Jikji GUI: {url}");
    }
    serve_loop(listener, GuiState::new(root, token))
}

fn spawn_background(args: GuiArgs) -> jikji_core::Result<ExitCode> {
    let port = if args.port == 0 {
        reserve_loopback_port(&args.host)?
    } else {
        args.port
    };
    let token = ManagementToken::generate()?;
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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
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
        "cleanup": cleanup_command(child.id()),
    });
    if args.json {
        print_json(&payload)?;
    } else {
        println!("{url}");
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

fn serve_loop(listener: TcpListener, state: GuiState) -> jikji_core::Result<ExitCode> {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let request_state = state.clone();
                thread::spawn(move || {
                    let _ = handle_stream(stream, &request_state);
                });
            }
            Err(source) => return Err(jikji_core::io_error("<gui-accept>", source)),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn handle_stream(stream: TcpStream, state: &GuiState) -> jikji_core::Result<()> {
    let request = http::HttpRequest::read(&stream)?;
    let response = route_request(state, &request, INDEX_HTML);
    http::write_response(stream, &response)
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
    fn index_html_has_semantic_three_pane_shell_and_focusable_controls() {
        for landmark in ["<header", "<main", "<nav", "<section", "<aside", "<footer"] {
            assert!(
                INDEX_HTML.contains(landmark),
                "missing landmark: {landmark}"
            );
        }
        for control in [
            "id=\"search-form\"",
            "id=\"root-select\"",
            "id=\"refresh\"",
            "id=\"reindex\"",
            "id=\"deep-index\"",
            "id=\"remove-root\"",
            "id=\"download\"",
            "id=\"reveal\"",
            "id=\"confirm-dialog\"",
        ] {
            assert!(INDEX_HTML.contains(control), "missing control: {control}");
        }
        assert!(INDEX_HTML.contains(":focus-visible"));
        assert!(INDEX_HTML.contains("min-height: 40px"));
        assert!(INDEX_HTML.contains("@media (max-width: 640px)"));
    }

    #[test]
    fn index_html_calls_central_gui_routes_and_keeps_mutation_token_out_of_markup() {
        for route in [
            "/api/status",
            "/api/roots",
            "/api/files",
            "/api/find",
            "/api/preview",
            "/api/root",
            "/api/refresh",
            "/api/reindex",
            "/api/deep-index",
            "/api/remove-root",
            "/download",
            "/reveal",
        ] {
            assert!(INDEX_HTML.contains(route), "missing route: {route}");
        }
        assert!(INDEX_HTML.contains("token:token()"));
        assert!(INDEX_HTML.contains("textContent"));
        assert!(!INDEX_HTML.contains("innerHTML"));
        assert!(!INDEX_HTML.contains("localStorage"));
        assert!(INDEX_HTML.contains("showModal"));
    }
}
