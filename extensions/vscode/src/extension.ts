// devsync for VS Code: a Source Control view (like Git) for running code on a
// remote machine, a status bar with sync state, auto sync, remote terminals
// and setup helpers. Everything is delegated to the devsync CLI.

import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { binaryPath, DevsyncEvent, DevsyncNotFound, EventStream, run, runJson, Status } from "./cli";
import { asResource, Model } from "./model";
import { DevsyncScm } from "./scm";
import { Sidebar } from "./views";
import { RemoteFiles } from "./remoteFiles";
import { Activity, formatBytes } from "./activity";

const CONFIG_FILE = ".devsync.toml";

type SyncState = "idle" | "pending" | "syncing" | "synced" | "offline" | "error" | "stopped" | "choose";

class Controller implements vscode.Disposable {
  private root?: string;
  private model?: Model;
  private scm?: DevsyncScm;
  private sidebar?: Sidebar;
  private remoteFiles?: RemoteFiles;
  private binaryStamp = "";
  private updateTimer?: NodeJS.Timeout;
  private askedToChoose = false;
  private readonly status: vscode.StatusBarItem;
  private watch?: EventStream;
  private readonly disposables: vscode.Disposable[] = [];
  private restartTimer?: NodeJS.Timeout;
  private restartDelay = 2000;
  private lastSync?: Date;
  private state: SyncState = "stopped";
  private detail = "";
  private readonly notifyJobs = new Set<string>();
  private readonly activity = new Activity();
  /** When offline: epoch ms of auto sync's next attempt. */
  private retryAt = 0;

  constructor(private readonly ctx: vscode.ExtensionContext, private readonly log: vscode.OutputChannel) {
    this.status = vscode.window.createStatusBarItem("devsync.status", vscode.StatusBarAlignment.Left, 50);
    this.status.name = "devsync";
    this.status.command = "devsync.menu";
    this.disposables.push(this.status);
  }

  async start(): Promise<void> {
    this.sidebar = new Sidebar({
      run: (command, background) =>
        background ? this.runInBackground(command, this.activeDir()) : this.runInTerminal(command, this.activeDir()),
      exec: (id) => void vscode.commands.executeCommand(id),
      open: (rel) => {
        if (this.root) void vscode.window.showTextDocument(vscode.Uri.file(path.join(this.root, rel)), { preview: true });
      },
      clearActivity: () => this.activity.clearRecent(),
    });
    this.remoteFiles = new RemoteFiles(
      this.log,
      () => this.model?.scheduleRefresh(0),
      (title, op) => this.withTransfer(title, op),
    );
    this.disposables.push(
      this.sidebar,
      this.remoteFiles,
      this.activity,
      this.activity.onDidChange(() => this.setState(this.state, this.detail)),
    );
    this.registerCommands();
    this.pushRunState();
    const root = await this.findRoot();
    if (root) this.activate(root);
  }

  // ------------------------------------------------------------ project

  private async findRoot(): Promise<string | undefined> {
    for (const f of vscode.workspace.workspaceFolders ?? []) {
      if (fs.existsSync(path.join(f.uri.fsPath, CONFIG_FILE))) return f.uri.fsPath;
    }
    const found = await vscode.workspace.findFiles(`**/${CONFIG_FILE}`, "**/node_modules/**", 1);
    return found[0] ? path.dirname(found[0].fsPath) : undefined;
  }

  private activate(root: string): void {
    if (this.root === root) return;
    this.root = root;
    void vscode.commands.executeCommand("setContext", "devsync.active", true);
    this.model = new Model(root, this.log);
    this.disposables.push(this.model);
    if (vscode.workspace.getConfiguration("devsync").get<boolean>("sourceControl", true)) {
      this.scm = new DevsyncScm(root, this.model);
      this.disposables.push(this.scm);
    }
    this.sidebar?.setModel(this.model);
    this.remoteFiles?.setRoot(root);
    // When the devsync program is updated, restart auto sync so no old
    // version keeps running in the background.
    this.updateTimer = setInterval(() => {
      const stamp = this.binaryVersionStamp();
      if (this.watch?.running && this.binaryStamp && stamp && stamp !== this.binaryStamp) {
        this.log.appendLine("devsync was updated; restarting auto sync");
        this.stopWatch();
        this.startWatch();
      }
    }, 15_000);
    this.disposables.push(this.model.onDidChange(() => this.setState(this.state, this.detail)));
    this.status.show();
    this.setState("stopped");
    if (vscode.workspace.getConfiguration("devsync").get<boolean>("autoSync", true)) {
      this.startWatch();
    }
    this.log.appendLine(`devsync project: ${root} (binary: ${binaryPath()})`);
  }

