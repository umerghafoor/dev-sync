// Thin wrapper around the devsync CLI. All logic lives in the CLI; the
// extension only renders what `devsync --json …` reports.

import { ChildProcess, spawn } from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";

export interface RunResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

export class DevsyncNotFound extends Error {}

/** Resolves the devsync binary: setting, PATH, then ~/.cargo/bin. */
export function binaryPath(): string {
  const configured = vscode.workspace.getConfiguration("devsync").get<string>("binaryPath", "").trim();
  if (configured) {
    return configured.replace(/^~(?=\/|$)/, os.homedir());
  }
  const dirs = (process.env.PATH ?? "").split(path.delimiter);
  // Editors started from the Dock/launcher may not inherit the shell PATH.
  dirs.push(
    path.join(os.homedir(), ".local", "bin"),
    path.join(os.homedir(), ".cargo", "bin"),
    "/usr/local/bin",
    "/opt/homebrew/bin",
  );
  for (const d of dirs) {
    const candidate = path.join(d, "devsync");
    try {
      fs.accessSync(candidate, fs.constants.X_OK);
      return candidate;
    } catch {
      // keep looking
    }
  }
  return "devsync";
}

/** Runs a devsync command to completion. */
export function run(cwd: string, args: string[], timeoutMs = 120_000): Promise<RunResult> {
  return new Promise((resolve, reject) => {
    let child: ChildProcess;
    try {
      child = spawn(binaryPath(), args, { cwd, env: { ...process.env, NO_COLOR: "1" } });
    } catch (e) {
      reject(e);
      return;
    }
    let stdout = "";
    let stderr = "";
    const timer = setTimeout(() => child.kill(), timeoutMs);
    child.stdout?.on("data", (d) => (stdout += d.toString()));
    child.stderr?.on("data", (d) => (stderr += d.toString()));
    child.on("error", (err: NodeJS.ErrnoException) => {
      clearTimeout(timer);
      reject(err.code === "ENOENT" ? new DevsyncNotFound("devsync binary not found") : err);
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      resolve({ code, stdout, stderr });
    });
  });
}

/** Runs `devsync --json <args>` and parses the last JSON line of stdout. */
export async function runJson<T>(cwd: string, args: string[], timeoutMs?: number): Promise<T> {
  const r = await run(cwd, ["--json", ...args], timeoutMs);
  const lines = r.stdout.trim().split("\n").filter((l) => l.trim().length > 0);
  const last = lines[lines.length - 1];
  if (!last) {
    throw new Error(r.stderr.trim() || `devsync ${args.join(" ")} exited with ${r.code}`);
  }
  const parsed = JSON.parse(last);
  if (parsed && parsed.event === "error") {
    throw new Error(parsed.message);
  }
  return parsed as T;
}

/** Long-running devsync process streaming NDJSON events. */
export class EventStream implements vscode.Disposable {
  private child?: ChildProcess;
  private buffer = "";
  private stopped = false;

  constructor(
    private readonly cwd: string,
    private readonly args: string[],
    private readonly onEvent: (e: DevsyncEvent) => void,
    private readonly onExit: (code: number | null, stderr: string) => void,
  ) {}

  start(): void {
    this.stopped = false;
    let stderr = "";
    const child = spawn(binaryPath(), ["--json", ...this.args], {
      cwd: this.cwd,
      env: { ...process.env, NO_COLOR: "1" },
    });
    this.child = child;
    child.stdout.on("data", (d) => {
      this.buffer += d.toString();
      let nl: number;
      while ((nl = this.buffer.indexOf("\n")) >= 0) {
        const line = this.buffer.slice(0, nl).trim();
        this.buffer = this.buffer.slice(nl + 1);
        if (line) {
          try {
            this.onEvent(JSON.parse(line));
          } catch {
            // ignore non-JSON noise
          }
        }
      }
    });
    child.stderr.on("data", (d) => {
      stderr = (stderr + d.toString()).slice(-4000);
    });
    child.on("error", (err: NodeJS.ErrnoException) => {
      this.onExit(null, err.code === "ENOENT" ? "devsync binary not found" : String(err));
    });
    child.on("close", (code) => {
      if (!this.stopped) {
        this.onExit(code, stderr);
      }
    });
  }

  get running(): boolean {
    return !!this.child && this.child.exitCode === null && !this.stopped;
  }

  stop(): void {
    this.stopped = true;
    this.child?.kill("SIGINT");
    this.child = undefined;
  }

  dispose(): void {
    this.stop();
  }
}

export type DevsyncEvent =
  | { event: "connecting"; host: string }
  | { event: "retrying"; attempt: number; error: string; delay_ms: number }
  | { event: "planned"; upload: number; upload_bytes: number; delete: number }
  | { event: "uploaded"; path: string; bytes: number }
  | { event: "deleted"; path: string }
  | { event: "synced"; files: number; bytes: number; deleted: number; millis: number }
  | { event: "up_to_date" }
  | { event: "pulled"; path: string }
  | { event: "conflict"; path: string; saved_as: string }
  | { event: "job_started"; id: string; command: string; cwd: string }
  | { event: "job_updated"; id: string; state: string; new_bytes: number }
  | { event: "job_finished"; id: string; state: string; exit_code: number | null }
  | { event: "watching"; root: string }
  | { event: "changed"; paths: number }
  | { event: "offline"; error: string }
  | { event: "needs_selection"; message: string }
  | { event: "warning"; message: string }
  | { event: "info"; message: string }
  | { event: "error"; message: string };

export interface LocalJob {
  id: string;
  command: string;
  cwd: string;
  host: string;
  started_unix: number;
  state: "running" | "exited" | "lost";
  exit_code: number | null;
  offset: number;
  pulled_back: boolean;
  output: string;
}

export interface Status {
  root: string;
  host: string;
  path: string;
  pending: number;
  pending_files: string[];
  running_jobs: number;
  connected: boolean;
  latency_ms: number;
  remote: { os: string; arch: string; root: string; agent_version: string } | null;
  error: string | null;
}

/** Quotes a string for POSIX shells. */
export function shQuote(s: string): string {
  if (/^[A-Za-z0-9_\-./=:@,+%]+$/.test(s)) {
    return s;
  }
  return `'${s.replace(/'/g, `'\\''`)}'`;
}
