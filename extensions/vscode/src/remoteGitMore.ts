// The rest of VS Code's Git feature set, for the remote repository:
// commit variants, branches, remotes, stash, tags, file history, commit
// actions (from the graph), blame in the status bar, auto fetch, init.
// Everything goes through `devsync git …` (RemoteGit.git / gitOk).

import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { gitUri } from "./remoteDocs";
import { Cancelled, RemoteGit } from "./remoteGit";

type Ref = { name: string; kind: "local" | "remote" | "tag"; sha: string; subject: string };

export class RemoteGitMore implements vscode.Disposable {
  private readonly disposables: vscode.Disposable[] = [];
  private readonly blameItem: vscode.StatusBarItem;
  private blameCache = new Map<string, { key: string; lines: BlameLine[] }>();
  private blameTimer?: NodeJS.Timeout;
  private fetchTimer?: NodeJS.Timeout;

  constructor(private readonly g: RemoteGit) {
    this.blameItem = vscode.window.createStatusBarItem("devsync.blame", vscode.StatusBarAlignment.Left, 49);
    this.blameItem.name = "Remote git blame";
    this.disposables.push(
      this.blameItem,
      vscode.window.onDidChangeTextEditorSelection(() => this.scheduleBlame()),
      vscode.window.onDidChangeActiveTextEditor(() => this.scheduleBlame()),
      g.onDidChangeHead(() => {
        this.blameCache.clear();
        this.scheduleBlame();
      }),
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration("devsync.git")) {
          this.setupAutoFetch();
          this.scheduleBlame();
        }
      }),
    );
    this.registerCommands();
    this.setupAutoFetch();
  }

  // ------------------------------------------------------------ helpers

  private ok(title: string, args: string[], push = true) {
    return this.g.gitOk(title, args, push);
  }

  private async q(args: string[]): Promise<string> {
    const r = await this.g.git(args);
    return r.code === 0 ? r.stdout : "";
  }

  private async confirm(message: string, detail: string, button: string): Promise<void> {
    if ((await vscode.window.showWarningMessage(message, { modal: true, detail }, button)) !== button) throw new Cancelled();
  }

  private async ask(title: string, prompt: string, value = ""): Promise<string> {
    const v = await vscode.window.showInputBox({ title, prompt, value, ignoreFocusOut: true });
    if (!v?.trim()) throw new Cancelled();
    return v.trim();
  }

  private async refs(kinds: Ref["kind"][]): Promise<Ref[]> {
    const where = { local: "refs/heads", remote: "refs/remotes", tag: "refs/tags" };
    const out = await this.q([
      "for-each-ref",
      "--sort=-committerdate",
      "--format=%(refname)%09%(objectname:short)%09%(subject)",
      ...kinds.map((k) => where[k]),
    ]);
    return out
      .split("\n")
      .filter(Boolean)
      .map((l) => {
        const [full, sha, subject] = l.split("\t");
        const kind: Ref["kind"] = full.startsWith("refs/heads/") ? "local" : full.startsWith("refs/tags/") ? "tag" : "remote";
        return { name: full.replace(/^refs\/(heads|remotes|tags)\//, ""), kind, sha, subject };
      })
      .filter((r) => !r.name.endsWith("/HEAD"));
  }

  private async pickRef(title: string, kinds: Ref["kind"][], exclude?: string): Promise<string> {
    const icon = { local: "$(git-branch)", remote: "$(cloud)", tag: "$(tag)" };
    const items = (await this.refs(kinds))
      .filter((r) => r.name !== exclude)
      .map((r) => ({ label: `${icon[r.kind]} ${r.name}`, description: r.sha, detail: r.subject, ref: r.name }));
    if (!items.length) {
      void vscode.window.showInformationMessage("Git (remote): nothing to choose from.");
      throw new Cancelled();
    }
    const p = await vscode.window.showQuickPick(items, { title, matchOnDescription: true });
    if (!p) throw new Cancelled();
    return p.ref;
  }

  private async pickRemote(title: string): Promise<string> {
    const remotes = (await this.q(["remote"])).split("\n").filter(Boolean);
    if (!remotes.length) {
      void vscode.window.showInformationMessage("Git (remote): this repository has no remotes. Use “Git Add Remote…”.");
      throw new Cancelled();
    }
    if (remotes.length === 1) return remotes[0];
    const p = await vscode.window.showQuickPick(remotes, { title });
    if (!p) throw new Cancelled();
    return p;
  }

  private async pickStash(title: string): Promise<string> {
    const out = await this.q(["stash", "list", "--format=%gd%x09%cr%x09%gs"]);
    const items = out
      .split("\n")
      .filter(Boolean)
      .map((l) => {
        const [ref, when, msg] = l.split("\t");
        return { label: `$(archive) ${msg}`, description: `${ref} · ${when}`, ref };
      });
    if (!items.length) {
      void vscode.window.showInformationMessage("Git (remote): there are no stashes.");
      throw new Cancelled();
    }
    const p = await vscode.window.showQuickPick(items, { title, matchOnDescription: true });
    if (!p) throw new Cancelled();
    return p.ref;
  }

  private input(): vscode.SourceControlInputBox | undefined {
    return this.g.scm?.inputBox;
  }

  private activeRel(uri?: vscode.Uri): string {
    const u = uri ?? vscode.window.activeTextEditor?.document.uri;
    if (!u || u.scheme !== "file") throw new Error("Open a file of the project first.");
    const rel = path.relative(this.g.root, u.fsPath);
    if (rel.startsWith("..") || path.isAbsolute(rel)) throw new Error("That file is not in the devsync project.");
    return rel.split(path.sep).join("/");
  }

  // ----------------------------------------------------------- commands

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
    const message = async (title: string) => {
      const box = this.input();
      const m = box?.value.trim() || (await this.ask(title, "Commit message"));
      return m;
    };
    const clearInput = () => {
      const box = this.input();
      if (box) box.value = "";
    };

    // ---- commit variants
    reg("devsync.git.commitAmend", async () => {
      const m = this.input()?.value.trim();
      await this.ok("Amending the last commit", ["commit", "-q", "--amend", ...(m ? ["-m", m] : ["--no-edit"])]);
      clearInput();
    });
    reg("devsync.git.commitAllAmend", async () => {
      const m = this.input()?.value.trim();
      await this.ok("Staging all", ["add", "-A", "--", "."]);
      await this.ok("Amending the last commit", ["commit", "-q", "--amend", ...(m ? ["-m", m] : ["--no-edit"])], false);
      clearInput();
    });
    reg("devsync.git.commitNoVerify", async () => {
      await this.ok("Committing (no verify)", ["commit", "-q", "--no-verify", "-m", await message("Commit (No Verify)")]);
      clearInput();
    });
    reg("devsync.git.commitSignoff", async () => {
      await this.ok("Committing (signed off)", ["commit", "-q", "--signoff", "-m", await message("Commit (Signed Off)")]);
      clearInput();
    });
    reg("devsync.git.commitEmpty", async () => {
      await this.ok("Committing (empty)", ["commit", "-q", "--allow-empty", "-m", await message("Commit Empty")]);
      clearInput();
    });
    reg("devsync.git.undoCommit", async () => {
      const msg = (await this.q(["log", "-1", "--format=%B"])).trim();
      await this.ok("Undoing the last commit", ["reset", "-q", "--soft", "HEAD~1"], false);
      const box = this.input();
      if (box && msg) box.value = msg;
    });

    // ---- changes
    reg("devsync.git.discardAll", async () => {
      await this.confirm(
        "Discard ALL changes in the remote repository?",
        "Tracked files go back to their last committed version and untracked files are deleted, on the remote and here. This can't be undone.",
        "Discard All",
      );
      await this.ok("Discarding all changes", ["checkout", "--", "."]);
      await this.ok("Deleting untracked files", ["clean", "-fd", "-q", "--", "."]);
    });
    reg("devsync.git.cleanAll", async () => {
      await this.confirm("Delete all untracked files?", "They are deleted on the remote and here. This can't be undone.", "Delete");
      await this.ok("Deleting untracked files", ["clean", "-fd", "-q", "--", "."]);
    });
    reg("devsync.git.stageAllTracked", () => this.ok("Staging tracked changes", ["add", "-u", "--", "."]));
    reg("devsync.git.stageAllUntracked", async () => {
      const untracked = this.g.changes.filter((e) => e.group === "untracked").map((e) => e.rel);
      if (untracked.length) await this.ok("Staging untracked files", ["add", "--", ...untracked]);
    });
    reg("devsync.git.openHEADFile", async (a?: { entry?: { rel: string } } | vscode.Uri) => {
      const rel = a && "entry" in a && a.entry ? a.entry.rel : this.activeRel(a instanceof vscode.Uri ? a : undefined);
      await vscode.window.showTextDocument(gitUri(this.g.root, rel, "HEAD"), { preview: true });
    });
    reg("devsync.git.ignore", async (a?: { entry?: { rel: string } } | vscode.Uri, all?: { entry?: { rel: string } }[]) => {
      const list = Array.isArray(all) && all.length ? all : [a];
      const rels = list
        .map((x) => (x && "entry" in x && x.entry ? x.entry.rel : x instanceof vscode.Uri ? this.activeRel(x) : undefined))
        .filter((x): x is string => !!x);
      const targets = rels.length ? rels : [this.activeRel()];
      // .gitignore is a normal project file: edit it here, devsync syncs it.
      const file = path.join(this.g.root, ".gitignore");
      const existing = fs.existsSync(file) ? fs.readFileSync(file, "utf8") : "";
      const add = targets.map((t) => `/${t}`).filter((t) => !existing.split("\n").includes(t));
      if (add.length) fs.writeFileSync(file, `${existing}${existing && !existing.endsWith("\n") ? "\n" : ""}${add.join("\n")}\n`);
      this.g.scheduleRefresh(1500);
    });

    // ---- branches
    reg("devsync.git.branch", async () => {
      const name = await this.ask("Create branch", "New branch name");
      await this.ok(`Creating ${name}`, ["checkout", "-b", name]);
    });
    reg("devsync.git.branchFrom", async (from?: string) => {
      const base = typeof from === "string" ? from : await this.pickRef("Create branch from…", ["local", "remote", "tag"]);
      const name = await this.ask(`Create branch from ${base.slice(0, 12)}`, "New branch name");
      await this.ok(`Creating ${name}`, ["checkout", "-b", name, base]);
    });
    reg("devsync.git.renameBranch", async () => {
      const name = await this.ask("Rename branch", `New name for ${this.g.branchName}`, this.g.branchName);
      await this.ok(`Renaming to ${name}`, ["branch", "-m", name], false);
    });
    reg("devsync.git.deleteBranch", async () => {
      const b = await this.pickRef("Delete branch", ["local"], this.g.branchName);
      const r = await this.g.git(["branch", "-d", b]);
      if (r.code !== 0) {
        await this.confirm(
          `${b} is not fully merged. Delete it anyway?`,
          "Its commits that are on no other branch will be hard to find again.",
          "Force Delete",
        );
        await this.ok(`Deleting ${b}`, ["branch", "-D", b], false);
      }
      this.g.scheduleRefresh(0);
    });
    reg("devsync.git.merge", async () => {
      const b = await this.pickRef("Merge branch into the current one", ["local", "remote"], this.g.branchName);
      await this.ok(`Merging ${b}`, ["merge", "--no-edit", b]);
    });
    reg("devsync.git.rebase", async () => {
      const b = await this.pickRef(`Rebase ${this.g.branchName} onto…`, ["local", "remote"], this.g.branchName);
      await this.ok(`Rebasing onto ${b}`, ["rebase", b]);
    });
    reg("devsync.git.rebaseAbort", () => this.ok("Aborting rebase", ["rebase", "--abort"]));
    reg("devsync.git.mergeAbort", () => this.ok("Aborting merge", ["merge", "--abort"]));
    reg("devsync.git.checkoutDetached", async (ref?: string) => {
      const r = typeof ref === "string" ? ref : await this.pickRef("Checkout (detached) to…", ["local", "remote", "tag"]);
      await this.ok(`Checking out ${r.slice(0, 12)}`, ["checkout", "--detach", r]);
    });

    // ---- remotes
    reg("devsync.git.pullRebase", () => this.ok("Pulling with rebase (remote git)", ["pull", "--rebase"]));
    reg("devsync.git.pullFrom", async () => {
      const remote = await this.pickRemote("Pull from remote");
      const b = (await this.pickRef(`Pull from ${remote}`, ["remote"])).replace(`${remote}/`, "");
      await this.ok(`Pulling ${remote}/${b}`, ["pull", remote, b]);
    });
    reg("devsync.git.pushTo", async () => {
      const remote = await this.pickRemote("Push to remote");
      await this.ok(`Pushing to ${remote}`, ["push", "-u", remote, "HEAD"], false);
    });
    reg("devsync.git.pushForce", async () => {
      await this.confirm(
        `Force push ${this.g.branchName}?`,
        "Overwrites the branch on the server (with lease: refused if someone else pushed meanwhile).",
        "Force Push",
      );
      await this.ok("Force pushing (with lease)", ["push", "--force-with-lease"], false);
    });
    reg("devsync.git.pushTags", async () => {
      const remote = await this.pickRemote("Push tags to");
      await this.ok("Pushing tags", ["push", remote, "--tags"], false);
    });
    reg("devsync.git.fetchAll", () => this.ok("Fetching all remotes", ["fetch", "--all", "--prune"], false));
    reg("devsync.git.addRemote", async () => {
      const url = await this.ask("Add remote", "Repository URL");
      const name = await this.ask("Add remote", "Remote name", "origin");
      await this.ok(`Adding ${name}`, ["remote", "add", name, url], false);
      await this.ok(`Fetching ${name}`, ["fetch", name], false);
    });
    reg("devsync.git.removeRemote", async () => {
      const remote = await this.pickRemote("Remove remote");
      await this.confirm(`Remove remote ${remote}?`, "Only the remote's entry is removed; no branches on the server are touched.", "Remove");
      await this.ok(`Removing ${remote}`, ["remote", "remove", remote], false);
    });

    // ---- stash
    const stash = async (title: string, extra: string[]) => {
      const m = await vscode.window.showInputBox({ title, prompt: "Stash message (optional)", ignoreFocusOut: true });
      if (m === undefined) return;
      await this.ok("Stashing", ["stash", "push", ...extra, ...(m.trim() ? ["-m", m.trim()] : [])]);
    };
    reg("devsync.git.stash", () => stash("Stash", []));
    reg("devsync.git.stashUntracked", () => stash("Stash (Include Untracked)", ["--include-untracked"]));
    reg("devsync.git.stashStaged", () => stash("Stash Staged", ["--staged"]));
    reg("devsync.git.stashPopLatest", () => this.ok("Popping latest stash", ["stash", "pop"]));
    reg("devsync.git.stashPop", async () => this.ok("Popping stash", ["stash", "pop", await this.pickStash("Pop stash")]));
    reg("devsync.git.stashApplyLatest", () => this.ok("Applying latest stash", ["stash", "apply"]));
    reg("devsync.git.stashApply", async () => this.ok("Applying stash", ["stash", "apply", await this.pickStash("Apply stash")]));
    reg("devsync.git.stashDrop", async () => {
      const s = await this.pickStash("Drop stash");
      await this.confirm(`Drop ${s}?`, "The stashed changes are deleted.", "Drop");
      await this.ok("Dropping stash", ["stash", "drop", s], false);
    });
    reg("devsync.git.stashDropAll", async () => {
      await this.confirm("Drop ALL stashes?", "Every stash is deleted. This can't be undone.", "Drop All");
      await this.ok("Dropping all stashes", ["stash", "clear"], false);
    });
    reg("devsync.git.stashView", async () => {
      const s = await this.pickStash("View stash");
      const r = await this.ok("Loading stash", ["stash", "show", "-p", "--include-untracked", s], false);
      const doc = await vscode.workspace.openTextDocument({ content: r.stdout, language: "diff" });
      await vscode.window.showTextDocument(doc, { preview: true });
    });

    // ---- tags
    reg("devsync.git.createTag", async (at?: string) => {
      const name = await this.ask("Create tag", "Tag name");
      const msg = await vscode.window.showInputBox({ title: `Tag ${name}`, prompt: "Message (empty: lightweight tag)" });
      if (msg === undefined) return;
      const target = typeof at === "string" ? [at] : [];
      await this.ok(`Creating tag ${name}`, msg.trim() ? ["tag", "-a", name, "-m", msg.trim(), ...target] : ["tag", name, ...target], false);
    });
    reg("devsync.git.deleteTag", async () => {
      const t = await this.pickRef("Delete tag", ["tag"]);
      await this.ok(`Deleting tag ${t}`, ["tag", "-d", t], false);
    });

    // ---- history
    reg("devsync.git.fileHistory", async (uri?: vscode.Uri) => {
      const rel = this.activeRel(uri instanceof vscode.Uri ? uri : undefined);
      const out = await this.q(["log", "--follow", "-n", "200", "--format=%H%x09%an%x09%cr%x09%s", "--", `./${rel}`]);
      const items = out
        .split("\n")
        .filter(Boolean)
        .map((l) => {
          const [sha, author, when, subject] = l.split("\t");
          return { label: `$(git-commit) ${subject}`, description: `${sha.slice(0, 7)} · ${author} · ${when}`, sha };
        });
      if (!items.length) {
        void vscode.window.showInformationMessage(`${rel} has no committed history on the remote.`);
        return;
      }
      const p = await vscode.window.showQuickPick(items, { title: `History of ${rel} (remote)`, matchOnDescription: true });
      if (!p) return;
      await vscode.commands.executeCommand(
        "vscode.diff",
        gitUri(this.g.root, rel, `${p.sha}^`),
        gitUri(this.g.root, rel, p.sha),
        `${path.basename(rel)} (${p.sha.slice(0, 7)})`,
      );
    });
    reg("devsync.git.showOutput", () => this.g.log.show());
    reg("devsync.git.init", async () => {
      await this.ok("Initializing a repository on the remote", ["init"], false);
      this.g.resetRepository();
    });

    // ---- commit actions (Graph right-click)
    reg("devsync.git.commitActions", async (sha: string, subject = "") => {
      type A = vscode.QuickPickItem & { run: () => Thenable<unknown> | Promise<unknown> };
      const short = sha.slice(0, 7);
      const actions: A[] = [
        { label: "$(git-compare) Open Changes", run: () => vscode.commands.executeCommand("devsync.git.showCommit", sha) },
        { label: "$(copy) Copy Commit ID", run: () => vscode.env.clipboard.writeText(sha) },
        { label: "$(copy) Copy Commit Message", run: async () => vscode.env.clipboard.writeText((await this.q(["log", "-1", "--format=%B", sha])).trim()) },
        { label: "$(git-branch) Create Branch from Here…", run: () => vscode.commands.executeCommand("devsync.git.branchFrom", sha) },
        { label: "$(tag) Create Tag Here…", run: () => vscode.commands.executeCommand("devsync.git.createTag", sha) },
        { label: "$(debug-disconnect) Checkout (Detached)", run: () => vscode.commands.executeCommand("devsync.git.checkoutDetached", sha) },
        { label: "$(git-pull-request) Cherry-Pick", run: () => this.ok(`Cherry-picking ${short}`, ["cherry-pick", sha]) },
        { label: "$(discard) Revert Commit", run: () => this.ok(`Reverting ${short}`, ["revert", "--no-edit", sha]) },
        { label: "$(arrow-left) Reset to Here (Soft: keep changes staged)", run: () => this.ok("Resetting (soft)", ["reset", "--soft", sha], false) },
        { label: "$(arrow-left) Reset to Here (Mixed: keep changes unstaged)", run: () => this.ok("Resetting (mixed)", ["reset", "--mixed", sha]) },
        {
          label: "$(warning) Reset to Here (Hard: discard changes)",
          run: async () => {
            await this.confirm(
              `Hard reset ${this.g.branchName} to ${short}?`,
              "All uncommitted changes are discarded, on the remote and here (local edits not yet synced are kept as conflicts).",
              "Reset",
            );
            await this.ok("Resetting (hard)", ["reset", "--hard", sha]);
          },
        },
      ];
      const p = await vscode.window.showQuickPick(actions, { title: `${short} ${subject}`.trim() });
      if (p) await p.run();
    });

    reg("devsync.git.toggleBlame", async () => {
      const cfg = vscode.workspace.getConfiguration("devsync");
      await cfg.update("git.blame", !cfg.get<boolean>("git.blame", true), vscode.ConfigurationTarget.Global);
    });
  }

  // -------------------------------------------------------------- blame

  private scheduleBlame(): void {
    if (this.blameTimer) clearTimeout(this.blameTimer);
    this.blameTimer = setTimeout(() => void this.updateBlame(), 350);
  }

  private async updateBlame(): Promise<void> {
    const ed = vscode.window.activeTextEditor;
    const on = vscode.workspace.getConfiguration("devsync").get<boolean>("git.blame", true);
    if (!on || !ed || ed.document.uri.scheme !== "file" || !this.g.scm) {
      this.blameItem.hide();
      return;
    }
    let rel: string;
    try {
      rel = this.activeRel(ed.document.uri);
    } catch {
      this.blameItem.hide();
      return;
    }
    if (!this.g.indexIds.has(rel)) {
      this.blameItem.hide();
      return;
    }
    // Blame the remote working tree (= the synced file). Recompute when HEAD
    // or the file's git status changes; unsent local edits show as not committed.
    const dirty = this.g.changes.some((e) => e.rel === rel) || ed.document.isDirty;
    const key = `${this.g.head}:${dirty ? this.g.changes.find((e) => e.rel === rel)?.index ?? "dirty" : "clean"}`;
    let cached = this.blameCache.get(rel);
    if (!cached || cached.key !== key) {
      const r = await this.g.git(["blame", "--porcelain", "--", `./${rel}`]);
      if (r.code !== 0) {
        this.blameItem.hide();
        return;
      }
      cached = { key, lines: parseBlame(r.stdout) };
      this.blameCache.set(rel, cached);
      if (this.blameCache.size > 50) this.blameCache.delete(this.blameCache.keys().next().value as string);
    }
    if (vscode.window.activeTextEditor !== ed) return;
    const b = cached.lines[ed.selection.active.line];
    if (!b) {
      this.blameItem.hide();
      return;
    }
    if (/^0+$/.test(b.sha)) {
      this.blameItem.text = "$(git-commit) Not committed yet";
      this.blameItem.tooltip = "This line has changes that are not committed on the remote";
      this.blameItem.command = undefined;
    } else {
      this.blameItem.text = `$(git-commit) ${b.author}, ${ago(b.time)}`;
      const tip = new vscode.MarkdownString(undefined, true);
      tip.appendMarkdown(`**${b.author}** · ${new Date(b.time * 1000).toLocaleString()}\n\n`);
      tip.appendText(b.summary);
      tip.appendMarkdown(`\n\n\`${b.sha.slice(0, 10)}\` · remote git · click for commit actions`);
      this.blameItem.tooltip = tip;
      this.blameItem.command = { command: "devsync.git.commitActions", title: "Commit actions", arguments: [b.sha, b.summary] };
    }
    this.blameItem.show();
  }

  // --------------------------------------------------------- auto fetch

  private setupAutoFetch(): void {
    if (this.fetchTimer) clearInterval(this.fetchTimer);
    this.fetchTimer = undefined;
    const cfg = vscode.workspace.getConfiguration("devsync");
    if (!cfg.get<boolean>("git.autofetch", false)) return;
    const secs = Math.max(60, cfg.get<number>("git.autofetchPeriod", 180));
    this.fetchTimer = setInterval(async () => {
      if (!this.g.scm || !vscode.window.state.focused) return;
      const r = await this.g.git(["fetch", "--prune", "--quiet"]);
      if (r.code === 0) this.g.scheduleRefresh(0);
    }, secs * 1000);
  }

  dispose(): void {
    if (this.blameTimer) clearTimeout(this.blameTimer);
    if (this.fetchTimer) clearInterval(this.fetchTimer);
    for (const d of this.disposables) d.dispose();
  }
}