  private requireRoot(): string {
    if (!this.root) {
      throw new Error("No devsync project here yet. Run “devsync: Connect Project to Remote…” first.");
    }
    return this.root;
  }

  /** Directory of the active editor if it is inside the project, else the root. */
  private activeDir(): string {
    const root = this.requireRoot();
    const doc = vscode.window.activeTextEditor?.document;
    if (doc && doc.uri.scheme === "file" && doc.uri.fsPath.startsWith(root + path.sep)) {
      return path.dirname(doc.uri.fsPath);
    }
    return root;
  }

  // -------------------------------------------------------------- status

  private setState(state: SyncState, detail = ""): void {
    this.state = state;
    this.detail = detail;
    const view: Record<SyncState, [string, string]> = {
      idle: ["$(cloud)", "devsync"],
      pending: ["$(cloud-upload)", "devsync: changes"],
      syncing: ["$(sync~spin)", "devsync: syncing"],
      synced: ["$(check)", "devsync"],
      offline: ["$(cloud-offline)", "devsync: offline"],
      error: ["$(error)", "devsync: error"],
      stopped: ["$(debug-pause)", "devsync: paused"],
      choose: ["$(list-tree)", "devsync: choose what to sync"],
    };
    let [icon, label] = view[state];
    const t = this.activity.transfer;
    if (t && t.direction !== "none") {
      const pct = t.bytesTotal > 0 ? (100 * Math.min(t.bytesDone, t.bytesTotal)) / t.bytesTotal : 100;
      icon = t.direction === "up" ? "$(cloud-upload)" : "$(cloud-download)";
      label = `devsync ${Math.floor(pct)}% · ${t.filesDone}/${t.filesTotal}`;
    } else if (t) {
      icon = "$(sync~spin)";
      label = "devsync: checking";
    }
    const running = this.model?.runningJobs ?? 0;
    this.status.text = `${icon} ${label}${running ? ` · $(play) ${running}` : ""}`;
    const tip = new vscode.MarkdownString(undefined, true);
    tip.appendMarkdown(`**devsync** — ${state}\n\n`);
    if (detail) tip.appendText(`${detail}\n\n`);
    if (t && t.direction !== "none") {
      tip.appendText(
        `${t.direction === "up" ? "Pushing" : "Pulling"} ${t.filesDone}/${t.filesTotal} files, ` +
          `${formatBytes(Math.min(t.bytesDone, t.bytesTotal))} of ${formatBytes(t.bytesTotal)}` +
          `${t.rate > 0 ? ` at ${formatBytes(t.rate)}/s` : ""}\n${t.path}\n\n`,
      );
    }
    if (state === "offline") tip.appendMarkdown("[Retry now](command:devsync.retry)\n\n");
    if (this.lastSync) tip.appendMarkdown(`Last sync: ${this.lastSync.toLocaleTimeString()}\n\n`);
    tip.appendMarkdown("Click for actions");
    this.status.tooltip = tip;
    this.status.backgroundColor =
      state === "error"
        ? new vscode.ThemeColor("statusBarItem.errorBackground")
        : state === "offline"
          ? new vscode.ThemeColor("statusBarItem.warningBackground")
          : undefined;
    void vscode.commands.executeCommand("setContext", "devsync.watching", !!this.watch?.running);
    this.pushRunState();
  }

