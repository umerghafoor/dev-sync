// Read-only documents with remote content, for Git-style diffs:
//   devsync-remote:/<rel>?<root>            the file as it is on the remote now
//   devsync-git:/<rel>?<root>#<ref>         git's version on the remote (ref
//                                           "HEAD", or "" = the index)

import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { run } from "./cli";

export const REMOTE_SCHEME = "devsync-remote";
export const GIT_SCHEME = "devsync-git";

export function remoteUri(root: string, rel: string): vscode.Uri {
  return vscode.Uri.from({ scheme: REMOTE_SCHEME, path: `/${rel}`, query: root });
}

/** `id`: the object id when known, so the content comes from the local cache. */
export function gitUri(root: string, rel: string, ref: "HEAD" | "", id?: string): vscode.Uri {
  return vscode.Uri.from({ scheme: GIT_SCHEME, path: `/${rel}`, query: root, fragment: id ? `${ref}@${id}` : ref });
}

/** Local cache of remote git objects (filled by `devsync git-objects`). */
export function objectPath(root: string, id: string): string {
  return path.join(root, ".devsync", "git-objects", id);
}

/** An empty document, for the missing side of an added or deleted file. */
export function emptyUri(rel: string): vscode.Uri {
  return vscode.Uri.from({ scheme: REMOTE_SCHEME, path: `/${rel}`, query: "", fragment: "empty" });
}

export class RemoteDocs implements vscode.TextDocumentContentProvider, vscode.Disposable {
  private readonly emitter = new vscode.EventEmitter<vscode.Uri>();
  readonly onDidChange = this.emitter.event;
  private readonly open = new Set<string>();
  private readonly disposables: vscode.Disposable[];

  constructor(private readonly log: vscode.OutputChannel) {
    this.disposables = [
      vscode.workspace.registerTextDocumentContentProvider(REMOTE_SCHEME, this),
      vscode.workspace.registerTextDocumentContentProvider(GIT_SCHEME, this),
      vscode.workspace.onDidCloseTextDocument((d) => this.open.delete(d.uri.toString())),
    ];
  }

  /** Re-reads open remote documents (after a sync or a git command). */
  refreshAll(): void {
    for (const u of this.open) this.emitter.fire(vscode.Uri.parse(u));
  }

  async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
    const root = uri.query;
    if (!root || uri.fragment === "empty") return "";
    this.open.add(uri.toString());
    const rel = uri.path.replace(/^\//, "");
    if (uri.scheme === GIT_SCHEME && uri.fragment.includes("@")) {
      // Known object id: read the cached copy, fetching it once if needed.
      const id = uri.fragment.split("@")[1];
      const file = objectPath(root, id);
      if (!fs.existsSync(file)) await run(root, ["git-objects", "--", id], 120_000);
      if (fs.existsSync(file)) return fs.promises.readFile(file, "utf8");
      // Too large for the cache: read that exact version directly.
      const r = await run(root, ["git", "--no-push", "--no-pull", "--", "cat-file", "blob", id], 300_000);
      if (r.code === 0) return r.stdout;
    }
    const args =
      uri.scheme === GIT_SCHEME
        ? ["git", "--no-push", "--no-pull", "--", "show", `${uri.fragment.split("@")[0]}:./${rel}`]
        : ["cat", "--", rel];
    const r = await run(root, args, 120_000);
    if (r.code === 0) return r.stdout;
    // Not on the remote / not in git at that revision: show it as empty.
    const err = r.stderr.trim();
    if (r.code === 1 || /does not exist|exists on disk, but not in|not on the remote/.test(err)) return "";
    this.log.appendLine(`${path.basename(rel)}: ${err}`);
    throw new Error(`devsync: can't read the remote version of ${rel}: ${err.split("\n").pop()}`);
  }

  dispose(): void {
    for (const d of this.disposables) d.dispose();
    this.emitter.dispose();
  }
}
