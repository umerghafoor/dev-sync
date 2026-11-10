// "Git (remote)": the remote project's real git repository in the Source
// Control view, working like VS Code's own Git: staged / changes groups,
// click for a diff, stage / unstage / discard, commit with ⌘Enter, branch
// and sync in the status bar. Every command runs `devsync git …`, which
// pushes local edits first (so git sees them) and brings back files that
// git changes (checkout, pull, discard, …).

import * as path from "path";
import * as vscode from "vscode";
import { DevsyncEvent, runJson } from "./cli";
import { tagged } from "./model";
import { emptyUri, gitUri } from "./remoteDocs";

interface GitResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

type Group = "merge" | "index" | "workingTree" | "untracked";

interface Entry {
  rel: string;
  /** Path before a rename. */
  from?: string;
  group: Group;
  /** Single letter as Git shows it: M A D R C U ? */
  letter: string;
}

const LETTER_TAG: Record<string, string> = {
  M: "modified",
  A: "added",
  D: "deleted",
  R: "renamed",
  C: "added",
  U: "conflict",
  "?": "untracked",
  T: "modified",
};

const LETTER_WORD: Record<string, string> = {
  M: "Modified",
  A: "Added",
  D: "Deleted",
  R: "Renamed",
  C: "Copied",
  U: "Conflict",
  "?": "Untracked",
  T: "Type changed",
};

/** Parses `git status --porcelain=v1 -b -z`. Paths are repo-relative. */
export function parseStatus(out: string): { branch: string; upstream: string; ahead: number; behind: number; entries: Entry[] } {
  const parts = out.split("\0");
  let branch = "";
  let upstream = "";
  let ahead = 0;
  let behind = 0;
  const entries: Entry[] = [];
  for (let i = 0; i < parts.length; i++) {
    const p = parts[i];
    if (!p) continue;
    if (p.startsWith("## ")) {
      const head = p.slice(3);
      const m = /^(.*?)(?:\.\.\.(\S+))?(?: \[(.*)\])?$/.exec(head);
      branch = (m?.[1] ?? head).replace(/^No commits yet on /, "").replace(/^HEAD \(no branch\)$/, "detached");
      upstream = m?.[2] ?? "";
      ahead = Number(/ahead (\d+)/.exec(m?.[3] ?? "")?.[1] ?? 0);
      behind = Number(/behind (\d+)/.exec(m?.[3] ?? "")?.[1] ?? 0);
      continue;
    }
    const x = p[0];
    const y = p[1];
    const rel = p.slice(3);
    let from: string | undefined;
    if (x === "R" || x === "C") from = parts[++i];
    if (x === "?" && y === "?") {
      entries.push({ rel, group: "untracked", letter: "?" });
      continue;
    }
    if (x === "!") continue;
    const conflict = x === "U" || y === "U" || (x === "A" && y === "A") || (x === "D" && y === "D");
    if (conflict) {
      entries.push({ rel, group: "merge", letter: "U" });
      continue;
    }
    if (x !== " ") entries.push({ rel, from, group: "index", letter: x });
    if (y !== " ") entries.push({ rel, group: "workingTree", letter: y });
  }
  return { branch, upstream, ahead, behind, entries };
}

type State = vscode.SourceControlResourceState & { entry: Entry };

export class RemoteGit implements vscode.Disposable {
  private sc?: vscode.SourceControl;
  private groups?: Record<"merge" | "index" | "workingTree", vscode.SourceControlResourceGroup>;
  /** Project path inside the repo ("" when the project is the repo root). */
  private prefix?: string;
  private branch = "";
  private ahead = 0;
  private behind = 0;
  private upstream = "";
  private entries: Entry[] = [];
  private timer?: NodeJS.Timeout;
  private refreshing = false;
  private again = false;
  private busy = 0;
  private readonly disposables: vscode.Disposable[] = [];

  constructor(
    private readonly root: string,
    private readonly log: vscode.OutputChannel,
    private readonly onFilesChanged: () => void,
  ) {
    this.registerCommands();
    this.scheduleRefresh(0);
  }

  // ------------------------------------------------------------ git calls

  /** Runs git on the remote. `push`: send local edits first. */
  private async git(args: string[], opts: { push?: boolean; onEvent?: (e: DevsyncEvent) => void } = {}): Promise<GitResult> {
    const flags = opts.push ? [] : ["--no-push"];
    return runJson<GitResult>(this.root, ["git", ...flags, "--", ...args], 15 * 60_000, opts.onEvent);
  }

