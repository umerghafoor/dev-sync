// Shared state for every devsync UI (Source Control panel, Activity Bar
// sidebar, status bar): local changes not yet on the remote, conflicts, and
// remote jobs. Loaded from the CLI and refreshed on file/job events.

import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { LocalJob, runJson } from "./cli";

export type ChangeStatus = "added" | "modified" | "deleted";

/** One row in any devsync list, independent of where it is shown. */
export interface Resource {
  kind: "change" | "conflict" | "job";
  /** Local file (change/conflict) or job output file. */
  file: string;
  /** Remote copy for conflicts. */
  remoteCopy?: string;
  job?: LocalJob;
  /** URI tagged for badges (A/M/D, !, ✓/✗). */
  uri: vscode.Uri;
  label: string;
  description: string;
  tooltip: string;
  contextValue: string;
  command?: vscode.Command;
  deleted?: boolean;
}

const TAG = "devsync";

export function tagged(file: string, tag: string): vscode.Uri {
  return vscode.Uri.file(file).with({ query: `${TAG}=${tag}` });
}

/** Git-style badges for tagged URIs only, so the Explorer is unaffected. */
export class Decorations implements vscode.FileDecorationProvider {
  private readonly emitter = new vscode.EventEmitter<vscode.Uri | vscode.Uri[] | undefined>();
  readonly onDidChangeFileDecorations = this.emitter.event;

  refresh(): void {
    this.emitter.fire(undefined);
  }

  provideFileDecoration(uri: vscode.Uri): vscode.FileDecoration | undefined {
    if (!uri.query.startsWith(`${TAG}=`)) return undefined;
    const d = (badge: string, color: string, tooltip: string) =>
      new vscode.FileDecoration(badge, tooltip, new vscode.ThemeColor(color));
    switch (uri.query.slice(TAG.length + 1)) {
      case "added":
        return d("A", "gitDecoration.addedResourceForeground", "New — not on the remote yet");
      case "modified":
        return d("M", "gitDecoration.modifiedResourceForeground", "Modified — not on the remote yet");
      case "deleted":
        return d("D", "gitDecoration.deletedResourceForeground", "Deleted — will be removed on the remote");
      case "conflict":
        return d("!", "gitDecoration.conflictingResourceForeground", "Changed both here and on the remote");
      case "job-ok":
        return d("✓", "testing.iconPassed", "Finished successfully");
      case "job-failed":
        return d("✗", "testing.iconFailed", "Failed");
      case "job-running":
        return d("…", "charts.blue", "Running");
      case "job-lost":
        return d("?", "problemsWarningIcon.foreground", "Lost (remote rebooted?)");
      default:
        return undefined;
    }
  }

  dispose(): void {
    this.emitter.dispose();
  }
}

export class Model implements vscode.Disposable {
  changes: Resource[] = [];
  conflicts: Resource[] = [];
  jobs: Resource[] = [];
  runningJobs = 0;
  pendingChanges = 0;

  private readonly emitter = new vscode.EventEmitter<void>();
  /** Fires after every refresh. */
  readonly onDidChange = this.emitter.event;
  private readonly decorations = new Decorations();
  private readonly disposables: vscode.Disposable[] = [];
  private refreshing = false;
  private again = false;
  private timer?: NodeJS.Timeout;
  private poll?: NodeJS.Timeout;

  constructor(readonly root: string, private readonly log: vscode.OutputChannel) {
    const schedule = () => this.scheduleRefresh();
    const jobsWatcher = vscode.workspace.createFileSystemWatcher(
      new vscode.RelativePattern(root, ".devsync/jobs/*/status.json"),
    );
    const conflictWatcher = vscode.workspace.createFileSystemWatcher(
      new vscode.RelativePattern(root, "**/*.devsync-remote"),
    );
    for (const w of [jobsWatcher, conflictWatcher]) {
      w.onDidChange(schedule);
      w.onDidCreate(schedule);
      w.onDidDelete(schedule);
    }
    this.disposables.push(
      this.emitter,
      this.decorations,
      vscode.window.registerFileDecorationProvider(this.decorations),
      jobsWatcher,
      conflictWatcher,
      vscode.workspace.onDidSaveTextDocument(schedule),
      vscode.workspace.onDidCreateFiles(schedule),
      vscode.workspace.onDidDeleteFiles(schedule),
      vscode.workspace.onDidRenameFiles(schedule),
      vscode.window.onDidChangeWindowState((s) => s.focused && schedule()),
    );
    this.scheduleRefresh(0);
  }

