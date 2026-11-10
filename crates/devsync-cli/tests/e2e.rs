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
        let mut child =
            Command::new(bin()).args(["hook", "claude"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
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
[ -f "{down}" ] && {{ grep -q timeout "{down}" && echo "ssh: connect to host box port 22: Operation timed out" >&2 || echo "ssh: connect to host box: Network is unreachable" >&2; exit 255; }}
# A broken shared (multiplexed) connection: sessions through it die silently.
case " $* " in *ControlMaster=auto*) [ -n "$FAKE_SSH_BROKEN_MUX" ] && exit 255 ;; esac
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
# Like a chatty ~/.bashrc: print a banner and stray bytes before every command.
[ -n "$FAKE_SSH_NOISE" ] && printf 'Welcome to the box!\n\006:u\316 conda ready\n'
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
fn shell_reconnects_and_slow_connects_get_more_time() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let home = base.join("remote-home");
    let local = base.join("local");
    fs::create_dir_all(home.join("proj")).unwrap();
    fs::create_dir_all(&local).unwrap();
    let ssh = fake_ssh(&base, &home);
    // A unique host name keeps the learned timeout of this test separate.
    let host = format!("me@box{}", std::process::id());
    let run = |args: &[&str]| -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&local)
            .env("NO_COLOR", "1")
            .env("DEVSYNC_SSH", &ssh)
            .env("SHELL", "/bin/sh")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    };
    ok(&run(&["init", &format!("{host}:~/proj")]));
    let cfg = local.join(".devsync.toml");
    fs::write(&cfg, fs::read_to_string(&cfg).unwrap().replace("multiplex = true", "multiplex = false")).unwrap();

    // The link is down when the terminal opens and comes back 1.5 s later:
    // the shell reconnects instead of exiting with 255.
    let down = base.join("DOWN");
    fs::write(&down, "").unwrap();
    let restore = {
        let down = down.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            fs::remove_file(down).unwrap();
        })
    };
    let o = run(&["shell"]);
    restore.join().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert!(err.contains("Reconnecting in"), "{err}");

    // A connect timeout doubles the time allowed for that host next time.
    fs::write(&down, "timeout").unwrap();
    let o = run(&["--quiet", "status"]);
    let _ = o;
    let learned = std::path::PathBuf::from(format!("/tmp/devsync-{}", unsafe { libc_getuid() }))
        .join(format!("connect-timeout-{}", host.replace(['@', '.'], "_")));
    let secs: u32 = fs::read_to_string(&learned).unwrap().trim().parse().unwrap();
    assert!(secs >= 60, "learned timeout {secs}");
    let _ = fs::remove_file(learned);
}

extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
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
            .env("FAKE_SSH_NOISE", "1")
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

