// "Graph (remote)": commit history of the remote repository with branch
// lanes, like VS Code's Graph view (which only shows local repositories).
// Clicking a commit opens all of its changes in a multi-file diff, with file
// contents served from the local git-object cache (fetched in one trip).

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

/** One drawn row: where the commit sits and which lines pass through. */
export interface Row {
  commit: Commit;
  col: number;
  /** Lines in the upper half of the row: [fromCol, toCol] (toCol = node column when merging in). */
  top: [number, number][];
  /** Lines in the lower half: [fromCol(node or lane), toCol]. */
  bottom: [number, number][];
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
        subject: subject ?? "",
      };
    });
}

/** Assigns lanes like `git log --graph`: each lane waits for one commit. */
export function layout(commits: Commit[]): Row[] {
  const lanes: (string | null)[] = [];
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
    const before = [...lanes];
    const top: [number, number][] = [];
    before.forEach((h, i) => {
      if (h === null) return;
      if (h === c.sha) {
        // Lanes waiting for this commit converge on it; a new lane starts here.
        if (i !== col || expected >= 0) top.push([i, col]);
        lanes[i] = null;
      } else {
        top.push([i, i]);
      }
    });
    const bottom: [number, number][] = [];
    c.parents.forEach((p, k) => {
      let j = lanes.indexOf(p);
      // The first parent stays in this lane when a lane further right waits
      // for it too; the lanes meet at the parent (like git's "|/").
      if (j < 0 || (k === 0 && j > col && lanes[col] === null)) {
        j = lanes[col] === null ? col : free();
        lanes[j] = p;
      }
      bottom.push([col, j]);
    });
    before.forEach((h, i) => {
      if (i !== col && h !== null && h !== c.sha && lanes[i] === h) bottom.push([i, i]);
    });
    while (lanes.length && lanes[lanes.length - 1] === null) lanes.pop();
    const width = Math.max(col + 1, ...top.map(([a, b]) => Math.max(a, b) + 1), ...bottom.map(([a, b]) => Math.max(a, b) + 1));
    rows.push({ commit: c, col, top, bottom, width });
  }
  return rows;
}

