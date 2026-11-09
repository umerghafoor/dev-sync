// devsync in the Source Control view, next to Git: a command box (⌘Enter
// runs on the remote, like committing) and the shared lists as resource
// groups.

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

export class DevsyncScm implements vscode.Disposable {
  readonly sc: vscode.SourceControl;
  private readonly changes: vscode.SourceControlResourceGroup;
  private readonly conflicts: vscode.SourceControlResourceGroup;
  private readonly jobs: vscode.SourceControlResourceGroup;
  private readonly sub: vscode.Disposable;

  constructor(root: string, private readonly model: Model) {
    this.sc = vscode.scm.createSourceControl("devsync", "devsync", vscode.Uri.file(root));
    this.sc.inputBox.placeholder = `Command to run on the remote (${process.platform === "darwin" ? "⌘" : "Ctrl+"}Enter)`;
    this.sc.acceptInputCommand = { command: "devsync.scmRun", title: "Run on remote" };
    this.changes = this.sc.createResourceGroup("changes", "Not on remote yet");
    this.changes.hideWhenEmpty = false;
    this.conflicts = this.sc.createResourceGroup("conflicts", "Conflicts");
    this.conflicts.hideWhenEmpty = true;
    this.jobs = this.sc.createResourceGroup("jobs", "Remote jobs");
    this.jobs.hideWhenEmpty = true;
    this.sub = model.onDidChange(() => this.render());
    this.render();
  }

  get input(): vscode.SourceControlInputBox {
    return this.sc.inputBox;
  }

  private render(): void {
    this.changes.resourceStates = this.model.changes.map(toState);
    this.conflicts.resourceStates = this.model.conflicts.map(toState);
    this.jobs.resourceStates = this.model.jobs.map(toState);
    this.sc.count = this.model.pendingChanges + this.model.conflicts.length;
  }

  dispose(): void {
    this.sub.dispose();
    this.sc.dispose();
  }
}