  /** Remote target from .devsync.toml, for display. */
  private remoteLabel(): string {
    if (!this.root) return "";
    try {
      const t = fs.readFileSync(path.join(this.root, CONFIG_FILE), "utf8");
      const host = /^\s*host\s*=\s*"([^"]*)"/m.exec(t)?.[1] ?? "";
      const dir = /^\s*path\s*=\s*"([^"]*)"/m.exec(t)?.[1] ?? "";
      return host ? `${host}:${dir}` : "";
    } catch {
      return "";
    }
  }

  private pushRunState(label?: string): void {
    const names: Record<SyncState, string> = {
      idle: "connected",
      pending: "changes waiting",
      syncing: "syncing…",
      synced: "in sync",
      offline: "offline — changes queued",
      error: "error",
      stopped: "auto sync paused",
      choose: "large remote — choose what to sync",
    };
    this.sidebar?.updateRun({
      active: !!this.root,
      state: this.state,
      label: label && label !== "devsync" ? label : names[this.state],
      detail: this.detail,
      host: this.remoteLabel(),
      running: this.model?.runningJobs ?? 0,
      pending: this.model?.pendingChanges ?? 0,
      history: this.ctx.workspaceState.get<string[]>("devsync.history", []),
      retryAt: this.state === "offline" ? this.retryAt : 0,
      transfer: this.activity.transfer,
      activity: this.activity.recent,
    });
  }

  private remember(command: string): void {
    const h = this.ctx.workspaceState.get<string[]>("devsync.history", []).filter((c) => c !== command);
    void this.ctx.workspaceState.update("devsync.history", [command, ...h].slice(0, 10));
    void this.ctx.workspaceState.update("devsync.lastCommand", command);
    this.pushRunState();
  }

  private onEvent(e: DevsyncEvent): void {
    this.activity.handle(e);
    switch (e.event) {
      case "stage":
        if (this.state !== "syncing") this.setState("syncing", e.message);
        break;
      case "pull_planned":
        if (e.download > 0)
          this.setState(
            "syncing",
            `${e.download} file(s) to download${e.already ? `, ${e.already} already here` : ""}`,
          );
        break;
      case "watching":
        this.setState("idle");
        break;
      case "changed":
        this.setState("pending", `${e.paths} change(s) waiting to be sent`);
        this.model?.scheduleRefresh();
        break;
      case "planned":
        if (e.upload > 0 || e.delete > 0) this.setState("syncing", `${e.upload} to upload, ${e.delete} to delete`);
        break;
      case "synced":
        this.lastSync = new Date();
        this.restartDelay = 2000;
        this.setState("synced", `sent ${e.files} file(s)`);
        this.log.appendLine(`synced ${e.files} file(s), ${e.bytes} bytes, ${e.deleted} deleted in ${e.millis} ms`);
        this.model?.scheduleRefresh(0);
        break;
      case "up_to_date":
        this.lastSync = new Date();
        this.setState("synced", "remote is up to date");
        this.model?.scheduleRefresh(0);
        break;
      case "retrying":
      case "offline":
        this.retryAt = Date.now() + (e.event === "offline" ? e.retry_in_ms : e.delay_ms);
        this.setState("offline", e.error);
        this.model?.scheduleRefresh();
        break;
      case "needs_selection":
        this.setState("choose", e.message);
        this.log.appendLine(e.message);
        if (!this.askedToChoose) {
          this.askedToChoose = true;
          void vscode.window
            .showWarningMessage(
              "The remote project is large. Choose which folders to sync before the first sync.",
              "Choose What to Sync",
            )
            .then((pick) => pick && this.remoteFiles?.reveal());
        }
        break;
      case "info":
        if (e.message === "retrying now") this.setState("syncing", "retrying now");
        break;
      case "pulled":
        this.log.appendLine(`pulled ${e.path}`);
        this.model?.scheduleRefresh();
        break;
      case "conflict":
        void this.showConflict(e.path, e.saved_as);
        this.model?.scheduleRefresh(0);
        break;
      case "job_updated":
        this.model?.scheduleRefresh();
        break;
      case "job_finished":
        this.model?.scheduleRefresh(0);
        void this.notifyFinished(e.id, e.state, e.exit_code);
        break;
      case "warning":
      case "error":
        this.log.appendLine(`${e.event}: ${e.message}`);
        if (e.event === "error") this.setState("error", e.message);
        break;
      default:
        break;
    }
  }

  private async notifyFinished(id: string, state: string, code: number | null): Promise<void> {
    if (!this.notifyJobs.delete(id)) return; // only background jobs started from here
    const ok = state === "exited" && code === 0;
    const msg = ok
      ? `Remote job finished successfully.`
      : `Remote job ${state === "lost" ? "was lost" : `failed (exit ${code})`}.`;
    const show = ok ? vscode.window.showInformationMessage : vscode.window.showWarningMessage;
    if (await show(msg, "Open Output")) this.openOutput(id);
  }

  private async showConflict(rel: string, savedAs: string): Promise<void> {
    const root = this.requireRoot();
    const pick = await vscode.window.showWarningMessage(
      `${rel} was changed both here and by a remote command. Your version is kept; the remote one is in ${path.basename(savedAs)}.`,
      "Compare",
    );
    if (pick === "Compare") {
      await vscode.commands.executeCommand(
        "vscode.diff",
        vscode.Uri.file(path.join(root, rel)),
        vscode.Uri.file(path.join(root, savedAs)),
        `${rel} (yours ↔ remote)`,
      );
    }
  }

  // --------------------------------------------------------------- watch

  /** Identifies the installed devsync binary (changes when it is updated). */
  private binaryVersionStamp(): string {
    try {
      const st = fs.statSync(binaryPath());
      return `${st.size}:${st.mtimeMs}`;
    } catch {
      return "";
    }
  }

  private startWatch(): void {
    const root = this.requireRoot();
    if (this.watch?.running) return;
    this.binaryStamp = this.binaryVersionStamp();
    this.askedToChoose = false;
    this.watch?.dispose();
    this.watch = new EventStream(
      root,
      ["watch"],
      (e) => this.onEvent(e),
      (code, stderr) => {
        const msg = stderr.trim().split("\n").pop() || `exited with ${code}`;
        this.log.appendLine(`auto sync stopped: ${msg}`);
        this.setState("error", msg);
        if (msg.includes("not found")) {
          void this.binaryMissing();
          return;
        }
        this.restartTimer = setTimeout(() => this.startWatch(), this.restartDelay);
        this.restartDelay = Math.min(this.restartDelay * 2, 60_000);
      },
    );
    this.watch.start();
    this.setState("idle", "starting auto sync");
  }

  private stopWatch(): void {
    if (this.restartTimer) clearTimeout(this.restartTimer);
    this.watch?.stop();
    this.watch = undefined;
    this.setState("stopped", "auto sync is off — use Push to send changes");
  }

  private async binaryMissing(): Promise<void> {
    const pick = await vscode.window.showErrorMessage(
      "The devsync command-line tool was not found. Install it (make install in the devsync repo) or set devsync.binaryPath.",
      "Open Settings",
    );
    if (pick) await vscode.commands.executeCommand("workbench.action.openSettings", "devsync.binaryPath");
  }

  // ----------------------------------------------------------- terminals

  private terminal(name: string, args: string[], cwd: string): vscode.Terminal {
    const t = vscode.window.createTerminal({
      name,
      cwd,
      shellPath: binaryPath(),
      shellArgs: args,
      iconPath: new vscode.ThemeIcon("remote"),
    });
    t.show();
    return t;
  }

  private runInTerminal(command: string, cwd: string): void {
    this.remember(command);
    void this.ctx.workspaceState.update("devsync.lastCwd", cwd);
    const short = command.length > 40 ? `${command.slice(0, 37)}…` : command;
    this.terminal(`devsync: ${short}`, ["exec", "--", command], cwd);
    this.model?.scheduleRefresh(1500);
  }

  private async runInBackground(command: string, cwd: string): Promise<void> {
    this.remember(command);
    const r = await vscode.window.withProgress(
      { location: vscode.ProgressLocation.SourceControl, title: "Starting remote job" },
      () => runJson<{ id: string; output: string }>(cwd, ["submit", "--", command], 600_000),
    );
    this.notifyJobs.add(r.id);
    this.model?.scheduleRefresh(0);
    if (vscode.workspace.getConfiguration("devsync").get<boolean>("openOutputOnBackgroundRun", true)) {
      this.openOutput(r.id);
    }
  }

  private openOutput(id: string): void {
    const file = path.join(this.requireRoot(), ".devsync", "jobs", id, "output.log");
    void vscode.window.showTextDocument(vscode.Uri.file(file), { preview: true });
  }

  private async askCommand(title: string): Promise<string | undefined> {
    return vscode.window.showInputBox({
      title,
      prompt: "Runs on the remote, in the folder of the file you have open (after syncing).",
      value: this.ctx.workspaceState.get<string>("devsync.lastCommand", ""),
      placeHolder: "e.g. pytest -x  ·  npm test  ·  python3 main.py",
      ignoreFocusOut: true,
    });
  }

  /** Tries to reach the remote now instead of waiting for the backoff. */
  private retryNow(): void {
    if (this.watch?.running) {
      this.watch.send("retry");
      this.setState("syncing", "retrying now");
    } else {
      this.startWatch();
    }
  }

  /**
   * Runs a one-off push/pull with a progress notification and the sidebar's
   * live transfer card, both fed from the CLI's progress events.
   */
  async withTransfer<T>(title: string, op: (onEvent: (e: DevsyncEvent) => void) => Promise<T>): Promise<T> {
    return vscode.window.withProgress(
      { location: vscode.ProgressLocation.Notification, title: `devsync: ${title}` },
      async (progress) => {
        let shown = 0;
        const onEvent = (e: DevsyncEvent) => {
          this.activity.handle(e);
          if (e.event === "stage") progress.report({ message: `${e.message}…` });
          if (e.event === "info" && e.message.startsWith("another devsync")) {
            progress.report({ message: "waiting for auto sync to finish…" });
          }
          if (e.event === "retrying") progress.report({ message: `network problem — retry #${e.attempt}` });
          if (e.event === "progress") {
            const pct = e.bytes_total > 0 ? (100 * Math.min(e.bytes_done, e.bytes_total)) / e.bytes_total : 100;
            const name = e.path.split("/").pop() ?? e.path;
            progress.report({
              increment: Math.max(0, pct - shown),
              message: `${e.files_done}/${e.files_total} files · ${formatBytes(Math.min(e.bytes_done, e.bytes_total))} / ${formatBytes(e.bytes_total)} · ${name}`,
            });
            shown = Math.max(shown, pct);
          }
        };
        try {
          return await op(onEvent);
        } finally {
          this.activity.end();
        }
      },
    );
  }

  /** Pulls the whole project ([]) or the given project-relative paths. */
  private async pullPaths(rels: string[]): Promise<void> {
    const root = this.requireRoot();
    const what = rels.length === 0 ? "project" : rels.length === 1 ? rels[0] : `${rels.length} items`;
    const r = await this.withTransfer(`pulling ${what} from remote`, (onEvent) =>
      runJson<{ updated: string[]; conflicts: { path: string; saved_as: string }[] }>(
        root,
        ["pull", "--", ...rels],
        24 * 3600_000,
        onEvent,
      ),
    );
    this.model?.scheduleRefresh(0);
    if (r.updated.length === 0 && r.conflicts.length === 0) {
      void vscode.window.showInformationMessage(`Already up to date with the remote (${what}).`);
      return;
    }
    const msg = `Pulled ${r.updated.length} file(s) from the remote.${
      r.conflicts.length
        ? ` ${r.conflicts.length} file(s) you edited were kept; the remote versions are listed under Conflicts.`
        : ""
    }`;
    if (r.conflicts.length) {
      if (await vscode.window.showWarningMessage(msg, "Show Conflicts")) {
        await vscode.commands.executeCommand("devsync.conflicts.focus");
      }
    } else {
      void vscode.window.showInformationMessage(msg);
    }
  }

  // ------------------------------------------------------------ commands

  private registerCommands(): void {
    const reg = (id: string, fn: (...args: any[]) => unknown) =>
      this.disposables.push(
        vscode.commands.registerCommand(id, async (...args: unknown[]) => {
          try {
            await fn(...args);
          } catch (e) {
            if (e instanceof DevsyncNotFound) await this.binaryMissing();
            else void vscode.window.showErrorMessage(`devsync: ${e instanceof Error ? e.message : String(e)}`);
          }
        }),
      );

    reg("devsync.menu", async () => {
      const last = this.ctx.workspaceState.get<string>("devsync.lastCommand");
      const items: (vscode.QuickPickItem & { cmd?: string })[] = [
        ...(this.state === "offline" || this.state === "error"
          ? [
              {
                label: "$(refresh) Retry now",
                description: this.detail,
                cmd: "devsync.retry",
              },
            ]
          : []),
        { label: "Run", kind: vscode.QuickPickItemKind.Separator },
        { label: "$(play) Run command on remote…", cmd: "devsync.run" },
        { label: "$(debug-rerun) Run again", description: last, cmd: "devsync.rerun" },
        { label: "$(run-all) Run in background…", cmd: "devsync.runBackground" },
        { label: "$(terminal) Open remote terminal", cmd: "devsync.shell" },
        { label: "Sync", kind: vscode.QuickPickItemKind.Separator },
        { label: "$(repo-pull) Pull everything from remote", cmd: "devsync.pull" },
        { label: "$(cloud-download) Pull files from remote…", cmd: "devsync.pullPick" },
        { label: "$(list-tree) Choose what to sync…", cmd: "devsync.chooseFiles" },
        { label: "$(cloud-upload) Push now", cmd: "devsync.push" },
        this.watch?.running
          ? { label: "$(debug-pause) Pause auto sync", cmd: "devsync.stopWatch" }
          : { label: "$(sync) Resume auto sync", cmd: "devsync.startWatch" },
        { label: "$(remote-explorer) Show devsync sidebar", cmd: "devsync.focusRun" },
        { label: "$(source-control) Show in Source Control", cmd: "devsync.showPanel" },
        { label: "More", kind: vscode.QuickPickItemKind.Separator },
        { label: "$(plug) Forward ports", cmd: "devsync.forward" },
        { label: "$(info) Status", cmd: "devsync.status" },
        { label: "$(pulse) Doctor", cmd: "devsync.doctor" },
        { label: "$(sparkle) Set up Claude Code", cmd: "devsync.setupClaude" },
        { label: "$(book) Getting started guide", cmd: "devsync.walkthrough" },
      ];
      const pick = await vscode.window.showQuickPick(items, {
        title: `devsync — ${this.state}${this.detail ? `: ${this.detail}` : ""}`,
      });
      if (pick?.cmd) await vscode.commands.executeCommand(pick.cmd);
    });

    reg("devsync.init", async () => {
      const folder = vscode.workspace.workspaceFolders?.[0];
      if (!folder) throw new Error("Open your project folder first (File → Open Folder…).");
      const target = await vscode.window.showInputBox({
        title: "Connect this project to a remote machine",
        prompt: "user@host:path — the host can be an alias from ~/.ssh/config; ~ means the remote home folder",
        placeHolder: `me@devbox:~/${path.basename(folder.uri.fsPath)}`,
        ignoreFocusOut: true,
        validateInput: (v) => (/^[^:\s]+:\S+$/.test(v.trim()) ? undefined : "Use the form user@host:path"),
      });
      if (!target) return;
      const r = await run(folder.uri.fsPath, ["init", target.trim()]);
      if (r.code !== 0) throw new Error(r.stderr.trim());
      this.activate(folder.uri.fsPath);
      const pick = await vscode.window.showInformationMessage(
        "Project connected. Run the doctor to check the connection?",
        "Run Doctor",
        "Later",
      );
      if (pick === "Run Doctor") await vscode.commands.executeCommand("devsync.doctor");
    });

    reg("devsync.scmRun", async () => {
      const input = this.scm?.input;
      const cmd = input?.value.trim();
      if (!input) return;
      if (!cmd) {
        void vscode.window.showInformationMessage("Type a command in the devsync box first, e.g. pytest");
        return;
      }
      input.value = "";
      this.runInTerminal(cmd, this.activeDir());
    });

    reg("devsync.scmRunBackground", async () => {
      const input = this.scm?.input;
      const cmd = input?.value.trim();
      if (!input || !cmd) {
        await vscode.commands.executeCommand("devsync.runBackground");
        return;
      }
      input.value = "";
      await this.runInBackground(cmd, this.activeDir());
    });

    reg("devsync.push", async () => {
      const root = this.requireRoot();
      const r = await this.withTransfer("pushing to remote", (onEvent) =>
        runJson<DevsyncEvent>(root, ["push"], 24 * 3600_000, onEvent),
      );
      this.lastSync = new Date();
      const said =
        r.event === "synced"
          ? `pushed ${r.files} file(s), ${formatBytes(r.bytes)}`
          : r.event === "up_to_date"
            ? "remote is up to date"
            : "";
      this.setState(this.watch?.running ? "synced" : "stopped", said);
      if (said) vscode.window.setStatusBarMessage(`$(check) devsync: ${said}`, 4000);
      this.model?.scheduleRefresh(0);
    });

    reg("devsync.pull", () => this.pullPaths([]));

    // Explorer / editor-tab context menu: one item or a multi-selection.
    reg("devsync.pullPaths", async (uri?: vscode.Uri | { node?: { path: string } }, uris?: vscode.Uri[]) => {
      const root = this.requireRoot();
      // From the Remote Files view: the item carries its remote path.
      if (uri && "node" in uri && uri.node) {
        await this.pullPaths([uri.node.path]);
        return;
      }
      const picked = (uris && uris.length ? uris : uri instanceof vscode.Uri ? [uri] : []).filter(
        (u) => u.scheme === "file",
      );
      if (!picked.length) {
        await vscode.commands.executeCommand("devsync.pullPick");
        return;
      }
      const rels = picked.map((u) => path.relative(root, u.fsPath));
      if (rels.some((r) => r.startsWith("..") || path.isAbsolute(r))) {
        throw new Error("Only files inside the devsync project can be pulled.");
      }
      await this.pullPaths(rels.some((r) => r === "") ? [] : rels);
    });

    // Pick remote files/folders from a searchable list.
    reg("devsync.pullPick", async () => {
      const root = this.requireRoot();
      type Item = vscode.QuickPickItem & { rel: string };
      const qp = vscode.window.createQuickPick<Item>();
      qp.title = "Pull from remote";
      qp.canSelectMany = true;
      qp.matchOnDescription = true;
      qp.placeholder = "Type to filter · Space to select · Enter to pull";
      const showAll = { iconPath: new vscode.ThemeIcon("list-flat"), tooltip: "Show all remote files" };
      const onlyChanged = { iconPath: new vscode.ThemeIcon("filter"), tooltip: "Show only remote changes" };
      let all = false;
      const load = async () => {
        qp.busy = true;
        qp.buttons = [all ? onlyChanged : showAll];
        qp.placeholder = all
          ? "All remote files · type to filter · Space to select · Enter to pull"
          : "Remote changes · type to filter · Space to select · Enter to pull";
        try {
          const r = await runJson<{ files: { path: string; size: number; status: string }[] }>(
            root,
            all ? ["ls"] : ["ls", "--changed"],
            120_000,
          );
          const label: Record<string, [string, string]> = {
            new: ["$(diff-added)", "only on remote"],
            changed: ["$(diff-modified)", "changed on remote"],
            edited: ["$(warning)", "changed on both sides — yours is kept, remote saved alongside"],
            same: ["$(check)", "same as yours"],
          };
          // Folders first (with how many files they contain), then files.
          const dirs = new Map<string, number>();
          for (const f of r.files) {
            const parts = f.path.split("/");
            for (let i = 1; i < parts.length; i++) {
              const d = parts.slice(0, i).join("/");
              dirs.set(d, (dirs.get(d) ?? 0) + 1);
            }
          }
          const folderItems: Item[] = [...dirs.entries()].sort().map(([d, n]) => ({
            rel: d,
            label: `$(folder) ${d}/`,
            description: `${n} file(s)`,
          }));
          const fileItems: Item[] = r.files.map((f) => ({
            rel: f.path,
            label: `${label[f.status]?.[0] ?? ""} ${f.path}`,
            description: label[f.status]?.[1] ?? f.status,
          }));
          qp.items = [
            ...(folderItems.length ? [{ rel: "", label: "Folders", kind: vscode.QuickPickItemKind.Separator }] : []),
            ...folderItems,
            ...(fileItems.length ? [{ rel: "", label: "Files", kind: vscode.QuickPickItemKind.Separator }] : []),
            ...fileItems,
          ] as Item[];
          if (!r.files.length) {
            qp.placeholder = all ? "The remote has no files yet" : "No remote changes — use the list button to see all files";
          }
        } catch (e) {
          qp.hide();
          void vscode.window.showErrorMessage(`devsync: ${e instanceof Error ? e.message : String(e)}`);
        } finally {
          qp.busy = false;
        }
      };
      qp.onDidTriggerButton(() => {
        all = !all;
        void load();
      });
      qp.onDidAccept(() => {
        const rels = qp.selectedItems.map((i) => i.rel).filter((r) => r !== "");
        qp.hide();
        if (rels.length) void this.pullPaths(rels).catch((e) => vscode.window.showErrorMessage(`devsync: ${e.message ?? e}`));
      });
      qp.onDidHide(() => qp.dispose());
      qp.show();
      await load();
    });

    reg("devsync.chooseFiles", async () => {
      this.requireRoot();
      await this.remoteFiles?.reveal();
    });
    reg("devsync.refreshRemoteFiles", () => this.remoteFiles?.refresh());

    reg("devsync.retry", () => this.retryNow());
    reg("devsync.startWatch", () => this.startWatch());
    reg("devsync.stopWatch", () => this.stopWatch());
    reg("devsync.toggleWatch", () => (this.watch?.running ? this.stopWatch() : this.startWatch()));
    reg("devsync.refresh", () => this.model?.refresh());
    reg("devsync.showPanel", () => vscode.commands.executeCommand("workbench.view.scm"));
    reg("devsync.walkthrough", () =>
      vscode.commands.executeCommand(
        "workbench.action.openWalkthrough",
        `${this.ctx.extension.id}#devsync.gettingStarted`,
        false,
      ),
    );

    reg("devsync.run", async () => {
      const cwd = this.activeDir();
      const cmd = await this.askCommand("Run on remote");
      if (cmd?.trim()) this.runInTerminal(cmd.trim(), cwd);
    });

    reg("devsync.rerun", async () => {
      const cmd = this.ctx.workspaceState.get<string>("devsync.lastCommand");
      if (!cmd) {
        await vscode.commands.executeCommand("devsync.run");
        return;
      }
      this.runInTerminal(cmd, this.ctx.workspaceState.get<string>("devsync.lastCwd") ?? this.requireRoot());
    });

    reg("devsync.runBackground", async () => {
      const cwd = this.activeDir();
      const cmd = await this.askCommand("Run on remote in the background");
      if (cmd?.trim()) await this.runInBackground(cmd.trim(), cwd);
    });

    reg("devsync.shell", () => this.terminal("devsync: remote terminal", ["shell"], this.activeDir()));
    reg("devsync.forward", () => this.terminal("devsync: ports", ["forward"], this.requireRoot()));
    reg("devsync.doctor", () => this.terminal("devsync: doctor", ["doctor"], this.requireRoot()));

    reg("devsync.status", async () => {
      const s = await runJson<Status>(this.requireRoot(), ["status"], 60_000);
      const link = s.connected
        ? `connected (${s.latency_ms} ms) to ${s.remote?.os}/${s.remote?.arch}`
        : `link down: ${s.error}`;
      const msg = `${s.host}:${s.path} — ${link}. ${s.pending} change(s) pending, ${s.running_jobs} job(s) running.`;
      this.log.appendLine(JSON.stringify(s, null, 2));
      if (await vscode.window.showInformationMessage(msg, "Details")) this.log.show();
    });

    reg("devsync.setupClaude", async () => {
      const r = await run(this.requireRoot(), ["setup", "claude"]);
      if (r.code !== 0) throw new Error(r.stderr.trim());
      void vscode.window.showInformationMessage(
        "Claude Code will now run tests, builds and installs on the remote. Restart Claude Code sessions to load the hook.",
      );
    });

    reg("devsync.collect", async () => {
      await run(this.requireRoot(), ["--quiet", "collect"], 120_000);
      await this.model?.refresh();
    });

    // Resource actions (from the Source Control view).
    reg("devsync.openJob", (a?: unknown) => {
      const r = asResource(a);
      if (r?.job) this.openOutput(r.job.id);
    });
    reg("devsync.killJob", async (a?: unknown) => {
      const r = asResource(a);
      if (!r?.job) return;
      await run(this.requireRoot(), ["kill", r.job.id]);
      this.model?.scheduleRefresh(0);
    });
    reg("devsync.rerunJob", (a?: unknown) => {
      const r = asResource(a);
      if (!r?.job) return;
      const root = this.requireRoot();
      this.runInTerminal(r.job.command, r.job.cwd ? path.join(root, r.job.cwd) : root);
    });
    reg("devsync.compareConflict", async (a?: unknown) => {
      const r = asResource(a);
      if (!r?.remoteCopy) return;
      const rel = path.relative(this.requireRoot(), r.file);
      await vscode.commands.executeCommand(
        "vscode.diff",
        vscode.Uri.file(r.file),
        vscode.Uri.file(r.remoteCopy),
        `${rel} (yours ↔ remote)`,
      );
    });
    reg("devsync.acceptRemote", async (a?: unknown) => {
      const r = asResource(a);
      if (r) await this.model?.acceptRemote(r);
    });
    reg("devsync.keepLocal", async (a?: unknown) => {
      const r = asResource(a);
      if (r) await this.model?.keepLocal(r);
    });
    reg("devsync.openFile", async (a?: unknown) => {
      const r = asResource(a);
      if (r) await vscode.window.showTextDocument(vscode.Uri.file(r.file));
    });
    reg("devsync.focusRun", async () => {
      await vscode.commands.executeCommand("devsync.run.focus");
      this.sidebar?.focusInput();
    });

    this.disposables.push(
      vscode.window.registerTerminalProfileProvider("devsync.remoteShell", {
        provideTerminalProfile: () =>
          new vscode.TerminalProfile({
            name: "devsync remote terminal",
            shellPath: binaryPath(),
            shellArgs: ["shell"],
            cwd: this.root,
            iconPath: new vscode.ThemeIcon("remote"),
          }),
      }),
      vscode.window.onDidCloseTerminal((t) => {
        if (t.name.startsWith("devsync:")) this.model?.scheduleRefresh(0);
      }),
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration("devsync.binaryPath") && this.watch?.running) {
          this.stopWatch();
          this.startWatch();
        }
      }),
      vscode.workspace.onDidCreateFiles(async (e) => {
        if (!this.root && e.files.some((f) => f.fsPath.endsWith(CONFIG_FILE))) {
          const root = await this.findRoot();
          if (root) this.activate(root);
        }
      }),
    );
  }

  dispose(): void {
    if (this.updateTimer) clearInterval(this.updateTimer);
    if (this.restartTimer) clearTimeout(this.restartTimer);
    this.watch?.dispose();
    for (const d of this.disposables) d.dispose();
  }
}

export function activate(ctx: vscode.ExtensionContext): void {
  const log = vscode.window.createOutputChannel("devsync");
  const controller = new Controller(ctx, log);
  ctx.subscriptions.push(log, controller);
  void controller.start();
  if (!ctx.globalState.get("devsync.welcomed")) {
    void ctx.globalState.update("devsync.welcomed", true);
    void vscode.commands.executeCommand(
      "workbench.action.openWalkthrough",
      `${ctx.extension.id}#devsync.gettingStarted`,
      false,
    );
  }
}

export function deactivate(): void {
  // Controller is disposed through ctx.subscriptions.
}
