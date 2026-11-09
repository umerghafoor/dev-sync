// The devsync sidebar in the Activity Bar:
//
//   RUN                 command box, big "Run on remote" button, status, recent commands
//   NOT ON REMOTE YET   A/M/D files waiting to be sent
//   CONFLICTS           (only when there are some)
//   REMOTE JOBS         ✓ / ✗ / … with open, stop, run again

import * as vscode from "vscode";
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
}

export interface RunHandlers {
  run(command: string, background: boolean): Promise<void> | void;
  exec(commandId: string): void;
}

/** The "Run" box at the top of the sidebar. */
class RunView implements vscode.WebviewViewProvider {
  private view?: vscode.WebviewView;
  private state?: RunState;

  constructor(private readonly handlers: RunHandlers) {}

  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.webview.options = { enableScripts: true };
    view.webview.html = this.html(view.webview);
    view.webview.onDidReceiveMessage((m: { type: string; command?: string; background?: boolean; id?: string }) => {
      if (m.type === "run" && m.command?.trim()) void this.handlers.run(m.command.trim(), !!m.background);
      if (m.type === "exec" && m.id) this.handlers.exec(m.id);
      if (m.type === "ready" && this.state) void view.webview.postMessage({ type: "state", ...this.state });
    });
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
    return /* html */ `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}';">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<style>
  :root { color-scheme: light dark; }
  body { padding: 8px 12px 12px; margin: 0; font-family: var(--vscode-font-family); font-size: var(--vscode-font-size); color: var(--vscode-foreground); }
  .status { display: flex; align-items: center; gap: 6px; margin: 2px 0 10px; color: var(--vscode-descriptionForeground); }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--vscode-testing-iconPassed, #3fb950); flex: none; }
  .dot.offline, .dot.error { background: var(--vscode-problemsWarningIcon-foreground, #d29922); }
  .dot.error { background: var(--vscode-errorForeground, #f14c4c); }
  .dot.syncing, .dot.pending { background: var(--vscode-charts-blue, #3794ff); }
  .dot.stopped { background: var(--vscode-disabledForeground, #888); }
  .status .host { margin-left: auto; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 55%; }
  textarea { width: 100%; box-sizing: border-box; resize: vertical; min-height: 30px; padding: 6px 8px;
    font-family: var(--vscode-editor-font-family); font-size: var(--vscode-editor-font-size);
    color: var(--vscode-input-foreground); background: var(--vscode-input-background);
    border: 1px solid var(--vscode-input-border, transparent); border-radius: 2px; outline: none; }
  textarea:focus { border-color: var(--vscode-focusBorder); }
  textarea::placeholder { color: var(--vscode-input-placeholderForeground); }
  button { font-family: inherit; font-size: inherit; cursor: pointer; border: 1px solid var(--vscode-button-border, transparent); border-radius: 2px; padding: 5px 10px; }
  .primary { display: block; width: 100%; margin-top: 8px; color: var(--vscode-button-foreground); background: var(--vscode-button-background); }
  .primary:hover { background: var(--vscode-button-hoverBackground); }
  .row { display: flex; gap: 6px; margin-top: 6px; }
  .secondary { flex: 1; color: var(--vscode-button-secondaryForeground); background: var(--vscode-button-secondaryBackground); }
  .secondary:hover { background: var(--vscode-button-secondaryHoverBackground); }
  .hint { margin-top: 6px; color: var(--vscode-descriptionForeground); font-size: 11px; }
  h3 { font-size: 11px; font-weight: 600; text-transform: uppercase; letter-spacing: .04em; color: var(--vscode-descriptionForeground); margin: 16px 0 4px; }
  ul { list-style: none; margin: 0; padding: 0; }
  li { display: flex; align-items: center; gap: 6px; padding: 2px 4px; border-radius: 3px; cursor: pointer; }
  li:hover { background: var(--vscode-list-hoverBackground); }
  li .cmd { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: var(--vscode-editor-font-family); }
  li .go { opacity: 0; color: var(--vscode-textLink-foreground); padding: 0 4px; }
  li:hover .go { opacity: 1; }
  .empty { display: none; }
  .inactive .active-only { display: none; }
  .inactive .empty { display: block; }
  .empty p { color: var(--vscode-descriptionForeground); line-height: 1.5; }
</style>
</head>
<body class="inactive">
  <div class="empty">
    <p>Run this project's code on a remote machine. Connect the folder to a remote to start.</p>
    <button class="primary" data-exec="devsync.init">Connect to Remote…</button>
    <button class="primary" style="background: var(--vscode-button-secondaryBackground); color: var(--vscode-button-secondaryForeground)" data-exec="devsync.walkthrough">Getting Started</button>
  </div>
  <div class="active-only">
    <div class="status"><span class="dot" id="dot"></span><span id="label">devsync</span><span class="host" id="host"></span></div>
    <textarea id="cmd" rows="1" placeholder="Command to run on the remote, e.g. pytest -x" aria-label="Command to run on the remote"></textarea>
    <button class="primary" id="run" title="Run on the remote (Enter)">▶ Run on Remote</button>
    <div class="row">
      <button class="secondary" id="bg" title="Start without waiting (${mod}Enter); output fills in as it runs">Run in Background</button>
      <button class="secondary" data-exec="devsync.shell" title="Interactive shell on the remote">Terminal</button>
    </div>
    <div class="hint">Runs in the remote folder of the file you have open. Enter: run · ${mod}Enter: background · Shift+Enter: new line</div>
    <div id="recent-wrap"><h3>Recent</h3><ul id="recent"></ul></div>
  </div>
<script nonce="${nonce}">
  const vscode = acquireVsCodeApi();
  const cmd = document.getElementById("cmd");
  const prev = vscode.getState();
  if (prev && prev.draft) cmd.value = prev.draft;
  const run = (background) => {
    const c = cmd.value.trim();
    if (!c) { cmd.focus(); return; }
    vscode.postMessage({ type: "run", command: c, background });
    cmd.value = ""; vscode.setState({ draft: "" });
  };
  document.getElementById("run").addEventListener("click", () => run(false));
  document.getElementById("bg").addEventListener("click", () => run(true));
  document.querySelectorAll("[data-exec]").forEach((b) =>
    b.addEventListener("click", () => vscode.postMessage({ type: "exec", id: b.dataset.exec })));
  cmd.addEventListener("input", () => vscode.setState({ draft: cmd.value }));
  cmd.addEventListener("keydown", (e) => {
    if (e.key !== "Enter" || e.shiftKey) return;
    e.preventDefault();
    run(e.metaKey || e.ctrlKey);
  });
  const esc = (s) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
  window.addEventListener("message", (ev) => {
    const m = ev.data;
    if (m.type === "focus") { cmd.focus(); return; }
    if (m.type !== "state") return;
    document.body.classList.toggle("inactive", !m.active);
    document.getElementById("dot").className = "dot " + m.state;
    const extra = [m.pending ? m.pending + " not sent" : "", m.running ? m.running + " running" : ""].filter(Boolean).join(" · ");
    document.getElementById("label").textContent = m.label + (extra ? " · " + extra : "");
    document.getElementById("label").title = m.detail || "";
    document.getElementById("host").textContent = m.host;
    document.getElementById("host").title = m.host;
    const list = document.getElementById("recent");
    document.getElementById("recent-wrap").style.display = m.history.length ? "" : "none";
    list.innerHTML = m.history.map((h, i) =>
      '<li data-i="' + i + '" title="Click to edit, ▶ to run"><span class="cmd">' + esc(h) + '</span><span class="go" data-run="' + i + '">▶</span></li>').join("");
    list.querySelectorAll("li").forEach((li) => li.addEventListener("click", (e) => {
      const h = m.history[Number(li.dataset.i)];
      if (e.target.dataset.run !== undefined) { vscode.postMessage({ type: "run", command: h, background: false }); return; }
      cmd.value = h; vscode.setState({ draft: h }); cmd.focus();
    }));
  });
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
