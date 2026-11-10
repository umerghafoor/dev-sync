// "Graph (remote)": the remote repository's history, drawn like VS Code's
// own Graph view (which only works for local repositories): colored lanes
// (outgoing commits in the branch color, the upstream in the remote color),
// ⊚ on HEAD, ref badges, an Outgoing/Incoming Changes row, a hover card with
// the full message and stats, and commit actions on right-click. Clicking a
// commit opens all its changes in one multi-file diff, with file contents
// served from the local git-object cache (fetched in one round trip).

import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { run, runJson } from "./cli";
import { gitUri, objectPath } from "./remoteDocs";
import { RemoteGit } from "./remoteGit";

interface GitResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

export interface Commit {
  sha: string;
  parents: string[];
  refs: string[];
  author: string;
  date: number;
  subject: string;
}

/** "ref" (current branch), "remote" (upstream) or "lane<n>" (other lanes). */
export type Color = string;
export type Seg = [number, number, Color];

export interface Row {
  commit: Commit;
  col: number;
  color: Color;
  /** Upper half: lines arriving at this row. */
  top: Seg[];
  /** Lower half: lines leaving this row. */
  bottom: Seg[];
  width: number;
}

const SEP = "\x1f";

export function parseLog(out: string): Commit[] {
  return out
    .split("\x1e")
    .map((r) => r.replace(/^\n/, ""))
    .filter(Boolean)
    .map((r) => {
      const [sha, parents, refs, author, date, subject] = r.split(SEP);
      return {
        sha,
        parents: parents ? parents.split(" ").filter(Boolean) : [],
        refs: refs
          ? refs
              .split(", ")
              .map((x) => x.trim())
              .filter((x) => x && !x.endsWith("/HEAD"))
          : [],
        author,
        date: Number(date) || 0,
        subject: (subject ?? "").trim(),
      };
    });
}

/** Commits reachable from `start` within the loaded list. */
export function reachable(commits: Commit[], start?: string): Set<string> {
  const byId = new Map(commits.map((c) => [c.sha, c]));
  const seen = new Set<string>();
  const stack = start ? [start] : [];
  while (stack.length) {
    const s = stack.pop()!;
    if (seen.has(s) || !byId.has(s)) continue;
    seen.add(s);
    stack.push(...byId.get(s)!.parents);
  }
  return seen;
}

/** Lanes like `git log --graph`; each line keeps the color of the commit it leaves. */
export function layout(commits: Commit[], colorOf: (c: Commit, lane: number) => Color = (_, l) => `lane${l}`): Row[] {
  const lanes: (string | null)[] = [];
  const laneColor: Color[] = [];
  const rows: Row[] = [];
  const free = () => {
    const i = lanes.indexOf(null);
    if (i >= 0) return i;
    lanes.push(null);
    return lanes.length - 1;
  };
  for (const c of commits) {
    const expected = lanes.indexOf(c.sha);
    const col = expected >= 0 ? expected : free();
    lanes[col] = c.sha;
    const color = colorOf(c, col);
    const before = [...lanes];
    const top: Seg[] = [];
    before.forEach((h, i) => {
      if (h === null) return;
      if (h === c.sha) {
        if (i !== col || expected >= 0) top.push([i, col, laneColor[i] ?? color]);
        lanes[i] = null;
      } else {
        top.push([i, i, laneColor[i]]);
      }
    });
    const bottom: Seg[] = [];
    c.parents.forEach((p, k) => {
      let j = lanes.indexOf(p);
      // The first parent stays in this lane when a lane further right waits
      // for it too; the lanes meet at the parent (like git's "|/").
      if (j < 0 || (k === 0 && j > col && lanes[col] === null)) {
        j = lanes[col] === null ? col : free();
        lanes[j] = p;
        laneColor[j] = color;
      }
      bottom.push([col, j, color]);
    });
    before.forEach((h, i) => {
      if (i !== col && h !== null && h !== c.sha && lanes[i] === h) bottom.push([i, i, laneColor[i]]);
    });
    while (lanes.length && lanes[lanes.length - 1] === null) {
      lanes.pop();
      laneColor.length = lanes.length;
    }
    const width = Math.max(col + 1, ...top.map(([a, b]) => Math.max(a, b) + 1), ...bottom.map(([a, b]) => Math.max(a, b) + 1));
    rows.push({ commit: c, col, color, top, bottom, width });
  }
  return rows;
}

