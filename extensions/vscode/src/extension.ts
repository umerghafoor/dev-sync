// devsync for VS Code: status bar sync state, auto sync, remote commands in
// terminals, a Remote Jobs view over the local results folder, and setup
// helpers. Everything is delegated to the devsync CLI.

import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { binaryPath, DevsyncEvent, DevsyncNotFound, EventStream, run, runJson, Status } from "./cli";
import { JobItem, JobsProvider } from "./jobs";

const CONFIG_FILE = ".devsync.toml";

type SyncState = "idle" | "pending" | "syncing" | "synced" | "offline" | "error" | "stopped";

class Controller implements vscode.Disposable {
  private root?: string;
  private readonly status: vscode.StatusBarItem;
  private watch?: EventStream;
  private jobs?: JobsProvider;
  private readonly disposables: vscode.Disposable[] = [];
  private collectTimer?: NodeJS.Timeout;
  private restartTimer?: NodeJS.Timeout;
  private restartDelay = 2000;
  private lastSync?: Date;
  private state: SyncState = "stopped";
  private detail = "";
  private readonly watchedJobs = new Set<string>();

  constructor(private readonly ctx: vscode.ExtensionContext, private readonly log: vscode.OutputChannel) {
    this.status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 50);
    this.status.command = "devsync.menu";
    this.disposables.push(this.status);
  }

  async start(): Promise<void> {
    this.registerCommands();
    const root = await this.findRoot();
    if (root) {
      this.activate(root);
    }
  }

  // ------------------------------------------------------------ project

  private async findRoot(): Promise<string | undefined> {
    for (const f of vscode.workspace.workspaceFolders ?? []) {
      if (fs.existsSync(path.join(f.uri.fsPath, CONFIG_FILE))) {
        return f.uri.fsPath;
      }
    }
    const found = await vscode.workspace.findFiles(`**/${CONFIG_FILE}`, "**/node_modules/**", 1);
    return found[0] ? path.dirname(found[0].fsPath) : undefined;
  }

  private activate(root: string): void {
    if (this.root === root) return;
    this.root = root;
    void vscode.commands.executeCommand("setContext", "devsync.active", true);
    this.jobs = new JobsProvider(root, this.log);
    this.disposables.push(this.jobs, vscode.window.registerTreeDataProvider("devsync.jobs", this.jobs));
    void this.jobs.refresh();
    this.status.show();
    this.setState("stopped");
    if (vscode.workspace.getConfiguration("devsync").get<boolean>("autoSync", true)) {
      this.startWatch();
    }
    // Without the watcher, still collect results of running jobs.
    this.collectTimer = setInterval(() => {
      if (this.jobs && this.jobs.runningCount > 0 && !this.watch?.running) {
        void run(root, ["--quiet", "collect"]).then(() => this.jobs?.scheduleRefresh());
      }
    }, 3000);
    this.log.appendLine(`devsync project: ${root} (binary: ${binaryPath()})`);
  }

  private requireRoot(): string {
    if (!this.root) {
      throw new Error("No devsync project in this workspace. Run “devsync: Initialize Project…” first.");
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
    const icons: Record<SyncState, string> = {
      idle: "$(cloud)",
      pending: "$(circle-filled)",
      syncing: "$(sync~spin)",
      synced: "$(check)",
      offline: "$(cloud-offline)",
      error: "$(error)",
      stopped: "$(debug-pause)",
    };
    const labels: Record<SyncState, string> = {
      idle: "devsync",
      pending: "devsync: changes",
      syncing: "devsync: syncing",
      synced: "devsync",
      offline: "devsync: offline",
      error: "devsync: error",
      stopped: "devsync: paused",
    };
    const running = this.jobs?.runningCount ?? 0;
    this.status.text = `${icons[state]} ${labels[state]}${running ? ` · ${running} running` : ""}`;
    const tip = new vscode.MarkdownString();
    tip.appendMarkdown(`**devsync** — ${state}\n\n`);
    if (detail) tip.appendText(`${detail}\n\n`);
    if (this.lastSync) tip.appendMarkdown(`Last sync: ${this.lastSync.toLocaleTimeString()}\n\n`);
    tip.appendMarkdown("Click for actions");
    this.status.tooltip = tip;
    this.status.backgroundColor =
      state === "error"
        ? new vscode.ThemeColor("statusBarItem.errorBackground")
        : state === "offline"
          ? new vscode.ThemeColor("statusBarItem.warningBackground")
          : undefined;
  }

  private onEvent(e: DevsyncEvent): void {
    switch (e.event) {
      case "watching":
        this.setState("idle");
        break;
      case "changed":
        this.setState("pending", `${e.paths} change(s) waiting to be sent`);
        break;
      case "planned":
        if (e.upload > 0 || e.delete > 0) this.setState("syncing", `${e.upload} to upload, ${e.delete} to delete`);
        break;
      case "synced":
        this.lastSync = new Date();
        this.restartDelay = 2000;
        this.setState("synced", `sent ${e.files} file(s)`);
        this.log.appendLine(`synced ${e.files} file(s), ${e.bytes} bytes, ${e.deleted} deleted in ${e.millis} ms`);
        break;
      case "up_to_date":
        this.lastSync = new Date();
        this.setState("synced", "remote is up to date");
        break;
      case "retrying":
      case "offline":
        this.setState("offline", e.event === "offline" ? e.error : e.error);
        break;
      case "pulled":
        this.log.appendLine(`pulled back ${e.path}`);
        break;
      case "conflict":
        void this.showConflict(e.path, e.saved_as);
        break;
      case "job_updated":
        this.jobs?.scheduleRefresh();
        break;
      case "job_finished":
        this.jobs?.scheduleRefresh();
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
    if (!this.watchedJobs.has(id)) return; // only background jobs started from here
    this.watchedJobs.delete(id);
    const ok = state === "exited" && code === 0;
    const msg = ok ? `Remote job ${id} finished successfully.` : `Remote job ${id} ${state === "lost" ? "was lost" : `failed (exit ${code})`}.`;
    const pick = await (ok ? vscode.window.showInformationMessage(msg, "Open Output") : vscode.window.showWarningMessage(msg, "Open Output"));
    if (pick) this.openOutput(id);
  }

  private async showConflict(rel: string, savedAs: string): Promise<void> {
    const root = this.requireRoot();
    const pick = await vscode.window.showWarningMessage(
      `${rel} was changed both here and by a remote command. The remote version was saved as ${savedAs}.`,
      "Compare",
    );
    if (pick === "Compare") {
      await vscode.commands.executeCommand(
        "vscode.diff",
        vscode.Uri.file(path.join(root, rel)),
        vscode.Uri.file(path.join(root, savedAs)),
        `${rel} (local ↔ remote)`,
      );
    }
  }

  // --------------------------------------------------------------- watch

  private startWatch(): void {
    const root = this.requireRoot();
    if (this.watch?.running) return;
    this.watch?.dispose();
    this.watch = new EventStream(
      root,
      ["watch"],
      (e) => this.onEvent(e),
      (code, stderr) => {
        const msg = stderr.trim().split("\n").pop() || `exited with ${code}`;
        this.log.appendLine(`watch stopped: ${msg}`);
        this.setState("error", msg);
        if (msg.includes("not found")) {
          void this.binaryMissing();
          return;
        }
        // Restart with backoff; the CLI already handles network errors, so
        // this only covers crashes and config errors being fixed.
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
    this.setState("stopped", "auto sync is off");
  }

  private async binaryMissing(): Promise<void> {
    const pick = await vscode.window.showErrorMessage(
      "devsync binary not found. Install it (cargo install --path crates/devsync-cli) or set devsync.binaryPath.",
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
    void this.ctx.workspaceState.update("devsync.lastCommand", command);
    void this.ctx.workspaceState.update("devsync.lastCwd", cwd);
    const short = command.length > 40 ? `${command.slice(0, 37)}…` : command;
    this.terminal(`devsync: ${short}`, ["exec", "--", command], cwd);
  }

  private openOutput(id: string): void {
    const root = this.requireRoot();
    const file = path.join(root, ".devsync", "jobs", id, "output.log");
    void vscode.window.showTextDocument(vscode.Uri.file(file), { preview: true, preserveFocus: false });
  }

  private async askCommand(title: string): Promise<string | undefined> {
    return vscode.window.showInputBox({
      title,
      prompt: "Runs on the remote in the directory matching the active file (after syncing).",
      value: this.ctx.workspaceState.get<string>("devsync.lastCommand", ""),
      placeHolder: "e.g. pytest -x  ·  npm test  ·  make",
      ignoreFocusOut: true,
    });
  }

  // ------------------------------------------------------------ commands

  private registerCommands(): void {
    const reg = (id: string, fn: (...args: any[]) => unknown) =>
      this.disposables.push(
        vscode.commands.registerCommand(id, async (...args: unknown[]) => {
          try {
            await fn(...args);
          } catch (e) {
            if (e instanceof DevsyncNotFound) {
              await this.binaryMissing();
            } else {
              void vscode.window.showErrorMessage(`devsync: ${e instanceof Error ? e.message : String(e)}`);
            }
          }
        }),
      );

    reg("devsync.menu", async () => {
      const items: (vscode.QuickPickItem & { cmd: string })[] = [
        { label: "$(play) Run command remotely…", cmd: "devsync.run" },
        { label: "$(debug-rerun) Re-run last command", cmd: "devsync.rerun", description: this.ctx.workspaceState.get("devsync.lastCommand") },
        { label: "$(run-all) Run in background…", cmd: "devsync.runBackground" },
        { label: "$(terminal) Open remote shell", cmd: "devsync.shell" },
        { label: "$(cloud-upload) Push now", cmd: "devsync.push" },
        { label: this.watch?.running ? "$(debug-pause) Stop auto sync" : "$(sync) Start auto sync", cmd: "devsync.toggleWatch" },
        { label: "$(cloud-download) Collect job results", cmd: "devsync.collect" },
        { label: "$(plug) Forward ports", cmd: "devsync.forward" },
        { label: "$(info) Status", cmd: "devsync.status" },
        { label: "$(pulse) Doctor", cmd: "devsync.doctor" },
        { label: "$(sparkle) Set up Claude Code integration", cmd: "devsync.setupClaude" },
      ];
      const pick = await vscode.window.showQuickPick(items, { title: `devsync — ${this.state}${this.detail ? `: ${this.detail}` : ""}` });
      if (pick) await vscode.commands.executeCommand(pick.cmd);
    });

    reg("devsync.init", async () => {
      const folder = vscode.workspace.workspaceFolders?.[0];
      if (!folder) throw new Error("Open a folder first.");
      const target = await vscode.window.showInputBox({
        title: "devsync: remote target",
        prompt: "user@host:/path/on/remote  (an ~/.ssh/config alias works as host; ~ is the remote home)",
        placeHolder: "me@devbox:~/projects/" + path.basename(folder.uri.fsPath),
        ignoreFocusOut: true,
        validateInput: (v) => (/^[^:\s]+:\S+$/.test(v.trim()) ? undefined : "Expected host:path"),
      });
      if (!target) return;
      const r = await run(folder.uri.fsPath, ["init", target.trim()]);
      if (r.code !== 0) throw new Error(r.stderr.trim());
      this.activate(folder.uri.fsPath);
      const pick = await vscode.window.showInformationMessage(
        "devsync is set up. Coding agents can run commands on the remote too.",
        "Set Up Claude Code",
        "Run Doctor",
      );
      if (pick === "Set Up Claude Code") await vscode.commands.executeCommand("devsync.setupClaude");
      if (pick === "Run Doctor") await vscode.commands.executeCommand("devsync.doctor");
    });

    reg("devsync.push", async () => {
      const root = this.requireRoot();
      await vscode.window.withProgress(
        { location: vscode.ProgressLocation.Notification, title: "devsync: pushing…", cancellable: false },
        async () => {
          const r = await run(root, ["push"], 600_000);
          if (r.code !== 0) throw new Error(r.stderr.trim().split("\n").pop());
          this.lastSync = new Date();
          this.setState("synced", r.stderr.trim().split("\n").pop() ?? "");
        },
      );
    });

    reg("devsync.toggleWatch", () => (this.watch?.running ? this.stopWatch() : this.startWatch()));

    reg("devsync.run", async () => {
      const cwd = this.activeDir();
      const cmd = await this.askCommand("Run command remotely");
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
      const cmd = await this.askCommand("Run command remotely in the background");
      if (!cmd?.trim()) return;
      void this.ctx.workspaceState.update("devsync.lastCommand", cmd.trim());
      const r = await runJson<{ id: string; output: string }>(cwd, ["submit", "--", cmd.trim()], 600_000);
      this.watchedJobs.add(r.id);
      this.jobs?.scheduleRefresh();
      if (vscode.workspace.getConfiguration("devsync").get<boolean>("openOutputOnBackgroundRun", true)) {
        this.openOutput(r.id);
      }
      void vscode.window.setStatusBarMessage(`devsync: started job ${r.id}`, 4000);
    });

    reg("devsync.shell", () => this.terminal("devsync: remote shell", ["shell"], this.activeDir()));
    reg("devsync.forward", () => this.terminal("devsync: ports", ["forward"], this.requireRoot()));
    reg("devsync.doctor", () => this.terminal("devsync: doctor", ["doctor"], this.requireRoot()));

    reg("devsync.status", async () => {
      const s = await runJson<Status>(this.requireRoot(), ["status"], 60_000);
      const link = s.connected
        ? `connected (${s.latency_ms} ms) to ${s.remote?.os}/${s.remote?.arch}`
        : `link down: ${s.error}`;
      const msg = `${s.host}:${s.path} — ${link}. ${s.pending} change(s) pending, ${s.running_jobs} job(s) running.`;
      this.log.appendLine(JSON.stringify(s, null, 2));
      void vscode.window.showInformationMessage(msg, "Show Details").then((p) => p && this.log.show());
    });

    reg("devsync.setupClaude", async () => {
      const r = await run(this.requireRoot(), ["setup", "claude"]);
      if (r.code !== 0) throw new Error(r.stderr.trim());
      void vscode.window.showInformationMessage(
        "Claude Code will now run code-executing commands on the remote. Restart Claude Code sessions to load the hook.",
      );
    });

    reg("devsync.collect", async () => {
      await run(this.requireRoot(), ["--quiet", "collect"], 120_000);
      await this.jobs?.refresh();
    });

    reg("devsync.refreshJobs", () => this.jobs?.refresh());

    reg("devsync.openJob", (item?: JobItem | string) => {
      const id = typeof item === "string" ? item : item?.job.id;
      if (id) this.openOutput(id);
    });

    reg("devsync.killJob", async (item?: JobItem) => {
      if (!item) return;
      await run(this.requireRoot(), ["kill", item.job.id]);
      await this.jobs?.refresh();
    });

    reg("devsync.rerunJob", (item?: JobItem) => {
      if (!item) return;
      const root = this.requireRoot();
      this.runInTerminal(item.job.command, item.job.cwd ? path.join(root, item.job.cwd) : root);
    });

    this.disposables.push(
      vscode.window.registerTerminalProfileProvider("devsync.remoteShell", {
        provideTerminalProfile: () =>
          new vscode.TerminalProfile({
            name: "devsync remote shell",
            shellPath: binaryPath(),
            shellArgs: ["shell"],
            cwd: this.root,
            iconPath: new vscode.ThemeIcon("remote"),
          }),
      }),
      vscode.window.onDidCloseTerminal((t) => {
        if (t.name.startsWith("devsync:")) this.jobs?.scheduleRefresh();
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
    if (this.collectTimer) clearInterval(this.collectTimer);
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
}

export function deactivate(): void {
  // Controller is disposed through ctx.subscriptions.
}