#[test]
fn changes_lists_unsynced_files_like_git_status() {
    let env = setup();
    write(&env.local.join("a.txt"), "1");
    write(&env.local.join("b.txt"), "1");
    let out = ok(&devsync(&env.local, &["changes"]));
    assert!(out.contains("A a.txt") && out.contains("A b.txt"), "{out}");

    ok(&devsync(&env.local, &["push"]));
    assert!(ok(&devsync(&env.local, &["changes"])).contains("nothing to push"));

    write(&env.local.join("a.txt"), "2");
    write(&env.local.join("c.txt"), "new");
    fs::remove_file(env.local.join("b.txt")).unwrap();
    let v: serde_json::Value = serde_json::from_str(ok(&devsync(&env.local, &["--json", "changes"])).trim()).unwrap();
    let got: Vec<(String, String)> = v["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["path"].as_str().unwrap().to_string(), c["status"].as_str().unwrap().to_string()))
        .collect();
    // Deletions only count when they would be propagated.
    assert_eq!(got, vec![("a.txt".into(), "modified".into()), ("c.txt".into(), "added".into())]);
    set_config(&env, "delete = false", "delete = true");
    let out = ok(&devsync(&env.local, &["changes"]));
    assert!(out.contains("D b.txt"), "{out}");
}

#[test]
fn changes_lists_only_files_that_differ_from_the_remote_before_first_sync() {
    let env = setup();
    // Both sides already have the same code, but no sync has completed.
    write(&env.remote.join("same.txt"), "x");
    write(&env.local.join("same.txt"), "x");
    write(&env.remote.join("differs.txt"), "remote");
    write(&env.local.join("differs.txt"), "local");
    write(&env.local.join("new.txt"), "new");
    // Before anything is known about the remote, everything is new.
    assert!(ok(&devsync(&env.local, &["changes"])).contains("A same.txt"));
    // Any remote scan (here a dry-run pull) teaches devsync what is there.
    ok(&devsync(&env.local, &["pull", "--dry-run"]));
    let out = ok(&devsync(&env.local, &["changes"]));
    assert!(!out.contains("same.txt"), "identical files are not listed: {out}");
    assert!(out.contains("M differs.txt") && out.contains("A new.txt"), "{out}");
    // Progress events are streamed for transfers.
    let o = devsync(&env.local, &["--json", "push"]);
    let out = ok(&o);
    assert!(out.contains(r#""event":"progress""#) && out.contains(r#""direction":"up""#), "{out}");
}

#[test]
fn pulls_resume_skip_what_is_here_and_never_run_twice_at_once() {
    let env = setup();
    let big: String = (0..300_000).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    write(&env.remote.join("big.txt"), &big);
    for i in 0..20 {
        write(&env.remote.join(format!("src/f{i}.py")), &format!("x = {i}"));
    }
    // An interrupted earlier download left the first half behind: resume it.
    write(&env.local.join(".big.txt.devsync-dl"), &big[..150_000]);
    // A stale partial file (older version) is detected and refetched.
    write(&env.local.join("src/.f1.py.devsync-dl"), "old content that is longer");
    // Some files are already here.
    write(&env.local.join("src/f0.py"), "x = 0");

    // Two syncs at once (auto sync + manual pull): one waits for the other.
    let a = {
        let local = env.local.clone();
        std::thread::spawn(move || devsync(&local, &["pull"]))
    };
    let b = devsync(&env.local, &["pull"]);
    let a = a.join().unwrap();
    ok(&a);
    ok(&b);
    assert_eq!(fs::read_to_string(env.local.join("big.txt")).unwrap(), big);
    assert_eq!(fs::read_to_string(env.local.join("src/f1.py")).unwrap(), "x = 1");
    assert!(!env.local.join(".big.txt.devsync-dl").exists());
    let both = format!("{}{}", String::from_utf8_lossy(&a.stderr), String::from_utf8_lossy(&b.stderr));
    assert!(both.contains("already here"), "{both}");
    assert!(both.contains("already up to date"), "the second pull finds everything here: {both}");
}

#[test]
fn remote_git_and_cat_work_like_local_git() {
    let env = setup();
    write(&env.local.join("a.txt"), "one\n");
    write(&env.local.join("b.txt"), "bee\n");
    ok(&devsync(&env.local, &["push"]));
    let git = |args: &[&str]| {
        let mut all = vec!["git", "--"];
        all.extend_from_slice(args);
        devsync(&env.local, &all)
    };
    ok(&git(&["init", "-q"]));
    ok(&git(&["add", "-A"]));
    ok(&git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "first"]));
    assert!(env.remote.join(".git").is_dir());
    assert!(!env.local.join(".git").exists(), "the remote's .git stays there");

    // A local edit is pushed first, so remote git sees it.
    write(&env.local.join("a.txt"), "two\n");
    assert_eq!(ok(&git(&["status", "--porcelain"])), " M a.txt\n");
    // `cat` shows the remote copy (for diffs); missing files exit 1.
    assert_eq!(ok(&devsync(&env.local, &["cat", "a.txt"])), "two\n");
    assert_eq!(devsync(&env.local, &["cat", "nope.txt"]).status.code(), Some(1));
    // `show` of HEAD gives the committed version, like local git.
    assert_eq!(ok(&git(&["show", "HEAD:a.txt"])), "one\n");
    // Git objects are fetched once into a local cache, for instant diffs.
    let id = ok(&git(&["rev-parse", "HEAD:a.txt"])).trim().to_string();
    ok(&devsync(&env.local, &["git-objects", &id, "0000000000000000000000000000000000000000", "nothex"]));
    assert_eq!(fs::read_to_string(env.local.join(".devsync/git-objects").join(&id)).unwrap(), "one\n");

    // Commands that change files bring the result back here.
    ok(&git(&["checkout", "--", "a.txt"]));
    assert_eq!(fs::read_to_string(env.local.join("a.txt")).unwrap(), "one\n");
    ok(&git(&["rm", "-q", "b.txt"]));
    assert!(!env.local.join("b.txt").exists(), "deleted on the remote, so removed here");

    // JSON output for the VS Code extension.
    let v: serde_json::Value =
        serde_json::from_str(ok(&devsync(&env.local, &["--json", "git", "--no-push", "status", "--porcelain"])).trim())
            .unwrap();
    assert_eq!(v["code"], 0);
    assert_eq!(v["stdout"], "D  b.txt\n");
}

#[test]
fn first_sync_pulls_existing_remote_code_into_new_folder() {
    let env = setup();
    // The remote already holds the project.
    write(&env.remote.join("src/app.py"), "print('remote')");
    write(&env.remote.join("README.md"), "remote readme");
    write(&env.remote.join("shared.txt"), "remote version");
    write(&env.remote.join("node_modules/dep/index.js"), "dep");
    // The new local folder only has one file of its own.
    write(&env.local.join("shared.txt"), "local version");

    let o = devsync(&env.local, &["push"]);
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    ok(&o);
    assert!(err.contains("first sync"), "{err}");

    // Remote-only files came down first…
    assert_eq!(fs::read_to_string(env.local.join("src/app.py")).unwrap(), "print('remote')");
    assert_eq!(fs::read_to_string(env.local.join("README.md")).unwrap(), "remote readme");
    assert!(!env.local.join("node_modules").exists(), "excluded paths are not pulled");
    // …a file present on both sides keeps the local version, the remote one is preserved…
    assert_eq!(fs::read_to_string(env.local.join("shared.txt")).unwrap(), "local version");
    assert_eq!(fs::read_to_string(env.local.join("shared.txt.devsync-remote")).unwrap(), "remote version");
    // …and then local changes were sent as usual.
    assert_eq!(fs::read_to_string(env.remote.join("shared.txt")).unwrap(), "local version");
    assert!(!env.remote.join("shared.txt.devsync-remote").exists());
    assert!(ok(&devsync(&env.local, &["changes"])).contains("nothing to push"));

    // Only the first sync pulls; afterwards sync is one-way again.
    write(&env.remote.join("later.txt"), "made on the remote");
    ok(&devsync(&env.local, &["push"]));
    assert!(!env.local.join("later.txt").exists());
}

#[test]
fn initial_pull_protects_remote_from_deletion_and_can_be_disabled() {
    // With delete on, an empty folder must not wipe an existing remote project.
    let env = setup();
    set_config(&env, "delete = false", "delete = true");
    write(&env.remote.join("keep.txt"), "important");
    ok(&devsync(&env.local, &["push"]));
    assert!(env.remote.join("keep.txt").exists());
    assert!(env.local.join("keep.txt").exists());

    // Opting out: nothing is downloaded.
    let env = setup();
    set_config(&env, "initial_pull = true", "initial_pull = false");
    write(&env.remote.join("remote-only.txt"), "x");
    ok(&devsync(&env.local, &["push"]));
    assert!(!env.local.join("remote-only.txt").exists());
}

#[test]
fn pull_brings_remote_changes_without_losing_local_edits() {
    let env = setup();
    write(&env.local.join("a.txt"), "a1");
    write(&env.local.join("b.txt"), "b1");
    ok(&devsync(&env.local, &["push"]));

    // Someone changes things on the remote…
    write(&env.remote.join("a.txt"), "a2 from remote");
    write(&env.remote.join("b.txt"), "b2 from remote");
    write(&env.remote.join("new/file.txt"), "new on remote");
    write(&env.remote.join("node_modules/x/i.js"), "dep");
    // …while b.txt is also edited locally.
    write(&env.local.join("b.txt"), "b-local-edit");

    let preview = devsync(&env.local, &["pull", "--dry-run"]);
    ok(&preview);
    assert!(String::from_utf8_lossy(&preview.stderr).contains("would update 2 file(s)"));
    assert!(!env.local.join("new/file.txt").exists(), "dry run changes nothing");

    let o = devsync(&env.local, &["pull"]);
    ok(&o);
    assert_eq!(fs::read_to_string(env.local.join("a.txt")).unwrap(), "a2 from remote", "untouched file updated");
    assert_eq!(fs::read_to_string(env.local.join("new/file.txt")).unwrap(), "new on remote");
    assert_eq!(fs::read_to_string(env.local.join("b.txt")).unwrap(), "b-local-edit", "local edit kept");
    assert_eq!(fs::read_to_string(env.local.join("b.txt.devsync-remote")).unwrap(), "b2 from remote");
    assert!(!env.local.join("node_modules").exists(), "excluded paths are not pulled");

    // Pulled files count as synced; only the local edit is left to push.
    let changes = ok(&devsync(&env.local, &["changes"]));
    assert_eq!(changes.trim(), "M b.txt", "{changes}");
}

#[test]
fn pull_specific_files_and_folders_and_list_remote() {
    let env = setup();
    write(&env.local.join("src/api/a.py"), "a1");
    write(&env.local.join("src/web/w.js"), "w1");
    write(&env.local.join("notes.txt"), "n1");
    ok(&devsync(&env.local, &["push"]));

    write(&env.remote.join("src/api/a.py"), "a2 remote");
    write(&env.remote.join("src/api/new.py"), "new remote");
    write(&env.remote.join("src/web/w.js"), "w2 remote");
    write(&env.remote.join("notes.txt"), "n2 remote");
    write(&env.local.join("notes.txt"), "n-local-edit");
    write(&env.remote.join("node_modules/dep/i.js"), "dep");
    write(&env.remote.join("data/big.csv"), "1,2,3");
    set_config(&env, "ignore = []", "ignore = [\"data/\"]");

    // ls shows what differs, with markers.
    let listing = ok(&devsync(&env.local, &["ls", "--changed"]));
    assert!(listing.contains("~") && listing.contains("src/api/a.py"), "{listing}");
    assert!(listing.contains("+") && listing.contains("src/api/new.py"), "{listing}");
    assert!(listing.contains("! ") && listing.contains("notes.txt"), "{listing}");
    assert!(!listing.contains("data/big.csv"), "sync.ignore'd paths hidden unless --all: {listing}");
    assert!(ok(&devsync(&env.local, &["ls", "--all", "data"])).contains("data/big.csv"));
    assert!(!ok(&devsync(&env.local, &["ls", "--all"])).contains("node_modules"), "built-in excludes never listed");
    let v: serde_json::Value =
        serde_json::from_str(ok(&devsync(&env.local, &["--json", "ls", "src/web"])).trim()).unwrap();
    assert_eq!(v["files"][0]["path"], "src/web/w.js");
    assert_eq!(v["files"][0]["status"], "changed");

    // Pull one folder: only that folder changes.
    ok(&devsync(&env.local, &["pull", "src/api"]));
    assert_eq!(fs::read_to_string(env.local.join("src/api/a.py")).unwrap(), "a2 remote");
    assert_eq!(fs::read_to_string(env.local.join("src/api/new.py")).unwrap(), "new remote");
    assert_eq!(fs::read_to_string(env.local.join("src/web/w.js")).unwrap(), "w1", "other folders untouched");

    // Pull a single file you edited: kept, remote saved alongside…
    ok(&devsync(&env.local, &["pull", "./notes.txt"]));
    assert_eq!(fs::read_to_string(env.local.join("notes.txt")).unwrap(), "n-local-edit");
    assert_eq!(fs::read_to_string(env.local.join("notes.txt.devsync-remote")).unwrap(), "n2 remote");
    // …unless --force.
    ok(&devsync(&env.local, &["pull", "--force", "notes.txt"]));
    assert_eq!(fs::read_to_string(env.local.join("notes.txt")).unwrap(), "n2 remote");

    // Several paths at once, including one excluded by sync.ignore, named explicitly.
    ok(&devsync(&env.local, &["pull", "src/web/w.js", "data"]));
    assert_eq!(fs::read_to_string(env.local.join("src/web/w.js")).unwrap(), "w2 remote");
    assert_eq!(fs::read_to_string(env.local.join("data/big.csv")).unwrap(), "1,2,3");
    // Built-in excludes are never pulled.
    assert!(!devsync(&env.local, &["pull", "node_modules/dep"]).status.success());

    // --to copies elsewhere without touching the project.
    let elsewhere = env.local.parent().unwrap().join("elsewhere");
    ok(&devsync(&env.local, &["pull", "src/api", "--to", elsewhere.to_str().unwrap()]));
    assert_eq!(fs::read_to_string(elsewhere.join("src/api/new.py")).unwrap(), "new remote");

    // Unknown path is a clear error.
    let o = devsync(&env.local, &["pull", "does/not/exist"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("nothing on the remote at does/not/exist"));
}

#[test]
fn broken_shared_ssh_connection_falls_back_to_direct() {
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
            .env("FAKE_SSH_BROKEN_MUX", "1")
            .output()
            .unwrap()
    };
    // Multiplexing stays on (the default); every multiplexed session fails.
    ok(&run(&["init", "me@box:~/proj"]));
    let cfg = local.join(".devsync.toml");
    fs::write(&cfg, fs::read_to_string(&cfg).unwrap().replace("retries = 0", "retries = 2")).unwrap();
    write(&local.join("hi.sh"), "echo direct works");
    let o = run(&["exec", "--", "sh hi.sh"]);
    assert_eq!(ok(&o), "direct works\n");
    assert!(!String::from_utf8_lossy(&o.stderr).contains("retry #"), "no retry loop needed");
}

#[test]
fn tree_and_selection_control_what_is_synced() {
    let env = setup();
    write(&env.remote.join("src/app.py"), "print(1)");
    write(&env.remote.join("README.md"), "readme");
    write(&env.remote.join("data/a.csv"), "1,2");
    write(&env.remote.join("data/sub/b.csv"), "3,4");
    write(&env.remote.join("models/m.bin"), "0123456789");
    write(&env.remote.join(".venv/lib/x.py"), "venv");

    // Browse one level with sizes; nothing is selected away yet.
    let tree = |path: &str| -> serde_json::Value {
        serde_json::from_str(ok(&devsync(&env.local, &["--json", "tree", path])).trim()).unwrap()
    };
    let root = tree("");
    let find = |v: &serde_json::Value, p: &str| v["nodes"].as_array().unwrap().iter().find(|n| n["path"] == p).cloned();
    let data = find(&root, "data").unwrap();
    assert_eq!((data["dir"].as_bool(), data["files"].as_u64(), data["size"].as_u64()), (Some(true), Some(2), Some(6)));
    assert_eq!(data["selection"], "selected");
    assert_eq!(find(&root, ".venv").unwrap()["selection"], "excluded");
    assert_eq!(find(&tree("data"), "data/sub").unwrap()["files"], 1);
    let human = ok(&devsync(&env.local, &["tree"]));
    assert!(human.contains("[x] data/") && human.contains(".venv/") && human.contains("always excluded"), "{human}");

    // Unselect the big folders, then do the first sync.
    ok(&devsync(&env.local, &["unselect", "data", "models"]));
    assert_eq!(find(&tree(""), "data").unwrap()["selection"], "unselected");
    ok(&devsync(&env.local, &["push"]));
    assert!(env.local.join("src/app.py").exists() && env.local.join("README.md").exists());
    assert!(!env.local.join("data").exists() && !env.local.join("models").exists(), "unselected folders not pulled");

    // Local files in an unselected folder are not uploaded either.
    write(&env.local.join("data/local-only.csv"), "x");
    ok(&devsync(&env.local, &["push"]));
    assert!(!env.remote.join("data/local-only.csv").exists());
    assert!(ok(&devsync(&env.local, &["changes"])).contains("nothing to push"));

    // Selecting a folder later can pull it right away.
    ok(&devsync(&env.local, &["select", "data", "--pull"]));
    assert_eq!(fs::read_to_string(env.local.join("data/sub/b.csv")).unwrap(), "3,4");
    let listing = ok(&devsync(&env.local, &["select"]));
    assert!(listing.contains("/models") && !listing.contains("/data"), "{listing}");
    ok(&devsync(&env.local, &["select", "--all"]));
    assert!(ok(&devsync(&env.local, &["select"])).contains("whole project"));
}

#[test]
fn truncated_agent_on_remote_is_replaced() {
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
    let cfg = local.join(".devsync.toml");
    fs::write(&cfg, fs::read_to_string(&cfg).unwrap().replace("multiplex = true", "multiplex = false")).unwrap();
    ok(&run(&["exec", "--", "true"]));
    let agent = fs::read_dir(home.join(".devsync/bin")).unwrap().flatten().next().unwrap().path();
    let full = fs::metadata(&agent).unwrap().len();

    // Simulate an upload that was cut off and still ended up in place.
    let bytes = fs::read(&agent).unwrap();
    fs::write(&agent, &bytes[..bytes.len() / 5]).unwrap();
    fs::remove_file(local.join(".devsync/remote.json")).unwrap();

    assert_eq!(ok(&run(&["exec", "--", "echo healed"])), "healed\n");
    assert_eq!(fs::metadata(&agent).unwrap().len(), full, "complete agent re-uploaded");
    // No temp files left behind.
    let leftovers: Vec<_> = fs::read_dir(home.join(".devsync/bin"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn large_remote_asks_what_to_sync_before_first_sync() {
    let env = setup();
    set_config(&env, "large_remote_limit_mb = 1024", "large_remote_limit_mb = 1");
    write(&env.remote.join("src/app.py"), "print(1)");
    write(&env.remote.join("data/huge.bin"), &"x".repeat(2 * 1024 * 1024));

    // Nothing chosen yet: neither push nor a whole pull scans or downloads.
    for args in [&["push"][..], &["pull"][..]] {
        let o = devsync(&env.local, args);
        assert!(!o.status.success(), "{args:?} must stop");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains("remote project is large") && err.contains("data/"), "{err}");
        assert!(!env.local.join("src").exists() && !env.local.join("data").exists());
    }
    // Pulling named paths is always allowed.
    ok(&devsync(&env.local, &["pull", "src"]));

    // Once a selection exists, the first sync proceeds without the big folder.
    ok(&devsync(&env.local, &["unselect", "data"]));
    ok(&devsync(&env.local, &["push"]));
    assert!(env.local.join("src/app.py").exists());
    assert!(!env.local.join("data").exists());
}
