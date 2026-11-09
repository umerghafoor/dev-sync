// Source Control view for devsync, modelled on the Git view:
//
//   [ command to run on remote … ]  (⌘Enter runs it, like committing)
//   Not on remote yet   A/M/D files waiting to be pushed      (Push)
//   Conflicts           remote edits that collided with yours (Compare / Accept / Keep)
//   Remote jobs         recent commands with ✓ / ✗ / …        (Open / Stop / Run again)

import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { LocalJob, runJson } from "./cli";

type ChangeStatus = "added" | "modified" | "deleted";

interface ChangesReply {
  changes: { path: string; status: ChangeStatus }[];
}

const DECO_QUERY = "devsync";

/** Resource state carrying what the context-menu commands need. */
export interface DevsyncResource extends vscode.SourceControlResourceState {
  kind: "change" | "conflict" | "job";
  /** Local file (change/conflict) or job output file. */
  file: string;
  /** Remote copy for conflicts. */
  remoteCopy?: string;
  job?: LocalJob;
}

function tagged(file: string, tag: string): vscode.Uri {
  return vscode.Uri.file(file).with({ query: `${DECO_QUERY}=${tag}` });
}

/** Badges for the resources above, like Git's A/M/D letters. Only URIs we
 *  tag are decorated, so the Explorer is unaffected. */
class Decorations implements vscode.FileDecorationProvider {
  private readonly emitter = new vscode.EventEmitter<vscode.Uri | vscode.Uri[] | undefined>();
  readonly onDidChangeFileDecorations = this.emitter.event;

  refresh(): void {
    this.emitter.fire(undefined);
  }

  provideFileDecoration(uri: vscode.Uri): vscode.FileDecoration | undefined {
    if (!uri.query.startsWith(`${DECO_QUERY}=`)) return undefined;
    const tag = uri.query.slice(DECO_QUERY.length + 1);
    const deco = (badge: string, color: string, tooltip: string) =>
      new vscode.FileDecoration(badge, tooltip, new vscode.ThemeColor(color));
    switch (tag) {
      case "added":
        return deco("A", "gitDecoration.addedResourceForeground", "New — not on the remote yet");
      case "modified":
        return deco("M", "gitDecoration.modifiedResourceForeground", "Modified — not on the remote yet");
      case "deleted":
        return deco("D", "gitDecoration.deletedResourceForeground", "Deleted — will be removed on the remote");
      case "conflict":
        return deco("!", "gitDecoration.conflictingResourceForeground", "Changed both here and on the remote");
      case "job-ok":
        return deco("✓", "testing.iconPassed", "Finished successfully");
      case "job-failed":
        return deco("✗", "testing.iconFailed", "Failed");
      case "job-running":
        return deco("…", "charts.blue", "Running");
      case "job-lost":
        return deco("?", "problemsWarningIcon.foreground", "Lost (remote rebooted?)");
      default:
        return undefined;
    }
  }

  dispose(): void {
    this.emitter.dispose();
  }
}

export class DevsyncScm implements vscode.Disposable {
  readonly sc: vscode.SourceControl;
  private readonly changes: vscode.SourceControlResourceGroup;
  private readonly conflicts: vscode.SourceControlResourceGroup;
  private readonly jobs: vscode.SourceControlResourceGroup;
  private readonly decorations = new Decorations();
  private readonly disposables: vscode.Disposable[] = [];
  private refreshing = false;
  private again = false;
  private timer?: NodeJS.Timeout;
  private poll?: NodeJS.Timeout;
  runningJobs = 0;
  pendingChanges = 0;

