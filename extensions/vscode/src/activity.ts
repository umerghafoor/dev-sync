// What devsync is moving right now and what it moved recently, built from
// the CLI's event stream (auto sync and one-off pushes/pulls alike).

import * as vscode from "vscode";
import { DevsyncEvent } from "./cli";

export interface TransferView {
  direction: "up" | "down" | "none";
  /** "checking remote files", "uploading", … */
  stage: string;
  path: string;
  filesDone: number;
  filesTotal: number;
  bytesDone: number;
  bytesTotal: number;
  /** Bytes per second (0 = not measured yet). */
  rate: number;
  /** Seconds left, -1 if unknown. */
  eta: number;
  /** Pulls: remote files already identical here (not downloaded again). */
  already: number;
}

export interface ActivityEntry {
  kind: "up" | "down" | "deleted" | "conflict";
  path: string;
  bytes: number;
  at: number;
}

const KEEP = 100;

export class Activity implements vscode.Disposable {
  transfer?: TransferView;
  recent: ActivityEntry[] = [];
  private already = 0;
  private rateFrom?: { at: number; bytes: number; direction: string };
  private readonly emitter = new vscode.EventEmitter<void>();
  readonly onDidChange = this.emitter.event;
  private timer?: NodeJS.Timeout;
  private clearTimer?: NodeJS.Timeout;

  /** Feeds one CLI event; returns true if it was about transfers. */
  handle(e: DevsyncEvent): boolean {
    switch (e.event) {
      case "stage":
        if (e.stage === "scan_local" || e.stage === "scan_remote") {
          this.set({ ...this.blank(), stage: e.message });
        } else if (this.transfer) {
          this.set({ ...this.transfer, stage: e.message });
        }
        return true;
      case "connecting":
        if (this.transfer && this.transfer.direction === "none") this.set({ ...this.transfer, stage: `connecting to ${e.host}` });
        return true;
      case "pull_planned":
        this.already = e.already;
        return true;
      case "progress": {
        const now = Date.now();
        if (!this.rateFrom || this.rateFrom.direction !== e.direction || this.rateFrom.bytes > e.bytes_done) {
          this.rateFrom = { at: now, bytes: e.bytes_done, direction: e.direction };
        }
        const secs = (now - this.rateFrom.at) / 1000;
        const rate = secs > 0.5 ? (e.bytes_done - this.rateFrom.bytes) / secs : 0;
        this.set({
          direction: e.direction,
          stage: e.direction === "up" ? "pushing" : "pulling",
          path: e.path,
          filesDone: e.files_done,
          filesTotal: e.files_total,
          bytesDone: e.bytes_done,
          bytesTotal: e.bytes_total,
          rate,
          eta: rate > 0 ? Math.max(0, (e.bytes_total - e.bytes_done) / rate) : -1,
          already: e.direction === "down" ? this.already : 0,
        });
        return true;
      }
      case "uploaded":
        this.add({ kind: "up", path: e.path, bytes: e.bytes, at: Date.now() });
        return true;
      case "pulled":
        this.add({ kind: "down", path: e.path, bytes: e.bytes ?? 0, at: Date.now() });
        return true;
      case "deleted":
        this.add({ kind: "deleted", path: e.path, bytes: 0, at: Date.now() });
        return true;
      case "conflict":
        this.add({ kind: "conflict", path: e.path, bytes: 0, at: Date.now() });
        return false;
      case "synced":
      case "up_to_date":
      case "pull_done":
      case "offline":
      case "needs_selection":
      case "error":
        this.end();
        return e.event !== "offline" && e.event !== "needs_selection" && e.event !== "error";
      default:
        return false;
    }
  }

  /** The operation finished (or failed): hide the progress shortly after. */
  end(): void {
    this.rateFrom = undefined;
    this.already = 0;
    if (this.clearTimer) clearTimeout(this.clearTimer);
    if (!this.transfer) return;
    if (this.transfer.direction !== "none") {
      // Show the completed bar briefly so short transfers are visible at all.
      this.set({ ...this.transfer, filesDone: this.transfer.filesTotal, bytesDone: this.transfer.bytesTotal, stage: "done", eta: 0 });
      this.clearTimer = setTimeout(() => this.set(undefined), 1500);
    } else {
      this.set(undefined);
    }
  }

  clearRecent(): void {
    this.recent = [];
    this.fire();
  }

  private blank(): TransferView {
    return { direction: "none", stage: "", path: "", filesDone: 0, filesTotal: 0, bytesDone: 0, bytesTotal: 0, rate: 0, eta: -1, already: 0 };
  }

  private set(t: TransferView | undefined): void {
    if (t && this.clearTimer) {
      clearTimeout(this.clearTimer);
      this.clearTimer = undefined;
    }
    this.transfer = t;
    this.fire();
  }

  private add(a: ActivityEntry): void {
    this.recent = [a, ...this.recent.filter((r) => r.path !== a.path)].slice(0, KEEP);
    this.fire();
  }

  /** Coalesces bursts (thousands of small files) into ~8 updates/s. */
  private fire(): void {
    if (this.timer) return;
    this.timer = setTimeout(() => {
      this.timer = undefined;
      this.emitter.fire();
    }, 120);
  }

  dispose(): void {
    if (this.timer) clearTimeout(this.timer);
    if (this.clearTimer) clearTimeout(this.clearTimer);
    this.emitter.dispose();
  }
}

export function formatBytes(n: number): string {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(1)} ${u[i]}`;
}
