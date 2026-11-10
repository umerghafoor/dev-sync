// The devsync sidebar in the Activity Bar:
//
//   RUN                 status (with Retry when offline), live push/pull progress,
//                       sync actions, command box, activity, recent commands
//   NOT ON REMOTE YET   A/M/D files waiting to be sent
//   CONFLICTS           (only when there are some)
//   REMOTE JOBS         ✓ / ✗ / … with open, stop, run again

import * as vscode from "vscode";
import { ActivityEntry, TransferView } from "./activity";
import { Model, Resource } from "./model";

export class ResourceItem extends vscode.TreeItem {
  constructor(readonly res: Resource) {
    super(res.uri, vscode.TreeItemCollapsibleState.None);
    this.label = res.label;
    this.description = res.deleted ? `${res.description} (deleted)`.trim() : res.description;
    this.tooltip = res.tooltip;
    this.contextValue = res.contextValue;
    this.command = res.command;
    if (res.kind === "job") {
      const icons: Record<string, [string, string]> = {
        "job-running": ["sync~spin", "charts.blue"],
        "job-ok": ["pass", "testing.iconPassed"],
        "job-failed": ["error", "testing.iconFailed"],
        "job-lost": ["question", "problemsWarningIcon.foreground"],
      };
      const [icon, color] = icons[res.contextValue] ?? ["circle-outline", "foreground"];
      this.iconPath = new vscode.ThemeIcon(icon, new vscode.ThemeColor(color));
    }
  }
}

class ListProvider implements vscode.TreeDataProvider<ResourceItem> {
  private readonly emitter = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this.emitter.event;
  model?: Model;

  constructor(private readonly pick: (m: Model) => Resource[]) {}

  refresh(): void {
    this.emitter.fire();
  }

  getTreeItem(el: ResourceItem): vscode.TreeItem {
    return el;
  }

  getChildren(): ResourceItem[] {
    return this.model ? this.pick(this.model).map((r) => new ResourceItem(r)) : [];
  }
}

export interface RunState {
  active: boolean;
  state: string;
  label: string;
  detail: string;
  host: string;
  running: number;
  pending: number;
  history: string[];
  /** When offline: epoch ms of the next automatic attempt (0 = unknown). */
  retryAt: number;
  transfer?: TransferView;
  activity: ActivityEntry[];
}

export interface RunHandlers {
  run(command: string, background: boolean): Promise<void> | void;
  exec(commandId: string): void;
  open(rel: string): void;
  clearActivity(): void;
}

/** The panel at the top of the sidebar: status, live transfer, actions,
 *  activity and the command box. */
class RunView implements vscode.WebviewViewProvider {
  private view?: vscode.WebviewView;
  private state?: RunState;