interface BlameLine {
  sha: string;
  author: string;
  time: number;
  summary: string;
}

/** Parses `git blame --porcelain` into one entry per line (index = line - 1). */
export function parseBlame(out: string): BlameLine[] {
  const commits = new Map<string, BlameLine>();
  const lines: BlameLine[] = [];
  let cur: BlameLine | undefined;
  let finalLine = 0;
  for (const l of out.split("\n")) {
    const head = /^([0-9a-f]{40}) \d+ (\d+)/.exec(l);
    if (head) {
      const sha = head[1];
      finalLine = Number(head[2]);
      cur = commits.get(sha) ?? { sha, author: "", time: 0, summary: "" };
      commits.set(sha, cur);
      continue;
    }
    if (!cur) continue;
    if (l.startsWith("author ")) cur.author = l.slice(7);
    else if (l.startsWith("author-time ")) cur.time = Number(l.slice(12));
    else if (l.startsWith("summary ")) cur.summary = l.slice(8);
    else if (l.startsWith("\t")) lines[finalLine - 1] = cur;
  }
  return lines;
}

function ago(t: number): string {
  const s = Date.now() / 1000 - t;
  if (s < 3600) return `${Math.max(1, Math.round(s / 60))} minutes ago`;
  if (s < 86400) return `${Math.round(s / 3600)} hours ago`;
  if (s < 86400 * 30) return `${Math.round(s / 86400)} days ago`;
  if (s < 86400 * 365) return `${Math.round(s / (86400 * 30))} months ago`;
  return `${Math.round(s / (86400 * 365))} years ago`;
}
