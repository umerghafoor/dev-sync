// "Remote Jobs" tree view backed by `devsync --json jobs` (the local results
// folder .devsync/jobs/), refreshed when that folder changes.

import * as path from "path";
import * as vscode from "vscode";
import { LocalJob, runJson } from "./cli";

export class JobItem extends vscode.TreeItem {
  constructor(public readonly job: LocalJob, root: string) {
    super(job.command || job.id, vscode.TreeItemCollapsibleState.None);
    const when = job.started_unix ? new Date(job.started_unix * 1000).toLocaleTimeString() : "";
    const where = job.cwd ? `./${job.cwd}` : ".";
    this.id = job.id;
    this.contextValue = job.state === "running" ? "job-running" : "job-done";
    this.description = `${statusText(job)} · ${where} · ${when}`;
    this.tooltip = new vscode.MarkdownString(
      `**${escapeMd(job.command)}**\n\n` +
        `- job: \`${job.id}\`\n- state: ${statusText(job)}\n- dir: \`${where}\`\n- host: \`${job.host}\`\n` +
        `- output: \`${path.relative(root, job.output)}\``,
    );
    this.iconPath = icon(job);
    this.command = { command: "devsync.openJob", title: "Open Output", arguments: [this] };
  }
}

function escapeMd(s: string): string {
  return s.replace(/[\\`*_{}[\]()#+\-.!|]/g, "\\$&");
}

function statusText(j: LocalJob): string {
  if (j.state === "running") return "running";
  if (j.state === "lost") return "lost";
  return j.exit_code === 0 ? "ok" : `exit ${j.exit_code}`;
}

function icon(j: LocalJob): vscode.ThemeIcon {
  if (j.state === "running") return new vscode.ThemeIcon("sync~spin");
  if (j.state === "lost") return new vscode.ThemeIcon("question", new vscode.ThemeColor("problemsWarningIcon.foreground"));
  return j.exit_code === 0
    ? new vscode.ThemeIcon("pass", new vscode.ThemeColor("testing.iconPassed"))
    : new vscode.ThemeIcon("error", new vscode.ThemeColor("testing.iconFailed"));
}

export class JobsProvider implements vscode.TreeDataProvider<JobItem>, vscode.Disposable {
  private readonly emitter = new vscode.EventEmitter<JobItem | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;
  private jobs: LocalJob[] = [];
  private readonly watcher: vscode.FileSystemWatcher;
  private pending?: NodeJS.Timeout;

  constructor(private readonly root: string, private readonly log: vscode.OutputChannel) {
    this.watcher = vscode.workspace.createFileSystemWatcher(
      new vscode.RelativePattern(root, ".devsync/jobs/*/status.json"),
    );
    const schedule = () => this.scheduleRefresh();
    this.watcher.onDidChange(schedule);
    this.watcher.onDidCreate(schedule);
    this.watcher.onDidDelete(schedule);
  }

  get runningCount(): number {
    return this.jobs.filter((j) => j.state === "running").length;
  }

  scheduleRefresh(): void {
    if (this.pending) clearTimeout(this.pending);
    this.pending = setTimeout(() => void this.refresh(), 250);
  }

  async refresh(): Promise<void> {
    try {
      this.jobs = await runJson<LocalJob[]>(this.root, ["jobs"]);
    } catch (e) {
      this.log.appendLine(`jobs: ${e instanceof Error ? e.message : String(e)}`);
    }
    this.emitter.fire(undefined);
  }

  getTreeItem(el: JobItem): vscode.TreeItem {
    return el;
  }

  getChildren(): JobItem[] {
    return this.jobs.slice(0, 50).map((j) => new JobItem(j, this.root));
  }

  dispose(): void {
    this.watcher.dispose();
    this.emitter.dispose();
    if (this.pending) clearTimeout(this.pending);
  }
}