  scheduleRefresh(delay = 400): void {
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
      await Promise.all([this.loadChanges(), this.loadConflicts(), this.loadJobs()]);
      void vscode.commands.executeCommand("setContext", "devsync.hasConflicts", this.conflicts.length > 0);
      this.decorations.refresh();
      this.emitter.fire();
    } finally {
      this.refreshing = false;
      if (this.again) {
        this.again = false;
        this.scheduleRefresh(200);
      }
    }
    if (this.poll) clearTimeout(this.poll);
    if (this.runningJobs > 0) this.poll = setTimeout(() => this.scheduleRefresh(0), 3000);
  }

  private async loadChanges(): Promise<void> {
    try {
      const r = await runJson<{ changes: { path: string; status: ChangeStatus }[] }>(this.root, ["changes"], 60_000);
      this.pendingChanges = r.changes.length;
      this.changes = r.changes.slice(0, 2000).map((c) => {
        const file = path.join(this.root, c.path);
        const dir = path.dirname(c.path);
        return {
          kind: "change",
          file,
          uri: tagged(file, c.status),
          label: path.basename(c.path),
          description: dir === "." ? "" : dir,
          tooltip: `${c.path} — ${c.status}, not yet on the remote`,
          contextValue: `change-${c.status}`,
          deleted: c.status === "deleted",
          command:
            c.status === "deleted"
              ? undefined
              : { command: "vscode.open", title: "Open", arguments: [vscode.Uri.file(file)] },
        };
      });
    } catch (e) {
      this.log.appendLine(`changes: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  private async loadConflicts(): Promise<void> {
    const found = await vscode.workspace.findFiles(
      new vscode.RelativePattern(this.root, "**/*.devsync-remote"),
      "**/{node_modules,.git,.devsync,.venv}/**",
      500,
    );
    this.conflicts = found
      .map((u) => u.fsPath)
      .sort()
      .map((remoteCopy) => {
        const file = remoteCopy.slice(0, -".devsync-remote".length);
        const rel = path.relative(this.root, file);
        const dir = path.dirname(rel);
        return {
          kind: "conflict",
          file,
          remoteCopy,
          uri: tagged(file, "conflict"),
          label: path.basename(file),
          description: dir === "." ? "" : dir,
          tooltip: `${rel} — changed here and by a remote command. Click to compare.`,
          contextValue: "conflict",
          command: {
            command: "vscode.diff",
            title: "Compare",
            arguments: [vscode.Uri.file(file), vscode.Uri.file(remoteCopy), `${rel} (yours ↔ remote)`],
          },
        };
      });
  }

  private async loadJobs(): Promise<void> {
    try {
      const jobs = await runJson<LocalJob[]>(this.root, ["jobs"], 60_000);
      this.runningJobs = jobs.filter((j) => j.state === "running").length;
      this.jobs = jobs.slice(0, 20).map((j) => {
        const state =
          j.state === "running"
            ? "job-running"
            : j.state === "lost"
              ? "job-lost"
              : j.exit_code === 0
                ? "job-ok"
                : "job-failed";
        const label = (j.command || j.id).replace(/\s+/g, " ").slice(0, 80);
        // Virtual file named after the command: list views label resources by file name.
        const virtual = path.join(this.root, ".devsync", "jobs", j.id, label.replace(/\//g, "∕"));
        const status = j.state === "running" ? "running" : j.state === "lost" ? "lost" : `exit ${j.exit_code ?? "?"}`;
        return {
          kind: "job",
          file: j.output,
          job: j,
          uri: tagged(virtual, state),
          label,
          description: `${status}${j.cwd ? ` · ./${j.cwd}` : ""}${j.started_unix ? ` · ${ago(j.started_unix)}` : ""}`,
          tooltip: `${j.command}\n${status} · ./${j.cwd} · job ${j.id}`,
          contextValue: state,
          command: { command: "vscode.open", title: "Open output", arguments: [vscode.Uri.file(j.output)] },
        };
      });
    } catch (e) {
      this.log.appendLine(`jobs: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  /** Conflict resolution: take the remote version. */
  async acceptRemote(r: Resource): Promise<void> {
    if (!r.remoteCopy) return;
    await fs.promises.copyFile(r.remoteCopy, r.file);
    await fs.promises.unlink(r.remoteCopy);
    this.scheduleRefresh(0);
  }

  /** Conflict resolution: keep yours, drop the remote copy. */
  async keepLocal(r: Resource): Promise<void> {
    if (!r.remoteCopy) return;
    const rel = path.relative(this.root, r.file);
    const ok = await vscode.window.showWarningMessage(
      `Discard the remote version of ${rel}?`,
      { modal: true, detail: "Your local file is kept. The remote command's version is deleted." },
      "Keep Mine",
    );
    if (ok !== "Keep Mine") return;
    await fs.promises.unlink(r.remoteCopy);
    this.scheduleRefresh(0);
  }

  dispose(): void {
    if (this.timer) clearTimeout(this.timer);
    if (this.poll) clearTimeout(this.poll);
    for (const d of this.disposables) d.dispose();
  }
}

function ago(unix: number): string {
  const s = Math.max(0, Math.floor(Date.now() / 1000) - unix);
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}

/** Commands get either a Resource, an SCM resource state or a tree item
 *  wrapping one; normalise to the Resource. */
export function asResource(arg: unknown): Resource | undefined {
  if (!arg || typeof arg !== "object") return undefined;
  const a = arg as { res?: Resource; kind?: string };
  if (a.res) return a.res;
  if (a.kind === "change" || a.kind === "conflict" || a.kind === "job") return arg as Resource;
  return undefined;
}