interface Details {
  author: string;
  email: string;
  date: number;
  message: string;
  coauthors: string[];
  stats: string;
  web?: string;
}

export class RemoteGraph implements vscode.WebviewViewProvider, vscode.Disposable {
  private view?: vscode.WebviewView;
  private rows: Row[] = [];
  private loading = false;
  private error = "";
  private webBase?: string | null;
  private readonly details = new Map<string, Details>();
  private readonly disposables: vscode.Disposable[] = [];

  constructor(
    private readonly root: string,
    private readonly git: RemoteGit,
    private readonly log: vscode.OutputChannel,
  ) {
    this.disposables.push(
      vscode.window.registerWebviewViewProvider("devsync.gitGraph", this, { webviewOptions: { retainContextWhenHidden: true } }),
      git.onDidChangeHead(() => void this.load()),
      vscode.commands.registerCommand("devsync.git.graphRefresh", () => this.load()),
      vscode.commands.registerCommand("devsync.git.graphGoToCurrent", () =>
        this.view?.webview.postMessage({ type: "reveal", sha: this.git.head }),
      ),
      vscode.commands.registerCommand("devsync.git.showCommit", (sha: string) => this.openCommit(sha)),
    );
  }

  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.webview.options = { enableScripts: true };
    view.webview.html = this.html(view.webview);
    view.webview.onDidReceiveMessage((m: { type: string; sha?: string; which?: string }) => {
      if (m.type === "ready") {
        this.post();
        if (!this.rows.length) void this.load();
      }
      if (m.type === "open" && m.sha) void this.openCommit(m.sha);
      if (m.type === "openRange" && m.which) void this.openRange(m.which as "outgoing" | "incoming");
      if (m.type === "actions" && m.sha) {
        const c = this.rows.find((r) => r.commit.sha === m.sha)?.commit;
        void vscode.commands.executeCommand("devsync.git.commitActions", m.sha, c?.subject ?? "");
      }
      if (m.type === "copy" && m.sha) void vscode.env.clipboard.writeText(m.sha);
      if (m.type === "details" && m.sha) void this.sendDetails(m.sha);
      if (m.type === "web" && m.sha) void this.openWeb(m.sha);
    });
    view.onDidChangeVisibility(() => view.visible && this.post());
  }

  async load(): Promise<void> {
    if (this.loading) return;
    this.loading = true;
    this.post();
    try {
      // The current branch plus its upstream, like VS Code's "Auto" graph.
      const upstream = this.git.upstreamName;
      const revs = ["HEAD", ...(upstream ? [upstream] : [])];
      const r = await runJson<GitResult>(
        this.root,
        [
          "git",
          "--no-push",
          "--",
          "log",
          "--topo-order",
          "-n",
          "300",
          `--format=%x1e%H${SEP}%P${SEP}%D${SEP}%an${SEP}%ct${SEP}%s`,
          ...revs,
          "--",
        ],
        120_000,
      );
      if (r.code !== 0) throw new Error((r.stderr || r.stdout).trim().split("\n").pop() || `git log exited with ${r.code}`);
      const commits = parseLog(r.stdout);
      const upTip = commits.find((c) => c.refs.includes(upstream))?.sha;
      const fromUpstream = reachable(commits, upTip);
      const fromHead = reachable(commits, this.git.head || commits[0]?.sha);
      this.rows = layout(commits, (c, lane) =>
        fromUpstream.has(c.sha) ? "remote" : fromHead.has(c.sha) ? "ref" : `lane${lane % 5}`,
      );
      this.error = "";
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
      this.log.appendLine(`graph (remote): ${this.error}`);
    } finally {
      this.loading = false;
      this.post();
    }
  }

  private post(): void {
    const [ahead, behind] = this.git.aheadBehind;
    void this.view?.webview.postMessage({
      type: "data",
      rows: this.rows,
      head: this.git.head,
      branch: this.git.branchName,
      upstream: this.git.upstreamName,
      ahead,
      behind,
      loading: this.loading,
      error: this.error,
    });
  }

  private async gitOut(args: string[]): Promise<string> {
    const r = await runJson<GitResult>(this.root, ["git", "--no-push", "--", ...args], 120_000);
    if (r.code !== 0) throw new Error(r.stderr.trim().split("\n").pop() || `git ${args[0]} failed`);
    return r.stdout;
  }

  /** https://github.com/owner/repo (or other forges) from the origin URL. */
  private async web(): Promise<string | undefined> {
    if (this.webBase === undefined) {
      try {
        const url = (await this.gitOut(["remote", "get-url", "origin"])).trim();
        const m = /^(?:git@|ssh:\/\/git@|https?:\/\/)([^/:]+)[/:](.+?)(?:\.git)?\/?$/.exec(url);
        this.webBase = m ? `https://${m[1]}/${m[2]}` : null;
      } catch {
        this.webBase = null;
      }
    }
    return this.webBase ?? undefined;
  }

  private async sendDetails(sha: string): Promise<void> {
    let d = this.details.get(sha);
    if (!d) {
      try {
        const out = await this.gitOut(["show", "--shortstat", `--format=%an${SEP}%ae${SEP}%at${SEP}%B%x1e`, sha]);
        const [head, stats = ""] = out.split("\x1e");
        const [author, email, at, ...msg] = head.split(SEP);
        const message = msg.join(SEP).trim();
        const coauthors = [...message.matchAll(/^Co-Authored-By:\s*(.+?)\s*<[^>]*>\s*$/gim)].map((m) => m[1]);
        d = { author, email, date: Number(at), message, coauthors, stats: stats.trim(), web: await this.web() };
        this.details.set(sha, d);
      } catch (e) {
        this.log.appendLine(`graph details: ${e instanceof Error ? e.message : String(e)}`);
        return;
      }
    }
    void this.view?.webview.postMessage({ type: "details", sha, details: d });
  }

  private async openWeb(sha: string): Promise<void> {
    const base = await this.web();
    if (base) await vscode.env.openExternal(vscode.Uri.parse(`${base}/commit/${sha}`));
  }

  /** All changes of a commit in one multi-file diff editor. */
  private openCommit(sha: string): Promise<void> {
    const c = this.rows.find((r) => r.commit.sha === sha)?.commit;
    return this.openDiff(
      ["diff-tree", "-r", "-M", "--root", "--no-commit-id", "-z", sha],
      `${sha.slice(0, 7)} ${c?.subject ?? ""} (remote)`,
    );
  }

  /** Unpushed (outgoing) or not-yet-pulled (incoming) changes. */
  private openRange(which: "outgoing" | "incoming"): Promise<void> {
    const up = this.git.upstreamName || "@{u}";
    return which === "outgoing"
      ? this.openDiff(["diff", "--raw", "-z", "-M", `${up}...HEAD`], `Outgoing Changes (${this.git.branchName}, remote)`)
      : this.openDiff(["diff", "--raw", "-z", "-M", `HEAD...${up}`], `Incoming Changes (${up}, remote)`);
  }

  private async openDiff(args: string[], title: string): Promise<void> {
    await vscode.window.withProgress({ location: { viewId: "devsync.gitGraph" }, title: "Loading changes" }, async () => {
      // :old_mode new_mode old_id new_id status \0 path [\0 new_path]
      const parts = (await this.gitOut(args)).split("\0");
      const files: { path: string; from?: string; before?: string; after?: string }[] = [];
      const id = (x: string) => (/^0+$/.test(x) ? undefined : x);
      for (let i = 0; i < parts.length; i++) {
        const meta = parts[i];
        if (!meta.startsWith(":")) continue;
        const [, , oldId, newId, status] = meta.slice(1).split(" ");
        if (status.startsWith("R") || status.startsWith("C")) {
          const from = parts[++i];
          files.push({ path: parts[++i], from, before: id(oldId), after: id(newId) });
        } else {
          files.push({ path: parts[++i], before: id(oldId), after: id(newId) });
        }
      }
      if (!files.length) {
        void vscode.window.showInformationMessage("No file changes.");
        return;
      }
      const ids = [...new Set(files.flatMap((f) => [f.before, f.after]).filter((x): x is string => !!x))].filter(
        (x) => !fs.existsSync(objectPath(this.root, x)),
      );
      for (let i = 0; i < ids.length; i += 200) await run(this.root, ["git-objects", "--", ...ids.slice(i, i + 200)], 600_000);
      const empty = (p: string) => vscode.Uri.from({ scheme: "devsync-remote", path: `/${p}`, query: "", fragment: "empty" });
      const resources = files.map((f) => [
        vscode.Uri.file(path.join(this.root, f.path)),
        f.before ? gitUri(this.root, f.from ?? f.path, "HEAD", f.before) : empty(f.path),
        f.after ? gitUri(this.root, f.path, "HEAD", f.after) : empty(f.path),
      ]);
      try {
        await vscode.commands.executeCommand("vscode.changes", title, resources);
      } catch {
        const pick = await vscode.window.showQuickPick(
          files.map((f, i) => ({ label: f.path, description: f.from ? `from ${f.from}` : "", i })),
          { title },
        );
        if (pick) {
          const [, l, r] = resources[pick.i];
          await vscode.commands.executeCommand("vscode.diff", l, r, path.basename(files[pick.i].path));
        }
      }
    });
  }

  private html(webview: vscode.Webview): string {
    const nonce = Math.random().toString(36).slice(2) + Date.now().toString(36);
    return /* html */ `<!DOCTYPE html>
<html lang="en"><head><meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}';">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<style>
  :root {
    --ref: var(--vscode-scmGraph-historyItemRefColor, #59a4f9);
    --remote: var(--vscode-scmGraph-historyItemRemoteRefColor, #b180d7);
    --lane0: var(--vscode-scmGraph-foreground1, #ffb000);
    --lane1: var(--vscode-scmGraph-foreground2, #dc267f);
    --lane2: var(--vscode-scmGraph-foreground3, #994f00);
    --lane3: var(--vscode-scmGraph-foreground4, #40b0a6);
    --lane4: var(--vscode-scmGraph-foreground5, #b66dff);
  }
  body { margin: 0; padding: 0; font-family: var(--vscode-font-family); font-size: var(--vscode-font-size); color: var(--vscode-foreground); user-select: none; }
  .row { display: flex; align-items: center; height: 22px; padding-right: 6px; cursor: pointer; white-space: nowrap; position: relative; }
  .row:hover { background: var(--vscode-list-hoverBackground); }
  .row.sel { background: var(--vscode-list-inactiveSelectionBackground); }
  .row:focus { outline: 1px solid var(--vscode-list-focusOutline); outline-offset: -1px; }
  svg.g { flex: none; display: block; }
  .subj { overflow: hidden; text-overflow: ellipsis; min-width: 0; flex: 0 1 auto; }
  .head .subj { font-weight: 600; }
  .author { margin-left: 6px; color: var(--vscode-descriptionForeground); overflow: hidden; text-overflow: ellipsis; min-width: 0; flex: 0 100 auto; }
  .refs { display: flex; gap: 4px; margin-left: 6px; flex: none; }
  .ref { display: inline-flex; align-items: center; gap: 3px; height: 18px; padding: 0 7px 0 5px; border-radius: 9px; font-size: 12px; color: #fff; }
  .ref svg { width: 14px; height: 14px; }
  .act { margin-left: auto; padding-left: 6px; opacity: 0; flex: none; display: flex; color: var(--vscode-icon-foreground); }
  .row:hover .act { opacity: 1; }
  .act svg { width: 16px; height: 16px; }
  .msg { padding: 8px 12px; color: var(--vscode-descriptionForeground); }
  .err { color: var(--vscode-errorForeground); }
  /* hover card */
  #card { position: fixed; z-index: 10; display: none; box-sizing: border-box; width: max-content; max-width: min(520px, calc(100vw - 12px)); left: 6px; overflow-wrap: anywhere; background: var(--vscode-editorHoverWidget-background);
    color: var(--vscode-editorHoverWidget-foreground); border: 1px solid var(--vscode-editorHoverWidget-border); border-radius: 4px; box-shadow: 0 2px 8px var(--vscode-widget-shadow);
    font-size: 13px; line-height: 1.55; user-select: text; cursor: default; }
  #card .sec { padding: 6px 10px; border-top: 1px solid var(--vscode-editorHoverWidget-border); }
  #card .sec:first-child { border-top: 0; }
  #card a { color: var(--vscode-textLink-foreground); text-decoration: none; cursor: pointer; }
  #card a:hover { text-decoration: underline; }
  #card .who { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  #card .avatar { width: 18px; height: 18px; border-radius: 3px; background: var(--ref); color: #fff; display: inline-grid; place-items: center; font-size: 10px; font-weight: 600; }
  #card .body { white-space: pre-wrap; word-break: break-word; max-height: 220px; overflow: auto; }
  #card .ins { color: var(--vscode-gitDecoration-addedResourceForeground, #81b88b); }
  #card .del { color: var(--vscode-gitDecoration-deletedResourceForeground, #c74e39); }
  #card .foot { display: flex; align-items: center; gap: 8px; }
  #card .dim { color: var(--vscode-descriptionForeground); }
</style></head>
<body><div id="list"><div class="msg">Loading remote history…</div></div><div id="card"></div>
<script nonce="${nonce}">
  const vscode = acquireVsCodeApi();
  const W = 11, H = 22, X0 = 9;
  const x = (c) => X0 + c * W;
  const col = (k) => "var(--" + (k || "lane0") + ")";
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
  const I = {
    target: '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M8 1a7 7 0 1 1 0 14A7 7 0 0 1 8 1zm0 1.4a5.6 5.6 0 1 0 0 11.2A5.6 5.6 0 0 0 8 2.4zM8 5a3 3 0 1 1 0 6 3 3 0 0 1 0-6z"/></svg>',
    cloud: '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"><path d="M4.5 12.5h7.2a2.8 2.8 0 0 0 .4-5.6 4 4 0 0 0-7.6-1A3.3 3.3 0 0 0 4.5 12.5z"/></svg>',
    branch: '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"><circle cx="5" cy="3.5" r="1.5"/><circle cx="5" cy="12.5" r="1.5"/><circle cx="11" cy="5" r="1.5"/><path d="M5 5v6M11 6.5c0 3-6 2-6 4.5"/></svg>',
    tag: '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"><path d="M2.5 2.5h5l6 6-5 5-6-6z"/><circle cx="5.5" cy="5.5" r="1"/></svg>',
    diff: '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2"><rect x="3.5" y="1.5" width="9" height="13" rx="1"/><path d="M8 4v4M6 6h4M6 11h4"/></svg>',
    copy: '<svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.2"><rect x="5.5" y="5.5" width="8" height="9" rx="1"/><path d="M10.5 5.5v-3h-8v9h3"/></svg>',
    commit: '<svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.3"><circle cx="8" cy="8" r="2.5"/><path d="M8 1v4.5M8 10.5V15"/></svg>',
  };
  const seg = (a, b, y1, y2, k) => a === b
    ? '<line x1="' + x(a) + '" y1="' + y1 + '" x2="' + x(b) + '" y2="' + y2 + '" stroke="' + col(k) + '" stroke-width="1.5"/>'
    : '<path d="M' + x(a) + ' ' + y1 + ' C ' + x(a) + ' ' + (y1 + y2) / 2 + ', ' + x(b) + ' ' + (y1 + y2) / 2 + ', ' + x(b) + ' ' + y2 + '" fill="none" stroke="' + col(k) + '" stroke-width="1.5"/>';
  const ago = (t) => { const s = Date.now() / 1000 - t; if (s < 60) return "now"; if (s < 3600) return Math.round(s / 60) + " min ago"; if (s < 86400) return Math.round(s / 3600) + " hr ago"; if (s < 86400 * 30) return Math.round(s / 86400) + " days ago"; return new Date(t * 1000).toLocaleDateString(); };
  let data = null, sel = null, hoverTimer = null, hoverSha = null;
  const details = {};

  function refBadges(c, m, color) {
    return c.refs.map((ref) => {
      const cur = ref.startsWith("HEAD -> ");
      const tag = ref.startsWith("tag: ");
      const name = cur ? ref.slice(8) : tag ? ref.slice(5) : ref;
      const remote = !cur && !tag && name === m.upstream;
      const bg = remote ? "var(--remote)" : cur ? "var(--ref)" : col(color);
      const icon = cur ? I.target : remote ? I.cloud : tag ? I.tag : I.branch;
      return '<span class="ref" style="background:' + bg + '" title="' + esc(ref) + '">' + icon + esc(name) + '</span>';
    }).join("");
  }

  function render() {
    const m = data, list = document.getElementById("list");
    if (!m) return;
    if (m.error && !m.rows.length) { list.innerHTML = '<div class="msg err">' + esc(m.error) + '</div>'; return; }
    if (!m.rows.length) { list.innerHTML = '<div class="msg">' + (m.loading ? "Loading remote history…" : "No commits yet.") + '</div>'; return; }
    const maxW = Math.min(10, Math.max(...m.rows.map((r) => r.width)));
    const svgW = x(maxW - 1) + 9;
    const firstCol = m.rows[0].col;
    let html = "";
    // Outgoing / incoming rows, like VS Code's Graph.
    const synth = [];
    if (m.behind) synth.push({ which: "incoming", label: "Incoming Changes", desc: m.upstream, color: "remote" });
    if (m.ahead) synth.push({ which: "outgoing", label: "Outgoing Changes", desc: m.branch, color: "ref" });
    synth.forEach((s, i) => {
      let g = (i > 0 ? seg(firstCol, firstCol, 0, H / 2 - 5, s.color) : "") + seg(firstCol, firstCol, H / 2 + 5, H, s.color);
      g += '<circle cx="' + x(firstCol) + '" cy="' + H / 2 + '" r="5" fill="none" stroke="' + col(s.color) + '" stroke-width="1.5" stroke-dasharray="3 2"/>';
      html += '<div class="row synthetic" tabindex="0" data-which="' + s.which + '" title="Click to see all ' + s.label.toLowerCase() + '"><svg class="g" width="' + svgW + '" height="' + H + '">' + g + '</svg>' +
        '<span class="subj">' + s.label + '</span><span class="author">' + esc(s.desc || "") + '</span><span class="act">' + I.diff + '</span></div>';
    });
    m.rows.forEach((r, idx) => {
      const c = r.commit;
      let g = "";
      if (idx === 0 && synth.length) g += seg(r.col, r.col, 0, H / 2, synth[synth.length - 1].color);
      for (const [a, b, k] of r.top) g += seg(a, b, 0, H / 2, k);
      for (const [a, b, k] of r.bottom) g += seg(a, b, H / 2, H, k);
      const isHead = c.sha === m.head;
      if (isHead) {
        g += '<circle cx="' + x(r.col) + '" cy="' + H / 2 + '" r="5.5" fill="var(--vscode-sideBar-background)" stroke="' + col(r.color) + '" stroke-width="2"/>' +
             '<circle cx="' + x(r.col) + '" cy="' + H / 2 + '" r="2.5" fill="' + col(r.color) + '"/>';
      } else if (c.parents.length > 1) {
        g += '<circle cx="' + x(r.col) + '" cy="' + H / 2 + '" r="4" fill="var(--vscode-sideBar-background)" stroke="' + col(r.color) + '" stroke-width="1.5"/>';
      } else {
        g += '<circle cx="' + x(r.col) + '" cy="' + H / 2 + '" r="3.5" fill="' + col(r.color) + '"/>';
      }
      html += '<div class="row' + (isHead ? " head" : "") + (sel === c.sha ? " sel" : "") + '" tabindex="0" data-sha="' + c.sha + '">' +
        '<svg class="g" width="' + svgW + '" height="' + H + '">' + g + '</svg>' +
        '<span class="subj">' + esc(c.subject) + '</span>' +
        (c.refs.length ? '<span class="refs">' + refBadges(c, m, r.color) + '</span>' : '<span class="author">' + esc(c.author) + '</span>') +
        '<span class="act" title="Open Changes">' + I.diff + '</span></div>';
    });
    if (m.error) html += '<div class="msg err">' + esc(m.error) + '</div>';
    list.innerHTML = html;
    list.querySelectorAll(".row[data-which]").forEach((el) => el.addEventListener("click", () => vscode.postMessage({ type: "openRange", which: el.dataset.which })));
    list.querySelectorAll(".row[data-sha]").forEach((el) => {
      const sha = el.dataset.sha;
      el.addEventListener("click", () => { sel = sha; list.querySelectorAll(".row").forEach((r) => r.classList.toggle("sel", r === el)); hideCard(); vscode.postMessage({ type: "open", sha }); });
      el.addEventListener("keydown", (e) => { if (e.key === "Enter") el.click(); });
      el.addEventListener("contextmenu", (e) => { e.preventDefault(); hideCard(); vscode.postMessage({ type: "actions", sha }); });
      el.addEventListener("mouseenter", () => { clearTimeout(hoverTimer); hoverTimer = setTimeout(() => showCard(sha, el), 600); });
      el.addEventListener("mouseleave", () => { clearTimeout(hoverTimer); hoverTimer = setTimeout(() => { if (!cardHovered) hideCard(); }, 250); });
    });
  }

  // ---- hover card
  const card = document.getElementById("card");
  let cardHovered = false;
  card.addEventListener("mouseenter", () => { cardHovered = true; clearTimeout(hoverTimer); });
  card.addEventListener("mouseleave", () => { cardHovered = false; hideCard(); });
  function hideCard() { card.style.display = "none"; hoverSha = null; }
  function showCard(sha, el) {
    hoverSha = sha;
    if (!details[sha]) vscode.postMessage({ type: "details", sha });
    fillCard(sha);
    const r = el.getBoundingClientRect();
    card.style.display = "block";
    const below = r.bottom + 4;
    const h = card.offsetHeight;
    card.style.top = (below + h < window.innerHeight || r.top - h - 4 < 0 ? below : r.top - h - 4) + "px";
  }
  function forge(url) {
    const host = new URL(url).hostname.replace(/^www\\./, "");
    return host === "github.com" ? "GitHub" : host === "gitlab.com" ? "GitLab" : host === "bitbucket.org" ? "Bitbucket" : host;
  }
  function fillCard(sha) {
    const row = data.rows.find((r) => r.commit.sha === sha);
    if (!row) return;
    const c = row.commit, d = details[sha];
    const author = d ? d.author : c.author;
    const when = d ? d.date : c.date;
    const initials = author.split(/\\s+/).map((w) => w[0] || "").join("").slice(0, 2).toUpperCase();
    let h = '<div class="sec"><div class="who"><span class="avatar">' + esc(initials) + '</span><b>' + esc(author) + '</b>, <span class="dim">' + ago(when) + ' (' + esc(new Date(when * 1000).toLocaleString()) + ')</span></div>';
    if (d && d.coauthors.length) h += d.coauthors.map((a) => '<div class="who"><b>' + esc(a) + '</b> <i class="dim">(Co-author)</i></div>').join("");
    h += '<div class="body">' + esc(d ? d.message : c.subject) + '</div></div>';
    if (d && d.stats) {
      h += '<div class="sec">' + esc(d.stats).replace(/(\\d+ insertions?\\(\\+\\))/, '<span class="ins">$1</span>').replace(/(\\d+ deletions?\\(-\\))/, '<span class="del">$1</span>') + '</div>';
    } else if (!d) {
      h += '<div class="sec dim">Loading details…</div>';
    }
    if (c.refs.length) h += '<div class="sec"><span class="refs" style="margin:0">' + refBadges(c, data, row.color) + '</span></div>';
    h += '<div class="sec foot">' + I.commit + '<a data-act="copy" title="Copy commit id">' + sha.slice(0, 7) + ' ' + I.copy + '</a>' +
      (d && d.web ? '<span class="dim">|</span><a data-act="web">Open on ' + esc(forge(d.web)) + '</a>' : "") +
      '<span class="dim">|</span><a data-act="actions">More…</a></div>';
    card.innerHTML = h;
    card.querySelectorAll("a[data-act]").forEach((a) => a.addEventListener("click", () => {
      const act = a.dataset.act;
      if (act === "copy") { vscode.postMessage({ type: "copy", sha }); a.textContent = "Copied"; }
      if (act === "web") vscode.postMessage({ type: "web", sha });
      if (act === "actions") { hideCard(); vscode.postMessage({ type: "actions", sha }); }
    }));
  }

  window.addEventListener("message", (ev) => {
    const m = ev.data;
    if (m.type === "data") { data = m; render(); }
    if (m.type === "details") { details[m.sha] = m.details; if (hoverSha === m.sha) fillCard(m.sha); }
    if (m.type === "reveal") {
      const el = document.querySelector('.row[data-sha="' + m.sha + '"]');
      if (el) { el.scrollIntoView({ block: "center" }); el.focus(); }
    }
  });
  vscode.postMessage({ type: "ready" });
</script></body></html>`;
  }

  dispose(): void {
    for (const d of this.disposables) d.dispose();
  }
}