  constructor(private readonly handlers: RunHandlers) {}

  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.webview.options = { enableScripts: true };
    view.webview.html = this.html(view.webview);
    view.webview.onDidReceiveMessage(
      (m: { type: string; command?: string; background?: boolean; id?: string; path?: string }) => {
        if (m.type === "run" && m.command?.trim()) void this.handlers.run(m.command.trim(), !!m.background);
        if (m.type === "exec" && m.id) this.handlers.exec(m.id);
        if (m.type === "open" && m.path) this.handlers.open(m.path);
        if (m.type === "clearActivity") this.handlers.clearActivity();
        if (m.type === "ready" && this.state) void view.webview.postMessage({ type: "state", ...this.state });
      },
    );
  }

  update(state: RunState): void {
    this.state = state;
    void this.view?.webview.postMessage({ type: "state", ...state });
  }

  focusInput(): void {
    void this.view?.webview.postMessage({ type: "focus" });
  }

  private html(webview: vscode.Webview): string {
    const nonce = Math.random().toString(36).slice(2) + Date.now().toString(36);
    const mod = process.platform === "darwin" ? "⌘" : "Ctrl+";
    // Small inline icons (codicon-like), so the webview needs no font files.
    const icon = (d: string) =>
      `<svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true"><path fill="currentColor" d="${d}"/></svg>`;
    const I = {
      down: icon("M7.5 1h1v10.3l3.6-3.6.7.7-4.8 4.8-4.8-4.8.7-.7 3.6 3.6V1zM2 14h12v1H2z"),
      up: icon("M8.5 15h-1V4.7L3.9 8.3l-.7-.7L8 2.8l4.8 4.8-.7.7-3.6-3.6V15zM2 1h12v1H2z"),
      files: icon("M3 1h6l4 4v10H3V1zm1 1v12h8V6H8V2H4zm5 .4V5h2.6L9 2.4z"),
      tree: icon("M1 2h5v4H4v2h3v1H4v3h3v1H3V6H1V2zm1 1v2h3V3H2zm6 4h7v3H8V7zm1 1v1h5V8H9zm-1 3h7v3H8v-3zm1 1v1h5v-1H9z"),
      play: icon("M4 2.5v11l9-5.5-9-5.5z"),
      term: icon("M1 3h14v10H1V3zm1 1v8h12V4H2zm1.6 1.6L6 8l-2.4 2.4-.7-.7L4.6 8 2.9 6.3l.7-.7zM7 10h5v1H7v-1z"),
      bg: icon("M8 1a7 7 0 1 1 0 14A7 7 0 0 1 8 1zm0 1a6 6 0 1 0 0 12A6 6 0 0 0 8 2zm-.5 2h1v4.3l2.9 1.7-.5.9-3.4-2V4z"),
      retry: icon("M13.5 2v4h-4V5h2.2A5 5 0 1 0 13 8h1a6 6 0 1 1-1.5-4V2h1z"),
    };
    return /* html */ `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}';">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<style>
  :root { color-scheme: light dark; --gap: 8px; --radius: 4px;
    --ok: var(--vscode-testing-iconPassed, #3fb950); --warn: var(--vscode-problemsWarningIcon-foreground, #d29922);
    --err: var(--vscode-errorForeground, #f14c4c); --busy: var(--vscode-charts-blue, #3794ff);
    --muted: var(--vscode-descriptionForeground); --card: var(--vscode-sideBarSectionHeader-background, rgba(128,128,128,.08));
    --line: var(--vscode-widget-border, rgba(128,128,128,.22)); }
  body { padding: 10px 12px 14px; margin: 0; font-family: var(--vscode-font-family); font-size: var(--vscode-font-size); color: var(--vscode-foreground); }
  svg { flex: none; }
  .card { background: var(--card); border: 1px solid var(--line); border-radius: var(--radius); padding: 8px 10px; margin-bottom: 10px; }
  .status { display: flex; align-items: center; gap: 8px; }
  .dot { width: 9px; height: 9px; border-radius: 50%; background: var(--ok); flex: none; box-shadow: 0 0 0 3px color-mix(in srgb, currentColor 0%, transparent); }
  .dot.offline, .dot.choose { background: var(--warn); }
  .dot.error { background: var(--err); }
  .dot.syncing, .dot.pending { background: var(--busy); }
  .dot.syncing { animation: pulse 1.2s ease-in-out infinite; }
  .dot.stopped { background: var(--vscode-disabledForeground, #888); }
  @keyframes pulse { 50% { opacity: .35; } }
  .status .text { flex: 1; min-width: 0; }
  .status .label { font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .status .host { color: var(--muted); font-size: 11px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .chips { display: flex; gap: 6px; margin-top: 6px; flex-wrap: wrap; }
  .chip { font-size: 11px; padding: 1px 7px; border-radius: 10px; background: var(--vscode-badge-background); color: var(--vscode-badge-foreground); cursor: pointer; }
  .chip:empty { display: none; }
  .offline-row { display: none; margin-top: 8px; gap: 8px; align-items: center; }
  .is-offline .offline-row { display: flex; }
  .offline-row .why { flex: 1; color: var(--muted); font-size: 11px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  .transfer { display: none; }
  .transfer.on { display: block; }
  .t-head { display: flex; align-items: center; gap: 6px; font-weight: 600; }
  .t-head .pct { margin-left: auto; font-variant-numeric: tabular-nums; }
  .t-head.up { color: var(--busy); } .t-head.down { color: var(--ok); }
  .meter { height: 6px; border-radius: 3px; background: var(--vscode-progressBar-background, var(--busy)); background: color-mix(in srgb, var(--busy) 18%, transparent); overflow: hidden; margin: 7px 0 6px; position: relative; }
  .meter > div { height: 100%; width: 0; background: var(--vscode-progressBar-background, var(--busy)); border-radius: 3px; transition: width .2s ease; }
  .meter.indeterminate > div { width: 30%; position: absolute; animation: slide 1.2s ease-in-out infinite; }
  @keyframes slide { from { left: -30%; } to { left: 100%; } }
  .t-stats { display: flex; justify-content: space-between; gap: 8px; color: var(--muted); font-size: 11px; font-variant-numeric: tabular-nums; flex-wrap: wrap; }
  .t-file { margin-top: 4px; font-family: var(--vscode-editor-font-family); font-size: 11px; color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; direction: rtl; text-align: left; }

  textarea { width: 100%; box-sizing: border-box; resize: vertical; min-height: 30px; padding: 6px 8px;
    font-family: var(--vscode-editor-font-family); font-size: var(--vscode-editor-font-size);
    color: var(--vscode-input-foreground); background: var(--vscode-input-background);
    border: 1px solid var(--vscode-input-border, transparent); border-radius: 2px; outline: none; }
  textarea:focus { border-color: var(--vscode-focusBorder); }
  textarea::placeholder { color: var(--vscode-input-placeholderForeground); }
  button { font-family: inherit; font-size: inherit; cursor: pointer; border: 1px solid var(--vscode-button-border, transparent); border-radius: 2px; padding: 5px 10px;
    display: inline-flex; align-items: center; justify-content: center; gap: 6px; }
  button:focus-visible { outline: 1px solid var(--vscode-focusBorder); outline-offset: 1px; }
  .primary { width: 100%; margin-top: 8px; color: var(--vscode-button-foreground); background: var(--vscode-button-background); }
  .primary:hover { background: var(--vscode-button-hoverBackground); }
  .secondary { color: var(--vscode-button-secondaryForeground); background: var(--vscode-button-secondaryBackground); }
  .secondary:hover { background: var(--vscode-button-secondaryHoverBackground); }
  .small { padding: 2px 8px; font-size: 11px; }
  .row { display: flex; gap: 6px; margin-top: 6px; } .row > button { flex: 1; }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 6px; }
  .grid button { justify-content: flex-start; white-space: nowrap; overflow: hidden; }
  .hint { margin-top: 6px; color: var(--muted); font-size: 11px; }
  h3 { display: flex; align-items: center; font-size: 11px; font-weight: 600; text-transform: uppercase; letter-spacing: .04em; color: var(--muted); margin: 16px 0 6px; }
  h3 .act { margin-left: auto; text-transform: none; letter-spacing: 0; font-weight: normal; cursor: pointer; color: var(--vscode-textLink-foreground); }
  ul { list-style: none; margin: 0; padding: 0; }
  li { display: flex; align-items: center; gap: 6px; padding: 2px 4px; border-radius: 3px; cursor: pointer; min-height: 20px; }
  li:hover { background: var(--vscode-list-hoverBackground); }
  li .cmd, li .path { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  li .cmd { font-family: var(--vscode-editor-font-family); }
  li .path .dir { color: var(--muted); margin-left: 4px; font-size: 11px; }
  li .meta { color: var(--muted); font-size: 11px; flex: none; font-variant-numeric: tabular-nums; }
  li .go { opacity: 0; color: var(--vscode-textLink-foreground); padding: 0 4px; }
  li:hover .go { opacity: 1; }
  .k { width: 14px; text-align: center; flex: none; font-weight: 700; }
  .k.up { color: var(--busy); } .k.down { color: var(--ok); } .k.deleted { color: var(--err); } .k.conflict { color: var(--warn); }
  .none { color: var(--muted); font-size: 11px; padding: 2px 4px; }
  .empty { display: none; }
  .inactive .active-only { display: none; }
  .inactive .empty { display: block; }
  .empty p { color: var(--muted); line-height: 1.5; }
</style>
</head>
<body class="inactive">
  <div class="empty">
    <p>Run this project's code on a remote machine. Connect the folder to a remote to start.</p>
    <button class="primary" data-exec="devsync.init">Connect to Remote…</button>
    <button class="primary secondary" data-exec="devsync.walkthrough">Getting Started</button>
  </div>
  <div class="active-only">
    <div class="card" id="status-card">
      <div class="status">
        <span class="dot" id="dot"></span>
        <div class="text"><div class="label" id="label">devsync</div><div class="host" id="host"></div></div>
      </div>
      <div class="chips"><span class="chip" id="chip-pending" data-exec="devsync.changes.focus" title="Show files not on the remote yet"></span><span class="chip" id="chip-running" data-exec="devsync.jobs.focus" title="Show remote jobs"></span></div>
      <div class="offline-row">
        <span class="why" id="why"></span>
        <button class="secondary small" data-exec="devsync.retry" title="Try to reach the remote now">${I.retry}Retry now</button>
      </div>
    </div>

    <div class="card transfer" id="transfer">
      <div class="t-head" id="t-head"><span id="t-icon"></span><span id="t-title"></span><span class="pct" id="t-pct"></span></div>
      <div class="meter" id="meter"><div id="fill"></div></div>
      <div class="t-stats"><span id="t-files"></span><span id="t-bytes"></span><span id="t-rate"></span></div>
      <div class="t-file" id="t-file"></div>
    </div>

    <div class="grid">
      <button class="secondary" data-exec="devsync.pull" title="Get all of the remote's changes; files you edited are never overwritten">${I.down}Pull All</button>
      <button class="secondary" data-exec="devsync.push" title="Send local changes now">${I.up}Push</button>
      <button class="secondary" data-exec="devsync.pullPick" title="Choose remote files or folders to pull">${I.files}Pull Files…</button>
      <button class="secondary" data-exec="devsync.chooseFiles" title="Browse the remote project and choose which folders to sync">${I.tree}Choose Sync…</button>
    </div>

    <h3>Run on remote</h3>
    <textarea id="cmd" rows="1" placeholder="e.g. pytest -x" aria-label="Command to run on the remote"></textarea>
    <button class="primary" id="run" title="Run on the remote (Enter)">${I.play}Run on Remote</button>
    <div class="row">
      <button class="secondary" id="bg" title="Start without waiting (${mod}Enter); output fills in as it runs">${I.bg}Background</button>
      <button class="secondary" data-exec="devsync.shell" title="Interactive shell on the remote">${I.term}Terminal</button>
    </div>
    <div class="hint">Runs in the remote folder of the open file · Enter run · ${mod}Enter background</div>

    <div id="activity-wrap"><h3>Activity <span class="act" id="clear" title="Clear the list">Clear</span></h3><ul id="activity"></ul></div>
    <div id="recent-wrap"><h3>Recent commands</h3><ul id="recent"></ul></div>
  </div>
<script nonce="${nonce}">
  const vscode = acquireVsCodeApi();
  const $ = (id) => document.getElementById(id);
  const cmd = $("cmd");
  const prev = vscode.getState();
  if (prev && prev.draft) cmd.value = prev.draft;
  const run = (background) => {
    const c = cmd.value.trim();
    if (!c) { cmd.focus(); return; }
    vscode.postMessage({ type: "run", command: c, background });
    cmd.value = ""; vscode.setState({ draft: "" });
  };
  $("run").addEventListener("click", () => run(false));
  $("bg").addEventListener("click", () => run(true));
  $("clear").addEventListener("click", () => vscode.postMessage({ type: "clearActivity" }));
  document.querySelectorAll("[data-exec]").forEach((b) =>
    b.addEventListener("click", () => vscode.postMessage({ type: "exec", id: b.dataset.exec })));
  cmd.addEventListener("input", () => vscode.setState({ draft: cmd.value }));
  cmd.addEventListener("keydown", (e) => {
    if (e.key !== "Enter" || e.shiftKey) return;
    e.preventDefault();
    run(e.metaKey || e.ctrlKey);
  });
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
  const fmt = (n) => { const u = ["B","KB","MB","GB","TB"]; let v = n, i = 0; while (v >= 1024 && i < 4) { v /= 1024; i++; } return i ? v.toFixed(1) + " " + u[i] : n + " B"; };
  const dur = (s) => s < 60 ? Math.ceil(s) + "s" : s < 3600 ? Math.floor(s / 60) + "m " + Math.floor(s % 60) + "s" : Math.floor(s / 3600) + "h " + Math.floor((s % 3600) / 60) + "m";
  const ago = (t) => { const s = Math.max(0, (Date.now() - t) / 1000); return s < 5 ? "now" : s < 60 ? Math.floor(s) + "s" : s < 3600 ? Math.floor(s / 60) + "m" : Math.floor(s / 3600) + "h"; };
  let last = null;

  function renderStatus(m) {
    $("dot").className = "dot " + m.state;
    $("label").textContent = m.label;
    $("label").title = m.detail || "";
    $("host").textContent = m.host; $("host").title = m.host;
    $("chip-pending").textContent = m.pending ? m.pending + " not on remote" : "";
    $("chip-running").textContent = m.running ? m.running + " running" : "";
    document.body.classList.toggle("is-offline", m.state === "offline" || m.state === "error");
    const secs = m.retryAt ? Math.max(0, Math.round((m.retryAt - Date.now()) / 1000)) : 0;
    $("why").textContent = m.state === "offline" ? (secs > 0 ? "Next try in " + secs + "s" : "Retrying…") : (m.detail || "");
    $("why").title = m.detail || "";
  }

  function renderTransfer(t) {
    const box = $("transfer");
    box.classList.toggle("on", !!t);
    if (!t) return;
    const moving = t.direction !== "none";
    const done = t.stage === "done";
    $("t-head").className = "t-head " + t.direction;
    $("t-icon").innerHTML = t.direction === "up" ? '${I.up}' : t.direction === "down" ? '${I.down}' : "";
    $("t-title").textContent = !moving ? (t.stage || "working") + "…" : done ? (t.direction === "up" ? "Pushed" : "Pulled") : t.direction === "up" ? "Pushing to remote" : "Pulling from remote";
    const pct = t.bytesTotal > 0 ? (100 * Math.min(t.bytesDone, t.bytesTotal)) / t.bytesTotal : t.filesTotal > 0 ? (100 * t.filesDone) / t.filesTotal : 0;
    $("t-pct").textContent = moving ? Math.floor(pct) + "%" : "";
    $("meter").classList.toggle("indeterminate", !moving);
    $("fill").style.width = moving ? pct + "%" : "";
    $("t-files").textContent = moving ? t.filesDone + " / " + t.filesTotal + " files here" : "";
    $("t-files").title = t.already ? t.already + " were already here; " + (t.filesTotal - t.already) + " to " + (t.direction === "up" ? "send" : "download") : "";
    $("t-bytes").textContent = moving ? fmt(Math.min(t.bytesDone, t.bytesTotal)) + " / " + fmt(t.bytesTotal) : "";
    $("t-rate").textContent = moving && !done && t.rate > 0 ? fmt(t.rate) + "/s" + (t.eta >= 0 ? " · " + dur(t.eta) + " left" : "") : "";
    // Big files get their own progress, so the card never looks stuck.
    $("t-file").textContent = moving && !done ? t.path + (t.fileSize >= 1048576 ? "  " + fmt(Math.min(t.fileDone, t.fileSize)) + " / " + fmt(t.fileSize) : "") : "";
    $("t-file").title = t.path;
  }

  function renderActivity(list) {
    const ul = $("activity");
    const marks = { up: "↑", down: "↓", deleted: "✕", conflict: "!" };
    const words = { up: "pushed", down: "pulled", deleted: "deleted on remote", conflict: "conflict — remote copy saved alongside" };
    ul.innerHTML = list.length ? list.slice(0, 50).map((a, i) => {
      const slash = a.path.lastIndexOf("/");
      const name = slash >= 0 ? a.path.slice(slash + 1) : a.path;
      const dir = slash >= 0 ? a.path.slice(0, slash) : "";
      return '<li data-i="' + i + '" title="' + esc(a.path + " — " + words[a.kind]) + '"><span class="k ' + a.kind + '">' + marks[a.kind] + '</span>' +
        '<span class="path">' + esc(name) + (dir ? '<span class="dir">' + esc(dir) + '</span>' : "") + '</span>' +
        '<span class="meta">' + (a.bytes ? fmt(a.bytes) + " · " : "") + ago(a.at) + '</span></li>';
    }).join("") : '<div class="none">Files pushed or pulled appear here.</div>';
    ul.querySelectorAll("li").forEach((li) => li.addEventListener("click", () => {
      const a = list[Number(li.dataset.i)];
      if (a.kind !== "deleted") vscode.postMessage({ type: "open", path: a.path });
    }));
  }

  function renderRecent(history) {
    const list = $("recent");
    $("recent-wrap").style.display = history.length ? "" : "none";
    list.innerHTML = history.map((h, i) =>
      '<li data-i="' + i + '" title="Click to edit, ▶ to run"><span class="cmd">' + esc(h) + '</span><span class="go" data-run="' + i + '">▶</span></li>').join("");
    list.querySelectorAll("li").forEach((li) => li.addEventListener("click", (e) => {
      const h = history[Number(li.dataset.i)];
      if (e.target.dataset.run !== undefined) { vscode.postMessage({ type: "run", command: h, background: false }); return; }
      cmd.value = h; vscode.setState({ draft: h }); cmd.focus();
    }));
  }

  window.addEventListener("message", (ev) => {
    const m = ev.data;
    if (m.type === "focus") { cmd.focus(); return; }
    if (m.type !== "state") return;
    const prevM = last;
    last = m;
    document.body.classList.toggle("inactive", !m.active);
    renderStatus(m);
    renderTransfer(m.transfer);
    if (!prevM || prevM.activity !== m.activity) renderActivity(m.activity);
    if (!prevM || JSON.stringify(prevM.history) !== JSON.stringify(m.history)) renderRecent(m.history);
  });
  // Countdown and "x s ago" stay current between updates.
  setInterval(() => { if (last) { renderStatus(last); renderActivity(last.activity); } }, 1000);
  vscode.postMessage({ type: "ready" });
</script>
</body>
</html>`;
  }
}