  private async gitOk(title: string, args: string[], push = true): Promise<GitResult> {
    this.busy++;
    this.updateStatusBar();
    try {
      const r = await vscode.window.withProgress(
        { location: vscode.ProgressLocation.SourceControl, title },
        () => this.git(args, { push }),
      );
      this.log.appendLine(`$ git ${args.join(" ")}\n${r.stdout}${r.stderr}`);
      if (r.code !== 0) {
        const msg = (r.stderr || r.stdout).trim().split("\n").filter(Boolean).slice(-3).join(" ");
        const pick = await vscode.window.showErrorMessage(`Git (remote): ${msg || `git exited with ${r.code}`}`, "Show Output");
        if (pick) this.log.show();
        throw new Cancelled();
      }
      return r;
    } finally {
      this.busy--;
      this.onFilesChanged();
      this.scheduleRefresh(0);
    }
  }

  // ------------------------------------------------------------- refresh

  scheduleRefresh(delay = 500): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => void this.refresh(), delay);
  }

  async refresh(): Promise<void> {
    if (this.refreshing) {
      this.again = true;
      return;
    }
    this.refreshing = true;
    try {
      if (this.prefix === undefined) {
        const r = await this.git(["rev-parse", "--show-prefix"]);
        if (r.code !== 0) {
          // Not a git repository (or git missing) on the remote.
          this.hide();
          return;
        }
        this.prefix = r.stdout.trim();
      }
      const r = await this.git(["status", "--porcelain=v1", "-b", "-z", "--", "."]);
      if (r.code !== 0) {
        this.log.appendLine(`git status: ${r.stderr.trim()}`);
        return;
      }
      const s = parseStatus(r.stdout);
      this.branch = s.branch;
      this.upstream = s.upstream;
      this.ahead = s.ahead;
      this.behind = s.behind;
      const prefix = this.prefix ?? "";
      this.entries = s.entries
        .filter((e) => e.rel.startsWith(prefix))
        .map((e) => ({ ...e, rel: e.rel.slice(prefix.length), from: e.from?.slice(prefix.length) }));
      this.render();
    } catch (e) {
      this.log.appendLine(`git (remote): ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      this.refreshing = false;
      if (this.again) {
        this.again = false;
        this.scheduleRefresh(200);
      }
    }
  }

  private hide(): void {
    this.sc?.dispose();
    this.sc = undefined;
    this.groups = undefined;
    void vscode.commands.executeCommand("setContext", "devsync.remoteGit", false);
  }

  private ensureScm(): void {
    if (this.sc) return;
    const sc = vscode.scm.createSourceControl("devsync-git", "Git (remote)", vscode.Uri.file(this.root));
    sc.inputBox.placeholder = `Message (${process.platform === "darwin" ? "⌘" : "Ctrl+"}Enter to commit on the remote)`;
    sc.acceptInputCommand = { command: "devsync.git.commit", title: "Commit" };
    const merge = sc.createResourceGroup("merge", "Merge Changes");
    const index = sc.createResourceGroup("index", "Staged Changes");
    const workingTree = sc.createResourceGroup("workingTree", "Changes");
    merge.hideWhenEmpty = true;
    index.hideWhenEmpty = true;
    workingTree.hideWhenEmpty = false;
    this.sc = sc;
    this.groups = { merge, index, workingTree };
    void vscode.commands.executeCommand("setContext", "devsync.remoteGit", true);
  }

  private render(): void {
    this.ensureScm();
    const groups = this.groups!;
    const states = (g: Group[]) => this.entries.filter((e) => g.includes(e.group)).map((e) => this.state(e));
    groups.merge.resourceStates = states(["merge"]);
    groups.index.resourceStates = states(["index"]);
    groups.workingTree.resourceStates = states(["workingTree", "untracked"]);
    this.sc!.count = this.entries.length;
    this.updateStatusBar();
  }

  private updateStatusBar(): void {
    if (!this.sc) return;
    const sync =
      this.busy > 0
        ? "$(sync~spin)"
        : this.upstream
          ? `$(sync)${this.behind ? ` ${this.behind}↓` : ""}${this.ahead ? ` ${this.ahead}↑` : ""}`
          : "$(cloud-upload)";
    this.sc.statusBarCommands = [
      { command: "devsync.git.checkout", title: `$(git-branch) ${this.branch || "…"} (remote)`, tooltip: "Remote branch — click to switch" },
      {
        command: this.upstream ? "devsync.git.sync" : "devsync.git.publish",
        title: sync,
        tooltip: this.upstream
          ? `Pull ${this.behind} and push ${this.ahead} commit(s) on the remote (${this.upstream})`
          : "Publish this branch from the remote",
      },
    ];
  }

  private state(e: Entry): State {
    const file = path.join(this.root, e.rel);
    const tag = e.group === "untracked" ? "untracked" : LETTER_TAG[e.letter] ?? "modified";
    const where = e.group === "index" ? "staged" : e.group === "merge" ? "conflict" : "not staged";
    return {
      entry: e,
      resourceUri: tagged(file, `git-${tag}`),
      contextValue: `git-${e.group}`,
      decorations: {
        tooltip: `${e.rel} — ${LETTER_WORD[e.letter] ?? e.letter} (${where}, remote git)${e.from ? `\nrenamed from ${e.from}` : ""}`,
        strikeThrough: e.letter === "D",
        faded: false,
      },
      command: { command: "devsync.git.open", title: "Open Changes", arguments: [e] },
    };
  }

  // ------------------------------------------------------------ commands

  private registerCommands(): void {
    const reg = (id: string, fn: (...a: any[]) => unknown) =>
      this.disposables.push(
        vscode.commands.registerCommand(id, async (...a: unknown[]) => {
          try {
            await fn(...a);
          } catch (e) {
            if (!(e instanceof Cancelled)) {
              void vscode.window.showErrorMessage(`Git (remote): ${e instanceof Error ? e.message : String(e)}`);
            }
          }
        }),
      );
    // Resource commands get the clicked state plus the multi-selection.
    const pick = (a: unknown, all?: unknown): Entry[] => {
      const list = (Array.isArray(all) && all.length ? all : [a]) as (State | Entry | undefined)[];
      return list
        .map((x) => (x && "entry" in x ? x.entry : (x as Entry | undefined)))
        .filter((x): x is Entry => !!x);
    };
    const rels = (es: Entry[]) => es.map((e) => e.rel);

    reg("devsync.git.refresh", () => this.refresh());
    reg("devsync.git.open", (a: unknown) => this.openChange(pick(a)[0]));
    reg("devsync.git.openFile", (a: unknown) => {
      const e = pick(a)[0];
      if (e) void vscode.window.showTextDocument(vscode.Uri.file(path.join(this.root, e.rel)));
    });
    reg("devsync.git.stage", (a: unknown, all: unknown) =>
      this.gitOk("Staging on the remote", ["add", "-A", "--", ...rels(pick(a, all))]),
    );
    reg("devsync.git.stageAll", () => this.gitOk("Staging all on the remote", ["add", "-A", "--", "."]));
    reg("devsync.git.unstage", (a: unknown, all: unknown) =>
      this.gitOk("Unstaging", ["reset", "-q", "HEAD", "--", ...rels(pick(a, all))], false),
    );
    reg("devsync.git.unstageAll", () => this.gitOk("Unstaging all", ["reset", "-q", "HEAD", "--", "."], false));
    reg("devsync.git.discard", async (a: unknown, all: unknown) => {
      const es = pick(a, all);
      if (!es.length) return;
      const untracked = es.filter((e) => e.group === "untracked");
      const tracked = es.filter((e) => e.group !== "untracked");
      const what = es.length === 1 ? path.basename(es[0].rel) : `${es.length} files`;
      const ok = await vscode.window.showWarningMessage(
        `Discard changes in ${what}?`,
        {
          modal: true,
          detail: untracked.length
            ? "Untracked files are DELETED on the remote and here. This can't be undone."
            : "The files go back to their last committed version, on the remote and here. This can't be undone.",
        },
        "Discard",
      );
      if (ok !== "Discard") return;
      if (tracked.length) await this.gitOk("Discarding changes", ["checkout", "--", ...rels(tracked)]);
      if (untracked.length) await this.gitOk("Deleting untracked files", ["clean", "-f", "-q", "--", ...rels(untracked)]);
    });
    reg("devsync.git.commit", () => this.commit(false));
    reg("devsync.git.commitAll", () => this.commit(true));
    reg("devsync.git.push", () => this.gitOk("Pushing (remote git)", ["push"], false));
    reg("devsync.git.pull", () => this.gitOk("Pulling (remote git)", ["pull", "--ff-only"]));
    reg("devsync.git.fetch", () => this.gitOk("Fetching (remote git)", ["fetch", "--prune"], false));
    reg("devsync.git.sync", async () => {
      await this.gitOk("Pulling (remote git)", ["pull", "--ff-only"]);
      await this.gitOk("Pushing (remote git)", ["push"], false);
    });
    reg("devsync.git.publish", () => this.gitOk("Publishing branch", ["push", "-u", "origin", "HEAD"], false));
    reg("devsync.git.checkout", () => this.checkout());
    reg("devsync.git.log", async () => {
      const r = await this.gitOk("Loading history", ["log", "--oneline", "--decorate", "-n", "200"], false);
      const doc = await vscode.workspace.openTextDocument({ content: r.stdout, language: "git-rebase" });
      await vscode.window.showTextDocument(doc, { preview: true });
    });
  }

  /** Like VS Code's Git: left = last version (index or HEAD), right = yours. */
  private async openChange(e?: Entry): Promise<void> {
    if (!e) return;
    const local = vscode.Uri.file(path.join(this.root, e.rel));
    const name = path.basename(e.rel);
    let left: vscode.Uri;
    let right: vscode.Uri;
    let title: string;
    if (e.group === "index") {
      left = e.letter === "A" ? emptyUri(e.rel) : gitUri(this.root, e.from ?? e.rel, "HEAD");
      right = e.letter === "D" ? emptyUri(e.rel) : gitUri(this.root, e.rel, "");
      title = `${name} (Index, remote)`;
    } else if (e.group === "untracked") {
      await vscode.window.showTextDocument(local, { preview: true });
      return;
    } else {
      left = e.group === "merge" ? gitUri(this.root, e.rel, "HEAD") : gitUri(this.root, e.rel, "");
      right = e.letter === "D" ? emptyUri(e.rel) : local;
      title = `${name} (Working Tree, remote git)`;
    }
    await vscode.commands.executeCommand("vscode.diff", left, right, title, { preview: true });
  }

  private async commit(all: boolean): Promise<void> {
    const sc = this.sc;
    if (!sc) return;
    // Pick up edits made since the last refresh before deciding.
    await this.refresh();
    let message = sc.inputBox.value.trim();
    const staged = this.entries.filter((e) => e.group === "index").length;
    const changed = this.entries.filter((e) => e.group !== "index").length;
    if (!staged && !changed) {
      void vscode.window.showInformationMessage("Git (remote): there are no changes to commit.");
      return;
    }
    let stageAll = all || (!staged && this.smartCommitEnabled());
    if (!staged && !stageAll) {
      const pick = await vscode.window.showWarningMessage(
        "There are no staged changes to commit. Stage all your changes and commit them directly?",
        { modal: true },
        "Yes",
        "Always",
      );
      if (!pick) return;
      if (pick === "Always") await vscode.workspace.getConfiguration("devsync").update("git.smartCommit", true, true);
      stageAll = true;
    }
    if (!stageAll && !staged) return;
    if (!message) {
      message =
        (await vscode.window.showInputBox({
          title: "Commit on the remote",
          prompt: "Commit message",
          ignoreFocusOut: true,
        })) ?? "";
      if (!message.trim()) return;
    }
    if (stageAll) await this.gitOk("Staging all", ["add", "-A", "--", "."]);
    await this.gitOk("Committing on the remote", ["commit", "-q", "-m", message], !stageAll);
    sc.inputBox.value = "";
  }

  smartCommitEnabled(): boolean {
    return vscode.workspace.getConfiguration("devsync").get<boolean>("git.smartCommit", false);
  }

  private async checkout(): Promise<void> {
    const r = await this.gitOk(
      "Listing branches",
      ["for-each-ref", "--sort=-committerdate", "--format=%(refname)\t%(objectname:short)\t%(subject)", "refs/heads", "refs/remotes"],
      false,
    );
    const refs = r.stdout
      .split("\n")
      .filter(Boolean)
      .map((l) => {
        const [full, sha, subject] = l.split("\t");
        const local = full.startsWith("refs/heads/");
        const name = full.replace(/^refs\/(heads|remotes)\//, "");
        return { full, sha, subject, local, name };
      })
      .filter((x) => !x.full.endsWith("/HEAD"));
    const localNames = new Set(refs.filter((x) => x.local).map((x) => x.name));
    type Item = vscode.QuickPickItem & { args?: string[]; create?: boolean };
    const items: Item[] = [
      { label: "$(plus) Create new branch…", create: true },
      { label: "Branches", kind: vscode.QuickPickItemKind.Separator },
      ...refs
        .filter((x) => x.local)
        .map((x) => ({ label: `$(git-branch) ${x.name}`, description: x.sha, detail: x.subject, args: ["checkout", x.name] })),
      { label: "Remote branches", kind: vscode.QuickPickItemKind.Separator },
      ...refs
        .filter((x) => !x.local)
        .map((x) => {
          // origin/foo → check out (and track) a local branch foo, like VS Code.
          const short = x.name.replace(/^[^/]+\//, "");
          const args = localNames.has(short) ? ["checkout", short] : ["checkout", "--track", x.name];
          return { label: `$(cloud) ${x.name}`, description: x.sha, detail: x.subject, args };
        }),
    ];
    const pick = await vscode.window.showQuickPick(items, {
      title: `Remote git — on ${this.branch}`,
      placeHolder: "Select a branch to check out on the remote",
      matchOnDescription: true,
    });
    if (!pick) return;
    if (pick.create) {
      const name = await vscode.window.showInputBox({ title: "New branch on the remote", prompt: "Branch name" });
      if (name?.trim()) await this.gitOk(`Creating ${name}`, ["checkout", "-b", name.trim()]);
      return;
    }
    if (pick.args) await this.gitOk(`Checking out`, pick.args);
  }

  dispose(): void {
    if (this.timer) clearTimeout(this.timer);
    this.sc?.dispose();
    for (const d of this.disposables) d.dispose();
  }
}

class Cancelled extends Error {}
