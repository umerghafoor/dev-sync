// "Remote Files": browse the remote project one folder at a time (sizes come
// from a time-limited count; nothing is hashed or downloaded) and choose what
// to sync with checkboxes. Checking pulls the item; unchecking stops syncing
// it (it stays untouched on the remote).

import * as path from "path";
import * as vscode from "vscode";
import { DevsyncEvent, runJson } from "./cli";

export interface RemoteNode {
  path: string;
  dir: boolean;
  size: number;
  files: number;
  complete: boolean;
  excluded: boolean;
  selection: "selected" | "partial" | "unselected" | "excluded";
}

function bytes(n: number): string {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(1)} ${u[i]}`;
}

export class RemoteItem extends vscode.TreeItem {
  constructor(readonly node: RemoteNode, root: string) {
    super(
      path.basename(node.path),
      node.dir && !node.excluded ? vscode.TreeItemCollapsibleState.Collapsed : vscode.TreeItemCollapsibleState.None,
    );
    const approx = node.complete ? "" : "≥ ";
    if (node.excluded) {
      this.description = "always excluded";
    } else if (node.dir) {
      this.description = `${approx}${bytes(node.size)} · ${approx}${node.files} files${
        node.selection === "partial" ? " · partly synced" : ""
      }`;
    } else {
      this.description = bytes(node.size);
    }
    this.tooltip = node.excluded
      ? `${node.path} is never synced (dependencies, VCS data, caches)`
      : `${node.path}\n${this.description}\n${
          node.selection === "unselected" ? "Not synced — check to sync and download it" : "Synced — uncheck to stop syncing it"
        }${node.complete ? "" : "\n(sizes are lower bounds: counting stopped early)"}`;
    // Files get their usual icons; folders a folder icon.
    this.resourceUri = vscode.Uri.file(path.join(root, node.path));
    if (node.dir) this.iconPath = vscode.ThemeIcon.Folder;
    if (!node.excluded) {
      this.checkboxState =
        node.selection === "unselected"
          ? vscode.TreeItemCheckboxState.Unchecked
          : vscode.TreeItemCheckboxState.Checked;
    }
    this.contextValue = node.excluded ? "remote-excluded" : "remote-node";
  }
}

export class RemoteFiles implements vscode.TreeDataProvider<RemoteItem>, vscode.Disposable {
  private readonly emitter = new vscode.EventEmitter<RemoteItem | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;
  private readonly view: vscode.TreeView<RemoteItem>;
  private root?: string;

  constructor(
    private readonly log: vscode.OutputChannel,
    private readonly afterChange: () => void,
    private readonly withTransfer: <T>(title: string, op: (onEvent: (e: DevsyncEvent) => void) => Promise<T>) => Promise<T>,
  ) {
    this.view = vscode.window.createTreeView("devsync.remoteFiles", {
      treeDataProvider: this,
      manageCheckboxStateManually: true,
      canSelectMany: true,
    });
    this.view.onDidChangeCheckboxState((e) => void this.onChecked(e));
  }

  setRoot(root: string): void {
    this.root = root;
    this.refresh();
  }

  refresh(): void {
    this.emitter.fire(undefined);
  }

  getTreeItem(el: RemoteItem): vscode.TreeItem {
    return el;
  }

  async getChildren(el?: RemoteItem): Promise<RemoteItem[]> {
    const root = this.root;
    if (!root) return [];
    try {
      const r = await runJson<{ nodes: RemoteNode[] }>(root, ["tree", "--", el?.node.path ?? ""], 300_000);
      this.view.message = undefined;
      return r.nodes.map((n) => new RemoteItem(n, root));
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      this.log.appendLine(`remote files: ${msg}`);
      if (!el) this.view.message = `Can't list the remote: ${msg}`;
      return [];
    }
  }

  async reveal(): Promise<void> {
    await vscode.commands.executeCommand("devsync.remoteFiles.focus");
  }

  private async onChecked(e: vscode.TreeCheckboxChangeEvent<RemoteItem>): Promise<void> {
    const root = this.root;
    if (!root) return;
    const select = e.items.filter(([, s]) => s === vscode.TreeItemCheckboxState.Checked).map(([i]) => i.node.path);
    const unselect = e.items
      .filter(([, s]) => s === vscode.TreeItemCheckboxState.Unchecked)
      .map(([i]) => i.node.path);
    try {
      await this.withTransfer(select.length ? `syncing ${select.join(", ")}` : "updating what is synced", async (onEvent) => {
        if (unselect.length) await runJson(root, ["unselect", "--", ...unselect], 60_000);
        // Selecting also downloads it, so it shows up locally right away.
        if (select.length) await runJson(root, ["select", "--pull", "--", ...select], 24 * 3600_000, onEvent);
      });
      if (unselect.length) {
        void vscode.window.showInformationMessage(
          `No longer syncing ${unselect.join(", ")}. It stays on the remote; local copies are kept.`,
        );
      }
    } catch (err) {
      void vscode.window.showErrorMessage(`devsync: ${err instanceof Error ? err.message : String(err)}`);
    }
    this.refresh();
    this.afterChange();
  }

  dispose(): void {
    this.view.dispose();
    this.emitter.dispose();
  }
}