  constructor(private readonly root: string, private readonly log: vscode.OutputChannel) {
    this.sc = vscode.scm.createSourceControl("devsync", "devsync", vscode.Uri.file(root));
    this.sc.inputBox.placeholder = `Command to run on the remote (${process.platform === "darwin" ? "⌘" : "Ctrl+"}Enter)`;
    this.sc.acceptInputCommand = { command: "devsync.scmRun", title: "Run on remote" };

    this.changes = this.sc.createResourceGroup("changes", "Not on remote yet");
    this.changes.hideWhenEmpty = false;
    this.conflicts = this.sc.createResourceGroup("conflicts", "Conflicts");
    this.conflicts.hideWhenEmpty = true;
    this.jobs = this.sc.createResourceGroup("jobs", "Remote jobs");
    this.jobs.hideWhenEmpty = true;

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
      this.sc,
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

  get input(): vscode.SourceControlInputBox {
    return this.sc.inputBox;
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
      this.sc.count = this.pendingChanges + this.conflicts.resourceStates.length;
      this.decorations.refresh();
    } finally {
      this.refreshing = false;
      if (this.again) {
        this.again = false;
        this.scheduleRefresh(200);
      }
    }
    // While jobs run, keep the list fresh even without other events.
    if (this.poll) clearTimeout(this.poll);
    if (this.runningJobs > 0) this.poll = setTimeout(() => this.scheduleRefresh(0), 3000);
  }

  private async loadChanges(): Promise<void> {
    try {
      const r = await runJson<ChangesReply>(this.root, ["changes"], 60_000);
      this.pendingChanges = r.changes.length;
      this.changes.resourceStates = r.changes.slice(0, 2000).map((c): DevsyncResource => {
        const file = path.join(this.root, c.path);
        return {
          kind: "change",
          file,
          resourceUri: tagged(file, c.status),
          contextValue: `change-${c.status}`,
          decorations: {
            strikeThrough: c.status === "deleted",
            faded: c.status === "deleted",
            tooltip: `${c.path} — ${c.status}, not yet on the remote`,
          },
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
    this.conflicts.resourceStates = found
      .map((u) => u.fsPath)
      .sort()
      .map((remoteCopy): DevsyncResource => {
        const file = remoteCopy.slice(0, -".devsync-remote".length);
        const rel = path.relative(this.root, file);
        return {
          kind: "conflict",
          file,
          remoteCopy,
          resourceUri: tagged(file, "conflict"),
          contextValue: "conflict",
          decorations: { tooltip: `${rel} — changed here and by a remote command. Click to compare.` },
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
      this.jobs.resourceStates = jobs.slice(0, 20).map((j): DevsyncResource => {
        const state =
          j.state === "running" ? "job-running" : j.state === "lost" ? "job-lost" : j.exit_code === 0 ? "job-ok" : "job-failed";
        // The SCM view labels resources by file name, so name the
        // (virtual) resource after the command and keep the id as its folder.
        const label = (j.command || j.id).replace(/\//g, "∕").replace(/\s+/g, " ").slice(0, 80);
        const virtual = path.join(this.root, ".devsync", "jobs", j.id, label);
        const status =
          j.state === "running" ? "running" : j.state === "lost" ? "lost" : `exit ${j.exit_code ?? "?"}`;
        return {
          kind: "job",
          file: j.output,
          job: j,
          resourceUri: tagged(virtual, state),
          contextValue: state,
          decorations: {
            tooltip: `${j.command}\n${status} · ./${j.cwd} · job ${j.id}`,
            faded: j.state === "lost",
          },
          command: { command: "vscode.open", title: "Open output", arguments: [vscode.Uri.file(j.output)] },
        };
      });
    } catch (e) {
      this.log.appendLine(`jobs: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  /** Conflict resolution: take the remote version. */
  async acceptRemote(r: DevsyncResource): Promise<void> {
    if (!r.remoteCopy) return;
    await fs.promises.copyFile(r.remoteCopy, r.file);
    await fs.promises.unlink(r.remoteCopy);
    this.scheduleRefresh(0);
  }

  /** Conflict resolution: keep yours, drop the remote copy. */
  async keepLocal(r: DevsyncResource): Promise<void> {
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
