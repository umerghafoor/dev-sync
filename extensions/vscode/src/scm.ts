// devsync in the Source Control view: ONE entry per project that works like
// VS Code's Git, for the remote repository (commit box, Merge / Staged /
// Changes), plus devsync's own lists (files not synced yet, conflicts,
// remote jobs), which appear only when they have something to show.
//
// When the remote project is not a git repository, the input box runs
// commands on the remote instead (⌘Enter), as before.

import * as vscode from "vscode";
import { Model, Resource } from "./model";

type State = vscode.SourceControlResourceState & { res: Resource; kind: Resource["kind"] };

function toState(r: Resource): State {
  return {
    res: r,
    kind: r.kind,
    resourceUri: r.uri,
    contextValue: r.contextValue,
    command: r.command,
    decorations: { tooltip: r.tooltip, strikeThrough: r.deleted, faded: r.deleted || r.contextValue === "job-lost" },
  };
}

export interface GitGroups {
  merge: vscode.SourceControlResourceGroup;
  index: vscode.SourceControlResourceGroup;
  workingTree: vscode.SourceControlResourceGroup;
}

export class DevsyncScm implements vscode.Disposable {
  readonly sc: vscode.SourceControl;
  /** Filled by RemoteGit when the remote project is a git repository. */
  readonly git: GitGroups;
  private readonly changes: vscode.SourceControlResourceGroup;
  private readonly conflicts: vscode.SourceControlResourceGroup;
  private readonly jobs: vscode.SourceControlResourceGroup;
  private readonly sub: vscode.Disposable;
  private gitMode = false;
  private gitCount = 0;

  constructor(root: string, private readonly model: Model) {
    this.sc = vscode.scm.createSourceControl("devsync", "Remote", vscode.Uri.file(root));
    // Groups appear in creation order: git first, like VS Code's Git view.
    this.git = {
      merge: this.sc.createResourceGroup("merge", "Merge Changes"),
      index: this.sc.createResourceGroup("index", "Staged Changes"),
      workingTree: this.sc.createResourceGroup("workingTree", "Changes"),
    };
    for (const g of Object.values(this.git)) g.hideWhenEmpty = true;
    this.changes = this.sc.createResourceGroup("changes", "Not on Remote Yet");
    this.conflicts = this.sc.createResourceGroup("conflicts", "Conflicts");
    this.jobs = this.sc.createResourceGroup("jobs", "Remote Jobs");
    this.changes.hideWhenEmpty = true;
    this.conflicts.hideWhenEmpty = true;
    this.jobs.hideWhenEmpty = true;
    this.setGitMode(false);
    this.sub = model.onDidChange(() => this.render());
    this.render();
  }

  get input(): vscode.SourceControlInputBox {
    return this.sc.inputBox;
  }

  get isGitMode(): boolean {
    return this.gitMode;
  }

  /** Git repository on the remote: the input box commits; otherwise it runs commands. */
  setGitMode(on: boolean): void {
    this.gitMode = on;
    const mod = process.platform === "darwin" ? "⌘" : "Ctrl+";
    if (on) {
      this.sc.inputBox.placeholder = `Message (${mod}Enter to commit on the remote)`;
      this.sc.acceptInputCommand = { command: "devsync.git.commit", title: "Commit" };
    } else {
      this.sc.inputBox.placeholder = `Command to run on the remote (${mod}Enter)`;
      this.sc.acceptInputCommand = { command: "devsync.scmRun", title: "Run on remote" };
      for (const g of Object.values(this.git)) g.resourceStates = [];
      this.sc.statusBarCommands = [];
      this.gitCount = 0;
    }
    this.render();
  }

  setGitCount(n: number): void {
    this.gitCount = n;
    this.render();
  }

  private render(): void {
    this.changes.resourceStates = this.model.changes.map(toState);
    this.conflicts.resourceStates = this.model.conflicts.map(toState);
    this.jobs.resourceStates = this.model.jobs.map(toState);
    this.sc.count = this.gitCount + this.model.pendingChanges + this.model.conflicts.length;
  }

  dispose(): void {
    this.sub.dispose();
    this.sc.dispose();
  }
}