export class Sidebar implements vscode.Disposable {
  private readonly changes = new ListProvider((m) => m.changes);
  private readonly conflicts = new ListProvider((m) => m.conflicts);
  private readonly jobs = new ListProvider((m) => m.jobs);
  private readonly changesView: vscode.TreeView<ResourceItem>;
  private readonly jobsView: vscode.TreeView<ResourceItem>;
  private readonly runView: RunView;
  private readonly disposables: vscode.Disposable[] = [];
  private modelSub?: vscode.Disposable;

  constructor(handlers: RunHandlers) {
    this.runView = new RunView(handlers);
    this.changesView = vscode.window.createTreeView("devsync.changes", { treeDataProvider: this.changes });
    this.jobsView = vscode.window.createTreeView("devsync.jobs", { treeDataProvider: this.jobs });
    this.disposables.push(
      this.changesView,
      this.jobsView,
      vscode.window.createTreeView("devsync.conflicts", { treeDataProvider: this.conflicts }),
      vscode.window.registerWebviewViewProvider("devsync.run", this.runView, {
        webviewOptions: { retainContextWhenHidden: true },
      }),
    );
  }

  setModel(model: Model): void {
    this.modelSub?.dispose();
    for (const p of [this.changes, this.conflicts, this.jobs]) p.model = model;
    this.modelSub = model.onDidChange(() => this.render(model));
    this.render(model);
  }

  private render(model: Model): void {
    this.changes.refresh();
    this.conflicts.refresh();
    this.jobs.refresh();
    const waiting = model.pendingChanges + model.conflicts.length;
    this.changesView.badge = waiting ? { value: waiting, tooltip: `${waiting} change(s) not on the remote yet` } : undefined;
    this.changesView.message = model.pendingChanges === 0 ? "Everything is on the remote." : undefined;
    this.jobsView.badge = model.runningJobs
      ? { value: model.runningJobs, tooltip: `${model.runningJobs} remote job(s) running` }
      : undefined;
  }

  updateRun(state: RunState): void {
    this.runView.update(state);
  }

  focusInput(): void {
    this.runView.focusInput();
  }

  dispose(): void {
    this.modelSub?.dispose();
    for (const d of this.disposables) d.dispose();
  }
}
