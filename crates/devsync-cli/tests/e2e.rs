//! End-to-end tests driving the real `devsync` binary.
//!
//! Most tests use the `local` endpoint (agent spawned directly). The ssh test
//! uses a fake `ssh` that runs the remote command locally with a separate
//! HOME, which exercises agent deployment and the ssh command lines.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_devsync")
}

struct Env {
    _tmp: tempfile::TempDir,
    local: PathBuf,
    remote: PathBuf,
}

fn write(p: &Path, content: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn devsync(cwd: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env("NO_COLOR", "1")
        .env_remove("DEVSYNC_SSH")
        .output()
        .expect("run devsync")
}

fn ok(o: &Output) -> String {
    assert!(
        o.status.success(),
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn setup() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let local = root.join("local");
    let remote = root.join("remote");
    fs::create_dir_all(&local).unwrap();
    ok(&devsync(&local, &["init", &format!("local:{}", remote.display())]));
    Env { _tmp: tmp, local, remote }
}

fn set_config(env: &Env, from: &str, to: &str) {
    let f = env.local.join(".devsync.toml");
    let t = fs::read_to_string(&f).unwrap();
    assert!(t.contains(from), "config has no {from:?}");
    fs::write(f, t.replacen(from, to, 1)).unwrap();
}

#[test]
fn push_respects_rules_and_is_incremental() {
    let env = setup();
    write(&env.local.join(".env"), "SECRET=1");
    write(&env.local.join(".gitignore"), "build/\n.env\n");
    write(&env.local.join("build/out.bin"), "artifact");
    write(&env.local.join("node_modules/x/index.js"), "x");
    write(&env.local.join("src/a.py"), "a = 1");
    write(&env.local.join("src/b.py"), "b = 1");
    write(&env.local.join(".devsyncignore"), "*.tmp\n");
    write(&env.local.join("scratch.tmp"), "x");
    ok(&devsync(&env.local, &["push"]));

    let r = &env.remote;
    assert_eq!(fs::read_to_string(r.join(".env")).unwrap(), "SECRET=1", ".gitignore'd files are synced");
    assert!(r.join("build/out.bin").exists());
    assert!(!r.join("node_modules").exists(), "dependency dirs are never synced");
    assert!(!r.join("scratch.tmp").exists(), ".devsyncignore is honoured");
    assert!(!r.join(".devsync").exists(), "local state stays local");

    // Second push: only the modified file moves.
    write(&env.local.join("src/a.py"), "a = 2");
    let o = devsync(&env.local, &["push", "-v"]);
    let err = String::from_utf8_lossy(&o.stderr);
    ok(&o);
    assert!(err.contains("↑ src/a.py"), "{err}");
    assert!(!err.contains("src/b.py"), "{err}");
    assert_eq!(fs::read_to_string(r.join("src/a.py")).unwrap(), "a = 2");

    // Deletions are off by default…
    fs::remove_file(env.local.join("src/b.py")).unwrap();
    ok(&devsync(&env.local, &["push"]));
    assert!(r.join("src/b.py").exists());
    // …and propagate once enabled.
    set_config(&env, "delete = false", "delete = true");
    ok(&devsync(&env.local, &["push"]));
    assert!(!r.join("src/b.py").exists());

    // use_gitignore = true: gitignored files are no longer sent (and, being
    // excluded on both sides, existing remote copies are left alone).
    set_config(&env, "use_gitignore = false", "use_gitignore = true");
    write(&env.local.join(".env"), "SECRET=2");
    let plan = ok(&devsync(&env.local, &["push", "--dry-run"]));
    assert!(!plan.contains(".env") && !plan.contains("build/"), "{plan}");
    ok(&devsync(&env.local, &["push"]));
    assert_eq!(fs::read_to_string(r.join(".env")).unwrap(), "SECRET=1");
}

#[test]
fn exec_runs_in_matching_dir_with_exit_code_and_pull_back() {
    let env = setup();
    write(&env.local.join("src/gen.sh"), "pwd\necho generated > out.txt\necho to-stderr >&2\nexit 3\n");
    let o = devsync(&env.local.join("src"), &["exec", "--", "sh gen.sh"]);
    assert_eq!(o.status.code(), Some(3));
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(stdout.contains(&format!("{}/src", env.remote.display())), "{stdout}");
    assert!(stdout.contains("to-stderr"), "stderr is captured too: {stdout}");
    assert_eq!(fs::read_to_string(env.local.join("src/out.txt")).unwrap(), "generated\n");

    // Multi-arg form is quoted correctly.
    let o = devsync(&env.local, &["exec", "--", "printf", "%s|", "a b", "c'd"]);
    assert_eq!(String::from_utf8_lossy(&o.stdout), "a b|c'd|");

    // Results are kept in the local folder.
    let jobs = ok(&devsync(&env.local, &["--json", "jobs"]));
    let v: serde_json::Value = serde_json::from_str(jobs.trim()).unwrap();
    let last = &v.as_array().unwrap()[0];
    assert_eq!(last["state"], "exited");
    let out = fs::read_to_string(last["output"].as_str().unwrap()).unwrap();
    assert_eq!(out, "a b|c'd|");
}

#[test]
fn pull_back_never_overwrites_local_edits() {
    let env = setup();
    write(&env.local.join("notes.txt"), "original\n");
    ok(&devsync(&env.local, &["push"]));
    // The remote command rewrites notes.txt after a delay; meanwhile we edit it locally.
    let id = ok(&devsync(&env.local, &["submit", "--", "sleep 1; echo remote-edit > notes.txt; echo new > fresh.txt"]));
    let id = id.trim().to_string();
    write(&env.local.join("notes.txt"), "local-edit\n");
    let o = devsync(&env.local, &["logs", &id]);
    ok(&o);
    assert_eq!(fs::read_to_string(env.local.join("notes.txt")).unwrap(), "local-edit\n");
    assert_eq!(fs::read_to_string(env.local.join("notes.txt.devsync-remote")).unwrap(), "remote-edit\n");
    assert_eq!(fs::read_to_string(env.local.join("fresh.txt")).unwrap(), "new\n");
    assert!(String::from_utf8_lossy(&o.stderr).contains("saved as notes.txt.devsync-remote"));
}

#[test]
fn submit_collect_and_kill() {
    let env = setup();
    let id = ok(&devsync(&env.local, &["submit", "--", "echo started; sleep 30"])).trim().to_string();
    std::thread::sleep(Duration::from_millis(800));
    ok(&devsync(&env.local, &["collect"]));
    let log = env.local.join(".devsync/jobs").join(&id).join("output.log");
    assert_eq!(fs::read_to_string(&log).unwrap(), "started\n", "collect fetches output into the results folder");

    ok(&devsync(&env.local, &["kill", &id]));
    let status: serde_json::Value =
        serde_json::from_slice(&fs::read(env.local.join(".devsync/jobs").join(&id).join("status.json")).unwrap())
            .unwrap();
    assert_eq!(status["state"], "exited");
    assert_eq!(status["exit_code"], 143);
    let remote = ok(&devsync(&env.local, &["jobs", "--remote"]));
    assert!(remote.contains(&id), "{remote}");
}

#[test]
fn exec_job_survives_while_no_session_is_open() {
    // The command must keep running with no client connected at all.
    let env = setup();
    let id = ok(&devsync(&env.local, &["submit", "--", "sleep 1; echo done-without-client"])).trim().to_string();
    std::thread::sleep(Duration::from_millis(1800));
    let out = ok(&devsync(&env.local, &["logs", &id]));
    assert_eq!(out, "done-without-client\n");
}

#[test]
fn claude_hook_and_setup() {
    let env = setup();
    let run_hook = |cmd: &str| -> String {
        let mut child = Command::new(bin())
            .args(["hook", "claude"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = serde_json::json!({"tool_name": "Bash", "cwd": env.local, "tool_input": {"command": cmd}});
        use std::io::Write;
        child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success());
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    assert_eq!(run_hook("git status && ls"), "");
    let out = run_hook("npm test");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let cmd = v["hookSpecificOutput"]["updatedInput"]["command"].as_str().unwrap();
    assert!(cmd.ends_with("exec -- 'npm test'"), "{cmd}");

    ok(&devsync(&env.local, &["setup", "all"]));
    let settings = fs::read_to_string(env.local.join(".claude/settings.json")).unwrap();
    assert!(settings.contains("hook claude"));
    assert!(fs::read_to_string(env.local.join("CLAUDE.md")).unwrap().contains("devsync"));
    assert!(fs::read_to_string(env.local.join("AGENTS.md")).unwrap().contains("devsync"));
}

/// A stand-in for ssh: drops options, takes the host, runs the remote command
/// locally under a fake HOME. `$FAKE_SSH_DOWN` makes it fail like ssh does.
fn fake_ssh(dir: &Path, home: &Path) -> PathBuf {
    let p = dir.join("fake-ssh");
    let script = format!(
        r#"#!/bin/sh
[ -f "{down}" ] && {{ echo "ssh: connect to host box: Network is unreachable" >&2; exit 255; }}
while [ $# -gt 0 ]; do
  case "$1" in
    -o|-p|-i|-L|-O) shift 2 ;;
    -*) shift ;;
    *) break ;;
  esac
done
shift
export HOME="{home}"
cd "$HOME"
exec sh -c "$*"
"#,
        down = dir.join("DOWN").display(),
        home = home.display()
    );
    fs::write(&p, script).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn ssh_transport_deploys_agent_and_recovers_from_outage() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let home = base.join("remote-home");
    let local = base.join("local");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&local).unwrap();
    let ssh = fake_ssh(&base, &home);
    let run = |args: &[&str]| -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&local)
            .env("NO_COLOR", "1")
            .env("DEVSYNC_SSH", &ssh)
            .output()
            .unwrap()
    };
    ok(&run(&["init", "me@box:~/proj"]));
    // Multiplexing makes no sense with the fake ssh.
    let cfg = local.join(".devsync.toml");
    fs::write(&cfg, fs::read_to_string(&cfg).unwrap().replace("multiplex = true", "multiplex = false")).unwrap();
    write(&local.join("hello.sh"), "echo hello from $HOME");

    let o = run(&["exec", "--", "sh hello.sh"]);
    assert_eq!(ok(&o), format!("hello from {}\n", home.display()));
    // "~/proj" was expanded on the remote side, and the agent was deployed there.
    assert!(home.join("proj/hello.sh").exists());
    let bins: Vec<_> = fs::read_dir(home.join(".devsync/bin")).unwrap().flatten().collect();
    assert_eq!(bins.len(), 1);
    assert!(local.join(".devsync/remote.json").exists());

    // Network down: a bounded number of retries, then a clear failure.
    fs::write(&cfg, fs::read_to_string(&cfg).unwrap().replace("retries = 0", "retries = 2")).unwrap();
    fs::write(base.join("DOWN"), "").unwrap();
    let o = run(&["push"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("retry #1") && err.contains("Network is unreachable"), "{err}");

    // Link comes back during the retries: the command still succeeds.
    fs::write(&cfg, fs::read_to_string(&cfg).unwrap().replace("retries = 2", "retries = 0")).unwrap();
    let down = base.join("DOWN");
    let restore = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        fs::remove_file(down).unwrap();
    });
    write(&local.join("hello.sh"), "echo back online");
    let o = run(&["exec", "--", "sh hello.sh"]);
    restore.join().unwrap();
    assert_eq!(ok(&o), "back online\n");
    assert!(String::from_utf8_lossy(&o.stderr).contains("retry"));
}