export class RemoteGraph implements vscode.WebviewViewProvider, vscode.Disposable {
  private view?: vscode.WebviewView;
  private rows: Row[] = [];
  private loading = false;
  private error = "";
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
    );
  }

  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.webview.options = { enableScripts: true };
    view.webview.html = this.html(view.webview);
    view.webview.onDidReceiveMessage((m: { type: string; sha?: string }) => {
      if (m.type === "ready") {
        this.post();
        if (!this.rows.length) void this.load();
      }
      if (m.type === "open" && m.sha) void this.openCommit(m.sha);
      if (m.type === "copy" && m.sha) void vscode.env.clipboard.writeText(m.sha);
    });
    view.onDidChangeVisibility(() => view.visible && this.post());
  }

  async load(): Promise<void> {
    if (this.loading) return;
    this.loading = true;
    this.post();
    try {
      // The current branch plus its upstream, like VS Code's "Auto" graph.
      const revs = ["HEAD", ...(this.git.upstreamName ? [this.git.upstreamName] : [])];
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
      this.rows = layout(parseLog(r.stdout));
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
    void this.view?.webview.postMessage({
      type: "data",
      rows: this.rows,
      head: this.git.head,
      upstream: this.git.upstreamName,
      loading: this.loading,
      error: this.error,
    });
  }

  /** Opens every change of a commit in one multi-file diff editor. */
  private async openCommit(sha: string): Promise<void> {
    const c = this.rows.find((r) => r.commit.sha === sha)?.commit;
    await vscode.window.withProgress(
      { location: { viewId: "devsync.gitGraph" }, title: "Loading commit" },
      async () => {
        // :old_mode new_mode old_id new_id status \0 path [\0 new_path]
        const r = await runJson<GitResult>(
          this.root,
          ["git", "--no-push", "--", "diff-tree", "-r", "-M", "--root", "--no-commit-id", "-z", sha],
          120_000,
        );
        if (r.code !== 0) throw new Error(r.stderr.trim());
        const parts = r.stdout.split("\0");
        const files: { path: string; from?: string; before?: string; after?: string }[] = [];
        for (let i = 0; i < parts.length; i++) {
          const meta = parts[i];
          if (!meta.startsWith(":")) continue;
          const [, , oldId, newId, status] = meta.slice(1).split(" ");
          const zero = (id: string) => /^0+$/.test(id);
          if (status.startsWith("R") || status.startsWith("C")) {
            const from = parts[++i];
            const to = parts[++i];
            files.push({ path: to, from, before: oldId, after: newId });
          } else {
            files.push({ path: parts[++i], before: zero(oldId) ? undefined : oldId, after: zero(newId) ? undefined : newId });
          }
        }
        if (!files.length) {
          void vscode.window.showInformationMessage("This commit changes no files.");
          return;
        }
        // Fetch every version this commit touches in one round trip.
        const ids = [...new Set(files.flatMap((f) => [f.before, f.after]).filter((x): x is string => !!x))].filter(
          (id) => !fs.existsSync(objectPath(this.root, id)),
        );
        for (let i = 0; i < ids.length; i += 200) await run(this.root, ["git-objects", "--", ...ids.slice(i, i + 200)], 600_000);
        const empty = (p: string) => vscode.Uri.from({ scheme: "devsync-remote", path: `/${p}`, query: "", fragment: "empty" });
        const resources = files.map((f) => [
          vscode.Uri.file(path.join(this.root, f.path)),
          f.before ? gitUri(this.root, f.from ?? f.path, "HEAD", f.before) : empty(f.path),
          f.after ? gitUri(this.root, f.path, "HEAD", f.after) : empty(f.path),
        ]);
        const title = `${sha.slice(0, 7)} ${c?.subject ?? ""} (remote)`;
        try {
          await vscode.commands.executeCommand("vscode.changes", title, resources);
        } catch {
          // Older VS Code: pick one file at a time.
          const pick = await vscode.window.showQuickPick(
            files.map((f, i) => ({ label: f.path, description: f.from ? `from ${f.from}` : "", i })),
            { title },
          );
          if (pick) {
            const [, l, rr] = resources[pick.i];
            await vscode.commands.executeCommand("vscode.diff", l, rr, `${path.basename(files[pick.i].path)} (${sha.slice(0, 7)})`);
          }
        }
      },
    );
  }

  private html(webview: vscode.Webview): string {
    const nonce = Math.random().toString(36).slice(2) + Date.now().toString(36);
    return /* html */ `<!DOCTYPE html>
<html lang="en"><head><meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}';">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<style>
  body { margin: 0; padding: 2px 0; font-family: var(--vscode-font-family); font-size: var(--vscode-font-size); color: var(--vscode-foreground); }
  .row { display: flex; align-items: center; height: 22px; padding-right: 8px; cursor: pointer; white-space: nowrap; }
  .row:hover { background: var(--vscode-list-hoverBackground); }
  .row.sel { background: var(--vscode-list-activeSelectionBackground); color: var(--vscode-list-activeSelectionForeground); }
  svg { flex: none; display: block; }
  .subj { overflow: hidden; text-overflow: ellipsis; flex: 0 1 auto; min-width: 0; }
  .head .subj { font-weight: 600; }
  .ref { flex: none; display: inline-flex; align-items: center; gap: 3px; margin-left: 6px; padding: 0 6px; height: 16px; border-radius: 8px; font-size: 11px;
    background: var(--vscode-badge-background); color: var(--vscode-badge-foreground); }
  .ref.cur { background: var(--vscode-button-background); color: var(--vscode-button-foreground); }
  .ref.remote { background: transparent; border: 1px solid var(--vscode-badge-background); color: var(--vscode-descriptionForeground); }
  .meta { margin-left: 8px; color: var(--vscode-descriptionForeground); flex: 0 1 auto; overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .msg { padding: 8px 12px; color: var(--vscode-descriptionForeground); }
  .err { color: var(--vscode-errorForeground); }
</style></head>
<body><div id="list"><div class="msg">Loading remote history…</div></div>
<script nonce="${nonce}">
  const vscode = acquireVsCodeApi();
  const COLORS = ["#4f8ef7", "#e5a33b", "#3fb07a", "#c05cd6", "#e2574c", "#2bb3c0", "#a3a83a", "#7a7ff0"];
  const W = 12, H = 22, R = 4;
  const x = (c) => 8 + c * W;
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
  const ago = (t) => { const s = Date.now() / 1000 - t; return s < 3600 ? Math.max(1, Math.round(s / 60)) + "m" : s < 86400 ? Math.round(s / 3600) + "h" : s < 86400 * 30 ? Math.round(s / 86400) + "d" : new Date(t * 1000).toLocaleDateString(); };
  const line = (a, b, y1, y2, color) => a === b
    ? '<line x1="' + x(a) + '" y1="' + y1 + '" x2="' + x(b) + '" y2="' + y2 + '" stroke="' + color + '" stroke-width="1.6"/>'
    : '<path d="M' + x(a) + ' ' + y1 + ' C ' + x(a) + ' ' + (y1 + y2) / 2 + ', ' + x(b) + ' ' + (y1 + y2) / 2 + ', ' + x(b) + ' ' + y2 + '" fill="none" stroke="' + color + '" stroke-width="1.6"/>';
  let sel = null;
  function render(m) {
    const list = document.getElementById("list");
    if (m.error && !m.rows.length) { list.innerHTML = '<div class="msg err">' + esc(m.error) + '</div>'; return; }
    if (!m.rows.length) { list.innerHTML = '<div class="msg">' + (m.loading ? "Loading remote history…" : "No commits yet.") + '</div>'; return; }
    const maxW = Math.min(8, Math.max(...m.rows.map((r) => r.width)));
    const svgW = x(maxW - 1) + 10;
    list.innerHTML = m.rows.map((r, i) => {
      const c = r.commit, color = (k) => COLORS[k % COLORS.length];
      let g = "";
      for (const [a, b] of r.top) g += line(a, b, 0, H / 2, color(a === b ? a : b));
      for (const [a, b] of r.bottom) g += line(a, b, H / 2, H, color(b));
      const isHead = c.sha === m.head;
      g += isHead
        ? '<circle cx="' + x(r.col) + '" cy="' + H / 2 + '" r="' + R + '" fill="var(--vscode-sideBar-background, #1e1e1e)" stroke="' + color(r.col) + '" stroke-width="2"/>'
        : '<circle cx="' + x(r.col) + '" cy="' + H / 2 + '" r="' + R + '" fill="' + color(r.col) + '"/>';
      const refs = c.refs.map((ref) => {
        const cur = ref.startsWith("HEAD -> ");
        const name = cur ? ref.slice(8) : ref.replace(/^tag: /, "");
        const remote = !cur && /^[^/]+\\//.test(name) && name === m.upstream;
        return '<span class="ref' + (cur ? " cur" : "") + (remote ? " remote" : "") + '" title="' + esc(ref) + '">' + (remote ? "☁ " : "") + esc(name) + '</span>';
      }).join("");
      return '<div class="row' + (isHead ? " head" : "") + (sel === c.sha ? " sel" : "") + '" data-sha="' + c.sha + '" title="' + esc(c.subject + "\\n" + c.sha.slice(0, 10) + " · " + c.author) + '">' +
        '<svg width="' + svgW + '" height="' + H + '">' + g + '</svg>' +
        '<span class="subj">' + esc(c.subject) + '</span>' + refs +
        '<span class="meta">' + esc(c.author) + ' · ' + ago(c.date) + '</span></div>';
    }).join("") + (m.error ? '<div class="msg err">' + esc(m.error) + '</div>' : "");
    list.querySelectorAll(".row").forEach((el) => {
      el.addEventListener("click", () => { sel = el.dataset.sha; list.querySelectorAll(".row").forEach((r) => r.classList.toggle("sel", r === el)); vscode.postMessage({ type: "open", sha: sel }); });
      el.addEventListener("contextmenu", (e) => { e.preventDefault(); vscode.postMessage({ type: "copy", sha: el.dataset.sha }); });
    });
  }
  window.addEventListener("message", (ev) => { if (ev.data.type === "data") render(ev.data); });
  vscode.postMessage({ type: "ready" });
</script></body></html>`;
  }

  dispose(): void {
    for (const d of this.disposables) d.dispose();
  }
}
