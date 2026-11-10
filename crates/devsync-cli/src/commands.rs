//! Command implementations.

use crate::ui::{bytes, Human, Json, Verbosity};
use crate::{AgentArgs, AgentSub, Cli, Cmd, Global, HookAgent, SetupTarget};
use anyhow::{anyhow, bail, Context, Result};
use devsync_client::config::{Config, CONFIG_FILE};
use devsync_client::{integrations, jobs, sync, ClientError, Event, Project, Reporter};
use devsync_transport::endpoint::sh_quote;
use serde_json::json;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Exit code for devsync's own failures in `exec` (like ssh's 255), so they
/// are distinguishable from the remote command's exit code.
const EXIT_DEVSYNC: i32 = 255;

pub fn verbosity(g: &Global) -> Verbosity {
    if g.quiet {
        Verbosity::Quiet
    } else if g.verbose {
        Verbosity::Verbose
    } else {
        Verbosity::Normal
    }
}

pub fn reporter(g: &Global, terse: bool, json_to_stderr: bool) -> Box<dyn Reporter> {
    if g.json {
        Box::new(Json { to_stderr: json_to_stderr })
    } else {
        Box::new(Human::new(verbosity(g), terse))
    }
}

pub fn project() -> Result<Project> {
    Ok(Project::discover(&std::env::current_dir()?)?)
}

fn human(g: &Global) -> Human {
    Human::new(verbosity(g), false)
}

fn print_json(v: serde_json::Value) {
    println!("{v}");
}

pub fn run(cli: Cli) -> i32 {
    let g = cli.global.clone();
    if let Some(d) = &g.dir {
        if let Err(e) = std::env::set_current_dir(d) {
            human(&g).error(&format!("{}: {e}", d.display()));
            return 2;
        }
    }
    let is_exec = matches!(cli.cmd, Cmd::Exec { .. });
    let res = match cli.cmd {
        Cmd::Agent(a) => agent(a),
        Cmd::Hook { agent } => hook(agent),
        Cmd::Init { target, force } => init(&g, &target, force),
        Cmd::Push { dry_run, force } => push(&g, dry_run, force),
        Cmd::Watch => crate::watch::run(&g),
        Cmd::Exec { no_push, no_pull_back, command } => exec(&g, no_push, no_pull_back, &command),
        Cmd::Submit { no_push, command } => submit(&g, no_push, &command),
        Cmd::Logs { id, no_follow } => logs(&g, id.as_deref(), no_follow),
        Cmd::Jobs { remote } => list_jobs(&g, remote),
        Cmd::Kill { id } => kill(&g, id.as_deref()),
        Cmd::Collect => collect(&g),
        Cmd::Pull { paths, force, to, dry_run } => pull(&g, &paths, force, to.as_deref(), dry_run),
        Cmd::Ls { paths, changed, all } => ls(&g, &paths, changed, all),
        Cmd::Status => status(&g),
        Cmd::Changes => changes(&g),
        Cmd::Cat { path } => cat(&g, &path),
        Cmd::GitObjects { max_size, ids } => git_objects(&g, &ids, max_size),
        Cmd::Git { no_push, no_pull, args } => git(&g, &args, no_push, no_pull),
        Cmd::Tree { path } => tree(&g, path.as_deref().unwrap_or("")),
        Cmd::Select { paths, pull, all } => select(&g, &paths, pull, all),
        Cmd::Unselect { paths } => unselect(&g, &paths),
        Cmd::Doctor => doctor(&g),
        Cmd::Forward { ports } => forward(&g, ports),
        Cmd::Shell => shell(),
        Cmd::Setup { what } => setup(&g, what),
        Cmd::Completions { .. } => Ok(0),
    };
    match res {
        Ok(code) => code,
        Err(e) => {
            if matches!(e.downcast_ref::<ClientError>(), Some(ClientError::Interrupted)) {
                return 130;
            }
            if g.json {
                print_json(json!({"event": "error", "message": format!("{e:#}")}));
            } else {
                human(&g).error(&format!("{e:#}"));
            }
            if is_exec {
                EXIT_DEVSYNC
            } else {
                1
            }
        }
    }
}

// ---------------------------------------------------------------- agent side

fn agent(a: AgentArgs) -> Result<i32> {
    if let Some(AgentSub::RunJob { dir }) = a.sub {
        return Ok(devsync_agent::run_supervisor(&dir));
    }
    if !a.stdio {
        bail!("the agent is started by devsync itself (agent --stdio)");
    }
    let agent = devsync_agent::Agent::open(&a.root).with_context(|| format!("cannot open root {}", a.root))?;
    agent.serve(std::io::stdin().lock(), std::io::stdout().lock())?;
    Ok(0)
}

fn hook(agent: HookAgent) -> Result<i32> {
    // A hook must never break the agent: any problem means "leave it alone".
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return Ok(0);
    }
    let exe = std::env::current_exe().and_then(|p| p.canonicalize()).unwrap_or_else(|_| "devsync".into());
    match agent {
        HookAgent::Claude => {
            if let Some(out) = integrations::claude_hook(&input, &exe) {
                println!("{out}");
            }
        }
    }
    Ok(0)
}

// ------------------------------------------------------------------- setup

fn init(g: &Global, target: &str, force: bool) -> Result<i32> {
    let (host, path) = target
        .split_once(':')
        .filter(|(h, p)| !h.is_empty() && !p.is_empty())
        .ok_or_else(|| anyhow!("expected user@host:/remote/path (or local:/path), got {target:?}"))?;
    let cwd = std::env::current_dir()?;
    let file = cwd.join(CONFIG_FILE);
    if file.exists() && !force {
        bail!("{} already exists (use --force to overwrite)", file.display());
    }
    let text = Config::template(host, path);
    let cfg: Config = toml_check(&text)?;
    cfg.validate().map_err(|e| anyhow!(e))?;
    std::fs::write(&file, text)?;
    integrations::gitignore_state_dir(&cwd);
    if g.json {
        print_json(json!({"event": "initialized", "config": file, "host": host, "path": path}));
    } else {
        let h = human(g);
        h.ok(&format!("created {}", file.display()));
        h.line("next: `devsync push`, then `devsync exec -- <cmd>`; for coding agents: `devsync setup claude`");
    }
    Ok(0)
}

fn toml_check(text: &str) -> Result<Config> {
    // Round-trip through the real loader to guarantee the template is valid.
    let dir = std::env::temp_dir().join(format!("devsync-init-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let f = dir.join(CONFIG_FILE);
    std::fs::write(&f, text)?;
    let r = Config::load(&f).map_err(|e| anyhow!(e));
    let _ = std::fs::remove_dir_all(&dir);
    r
}

fn setup(g: &Global, what: SetupTarget) -> Result<i32> {
    let p = project()?;
    let exe = std::env::current_exe()?.canonicalize()?;
    let h = human(g);
    if matches!(what, SetupTarget::Claude | SetupTarget::All) {
        integrations::setup_claude(&p.root, &exe).map_err(|e| anyhow!(e))?;
        h.ok("Claude Code: hook added to .claude/settings.json, instructions added to CLAUDE.md");
        h.line("restart Claude Code (or open /hooks once) so it loads the hook");
    }
    if matches!(what, SetupTarget::AgentsMd | SetupTarget::All) {
        integrations::setup_agents_md(&p.root).map_err(|e| anyhow!(e))?;
        h.ok("instructions added to AGENTS.md");
    }
    if g.json {
        print_json(json!({"event": "setup_done", "root": p.root}));
    }
    Ok(0)
}

// -------------------------------------------------------------------- sync

fn push(g: &Global, dry_run: bool, force: bool) -> Result<i32> {
    let p = project()?;
    let rep = reporter(g, false, false);
    let stats = sync::push(&p, rep.as_ref(), sync::PushOptions { dry_run, force })?;
    if dry_run {
        if g.json {
            let up: Vec<_> = stats.plan.upload.iter().map(|e| &e.path).collect();
            print_json(json!({"event": "plan", "upload": up, "delete": stats.plan.delete}));
        } else {
            for e in &stats.plan.upload {
                println!("+ {} ({})", e.path, bytes(e.size));
            }
            for d in &stats.plan.delete {
                println!("- {d}");
            }
            if stats.plan.is_empty() {
                println!("nothing to push");
            }
        }
    }
    Ok(0)
}

fn cat(g: &Global, path: &str) -> Result<i32> {
    let p = project()?;
    let rep = reporter(g, true, true);
    // Paths are project-relative, wherever devsync was started.
    let p = Project { rel_cwd: String::new(), ..p };
    match devsync_client::remote::cat(&p, rep.as_ref(), path)? {
        Some(bytes) => {
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            out.write_all(&bytes)?;
            out.flush()?;
            Ok(0)
        }
        None => {
            if !g.quiet {
                human(g).warn(&format!("{path} is not on the remote"));
            }
            Ok(1)
        }
    }
}

/// Fetches git blobs into `.devsync/git-objects/<id>` (content-addressed,
/// so each is fetched at most once). One remote command for all of them.
fn git_objects(g: &Global, ids: &[String], max_size: u64) -> Result<i32> {
    let p = project()?;
    let p = Project { rel_cwd: String::new(), ..p };
    let dir = p.state_dir().join("git-objects");
    std::fs::create_dir_all(&dir)?;
    let wanted: Vec<&String> = ids
        .iter()
        .filter(|id| id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit()) && id.bytes().any(|b| b != b'0'))
        .filter(|id| !dir.join(id.as_str()).exists())
        .collect();
    let mut stored = 0;
    if !wanted.is_empty() {
        // Output: "blob <id> <size>\n<bytes>" per object, "skip <id>" otherwise.
        let script = r#"max=$1; shift; for o do s=$(git cat-file -s "$o" 2>/dev/null) || { echo "skip $o"; continue; }; if [ "$s" -gt "$max" ]; then echo "skip $o"; continue; fi; echo "blob $o $s"; git cat-file blob "$o"; done"#;
        let mut argv = vec!["sh".to_string(), "-c".into(), script.into(), "sh".into(), max_size.to_string()];
        argv.extend(wanted.iter().map(|s| s.to_string()));
        let out = devsync_client::remote::run(&p, &argv, std::time::Duration::from_secs(300))?;
        let buf = &out.stdout;
        let mut i = 0;
        while i < buf.len() {
            let nl = match buf[i..].iter().position(|&b| b == b'\n') {
                Some(n) => i + n,
                None => break,
            };
            let header = String::from_utf8_lossy(&buf[i..nl]).to_string();
            i = nl + 1;
            let parts: Vec<&str> = header.split(' ').collect();
            if parts.len() == 3 && parts[0] == "blob" {
                let size: usize = parts[2].parse().unwrap_or(0);
                let end = (i + size).min(buf.len());
                if end - i == size {
                    let tmp = dir.join(format!(".{}.tmp", parts[1]));
                    std::fs::write(&tmp, &buf[i..end])?;
                    std::fs::rename(&tmp, dir.join(parts[1]))?;
                    stored += 1;
                }
                i = end;
            }
        }
    }
    if g.json {
        print_json(json!({"event": "git_objects", "stored": stored, "dir": dir}));
    }
    Ok(0)
}

/// git subcommands that can change files in the working tree.
fn git_changes_files(args: &[String]) -> bool {
    const WRITES: &[&str] = &[
        "checkout",
        "switch",
        "restore",
        "reset",
        "pull",
        "merge",
        "rebase",
        "stash",
        "cherry-pick",
        "revert",
        "am",
        "apply",
        "clean",
        "mv",
        "rm",
        "submodule",
        "worktree",
    ];
    // First non-option word is the subcommand (skips e.g. `-c k=v`, `-C dir`).
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-c" || a == "-C" {
            it.next();
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        return WRITES.contains(&a.as_str());
    }
    false
}

fn git(g: &Global, args: &[String], no_push: bool, no_pull: bool) -> Result<i32> {
    let p = project()?;
    let rep = reporter(g, true, true);
    if !no_push {
        sync::push(&p, rep.as_ref(), sync::PushOptions::default())?;
    }
    let mut argv = vec!["git".to_string()];
    argv.extend(args.iter().cloned());
    let out = devsync_client::remote::run(&p, &argv, std::time::Duration::from_secs(600))?;
    let code = out.code.unwrap_or(1);
    if g.json {
        print_json(json!({"event": "git", "code": out.code, "stdout": String::from_utf8_lossy(&out.stdout),
            "stderr": String::from_utf8_lossy(&out.stderr)}));
    } else {
        use std::io::Write;
        let _ = std::io::stdout().write_all(&out.stdout);
        let _ = std::io::stderr().write_all(&out.stderr);
    }
    if !no_pull && git_changes_files(args) {
        sync::pull_remote_changes(&p, rep.as_ref())?;
    }
    Ok(code)
}

fn pull(g: &Global, paths: &[String], force: bool, to: Option<&Path>, dry_run: bool) -> Result<i32> {
    let p = project()?;
    let rep = reporter(g, false, false);
    if let Some(dest) = to {
        if paths.is_empty() {
            bail!("--to needs at least one path: devsync pull <path…> --to <dir>");
        }
        if dry_run {
            bail!("--dry-run is not supported with --to");
        }
        let mut n = 0;
        for path in paths {
            n += sync::pull(&p, rep.as_ref(), path, Some(dest))?.len();
        }
        if g.json {
            print_json(json!({"event": "pull_done", "copied": n, "to": dest}));
        } else {
            human(g).ok(&format!("copied {n} file(s) to {}", dest.display()));
        }
        return Ok(0);
    }
    let stats = if paths.is_empty() {
        if force {
            bail!("--force needs explicit paths (it overwrites local edits)");
        }
        sync::pull_project(&p, rep.as_ref(), dry_run)?
    } else {
        sync::pull_paths(&p, rep.as_ref(), paths, force, dry_run)?
    };
    if g.json {
        print_json(json!({"event": "pull_done", "updated": stats.updated,
            "conflicts": stats.conflicts.iter().map(|(a, b)| json!({"path": a, "saved_as": b})).collect::<Vec<_>>(),
            "dry_run": dry_run}));
    } else if stats.updated.is_empty() && stats.conflicts.is_empty() {
        human(g).ok("already up to date with the remote");
    } else if !dry_run {
        human(g).ok(&format!(
            "pulled {} file(s){}",
            stats.updated.len(),
            if stats.conflicts.is_empty() {
                String::new()
            } else {
                format!(", {} conflict(s) saved as *.devsync-remote", stats.conflicts.len())
            }
        ));
    }
    Ok(0)
}

fn ls(g: &Global, paths: &[String], changed: bool, all: bool) -> Result<i32> {
    let p = project()?;
    let rep = reporter(g, true, true);
    let mut files = sync::list_remote(&p, rep.as_ref(), paths, all)?;
    if changed {
        files.retain(|f| f.status != sync::RemoteStatus::Same);
    }
    if g.json {
        print_json(json!({"files": files}));
        return Ok(0);
    }
    if files.is_empty() {
        println!("{}", if changed { "no remote changes" } else { "no files" });
    }
    for f in &files {
        let mark = match f.status {
            sync::RemoteStatus::New => '+',
            sync::RemoteStatus::Changed => '~',
            sync::RemoteStatus::Edited => '!',
            sync::RemoteStatus::Same => '=',
        };
        println!("{mark} {:>9}  {}", bytes(f.size), f.path);
    }
    Ok(0)
}

// -------------------------------------------------------------------- jobs

fn join_command(parts: &[String]) -> String {
    if parts.len() == 1 {
        parts[0].clone()
    } else {
        parts.iter().map(|a| sh_quote(a)).collect::<Vec<_>>().join(" ")
    }
}

fn exit_code_of(j: &jobs::LocalJob) -> i32 {
    match j.state.as_str() {
        "exited" => j.exit_code.unwrap_or(1),
        _ => EXIT_DEVSYNC,
    }
}

fn exec(g: &Global, no_push: bool, no_pull_back: bool, command: &[String]) -> Result<i32> {
    let p = project()?;
    let rep = reporter(g, true, true);
    let cmd = join_command(command);
    if !no_push {
        sync::push(&p, rep.as_ref(), sync::PushOptions::default())?;
    }
    let job = jobs::submit(&p, &cmd, rep.as_ref())?;
    if g.verbose {
        rep.event(Event::Info {
            message: format!("→ {}:./{} $ {cmd}  [job {}]", p.config.remote.host, p.rel_cwd, job.id),
        });
    }
    let mut out = std::io::stdout().lock();
    let finished = match jobs::follow(&p, &job.id, &mut out, rep.as_ref()) {
        Ok(j) => j,
        Err(ClientError::Interrupted) => {
            devsync_client::INTERRUPTED.store(false, Ordering::SeqCst);
            rep.event(Event::Warning { message: format!("interrupted — stopping remote job {}", job.id) });
            jobs::kill(&p, &job.id, rep.as_ref())?;
            return Ok(130);
        }
        Err(e) => return Err(e.into()),
    };
    drop(out);
    if finished.state == "lost" {
        rep.event(Event::Warning {
            message: format!("job {} was lost on the remote (machine rebooted or agent killed?)", job.id),
        });
    }
    let mut j = finished;
    if !no_pull_back && p.config.exec.pull_back && j.state == "exited" {
        sync::pull_back(&p, rep.as_ref(), &j.id)?;
    }
    j.pulled_back = true;
    jobs::save(&p, &j)?;
    let code = exit_code_of(&j);
    if code != 0 && !g.quiet && !g.json {
        human(g).line(&format!("remote command exited with {code}"));
    }
    Ok(code)
}

fn submit(g: &Global, no_push: bool, command: &[String]) -> Result<i32> {
    let p = project()?;
    let rep = reporter(g, true, false);
    if !no_push {
        sync::push(&p, rep.as_ref(), sync::PushOptions::default())?;
    }
    let job = jobs::submit(&p, &join_command(command), rep.as_ref())?;
    if g.json {
        print_json(json!({"event": "submitted", "id": job.id, "output": jobs::output_path(&p, &job.id)}));
    } else {
        println!("{}", job.id);
        let h = human(g);
        if !g.quiet {
            h.ok(&format!("started job {} (runs on its own; no connection is kept open)", job.id));
            h.line(&format!(
                "output: devsync logs {}   results folder: {}",
                job.id,
                jobs::job_dir(&p, &job.id).strip_prefix(&p.root).unwrap_or(&jobs::job_dir(&p, &job.id)).display()
            ));
        }
    }
    Ok(0)
}

fn logs(g: &Global, id: Option<&str>, no_follow: bool) -> Result<i32> {
    let p = project()?;
    let rep = reporter(g, true, true);
    let id = jobs::resolve_id(&p, id)?;
    let job = match jobs::load(&p, &id) {
        Some(j) => j,
        None => {
            // Known only remotely (e.g. started from another machine): adopt it.
            let j = jobs::LocalJob {
                id: id.clone(),
                command: String::new(),
                cwd: String::new(),
                host: p.host_label(),
                started_unix: 0,
                state: "running".into(),
                exit_code: None,
                offset: 0,
                pulled_back: true,
            };
            jobs::save(&p, &j)?;
            std::fs::write(jobs::output_path(&p, &id), b"")?;
            j
        }
    };
    let mut out = std::io::stdout().lock();
    jobs::print_local(&p, &id, &mut out)?;
    if job.finished() || no_follow {
        return Ok(if job.finished() { exit_code_of(&job) } else { 0 });
    }
    match jobs::follow(&p, &id, &mut out, rep.as_ref()) {
        Ok(mut j) => {
            drop(out);
            if !j.pulled_back && p.config.exec.pull_back && j.state == "exited" {
                sync::pull_back(&p, rep.as_ref(), &j.id)?;
            }
            j.pulled_back = true;
            jobs::save(&p, &j)?;
            Ok(exit_code_of(&j))
        }
        Err(ClientError::Interrupted) => {
            drop(out);
            if !g.json {
                human(g).line(&format!("detached; job keeps running. Resume with: devsync logs {id}"));
            }
            Ok(0)
        }
        Err(e) => Err(e.into()),
    }
}

fn age(started: i64) -> String {
    let now =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let s = (now - started).max(0);
    match s {
        _ if started == 0 => "?".into(),
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

fn list_jobs(g: &Global, remote: bool) -> Result<i32> {
    let p = project()?;
    if remote {
        let rep = reporter(g, true, true);
        let list = jobs::remote_list(&p, rep.as_ref())?;
        if g.json {
            let v: Vec<_> = list
                .iter()
                .map(|j| {
                    json!({"id": j.id, "command": j.command, "cwd": j.cwd, "started_unix": j.started_unix,
                    "state": format!("{:?}", j.state), "output_len": j.output_len})
                })
                .collect();
            print_json(json!(v));
        } else {
            for j in list {
                println!("{:<18} {:<12} {:>5}  {}", j.id, format!("{:?}", j.state), age(j.started_unix), j.command);
            }
        }
        return Ok(0);
    }
    let list = jobs::local_jobs(&p);
    if g.json {
        let v: Vec<_> = list
            .iter()
            .map(|j| {
                let mut o = serde_json::to_value(j).unwrap_or_default();
                o["output"] = json!(jobs::output_path(&p, &j.id));
                o
            })
            .collect();
        print_json(json!(v));
    } else if list.is_empty() {
        println!("no jobs yet");
    } else {
        println!("{:<18} {:<8} {:>4} {:>5}  COMMAND", "ID", "STATE", "EXIT", "AGE");
        for j in list.iter().take(30) {
            let exit = j.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "-".into());
            let cmd: String = j.command.chars().take(70).collect();
            println!("{:<18} {:<8} {:>4} {:>5}  {}", j.id, j.state, exit, age(j.started_unix), cmd);
        }
    }
    Ok(0)
}

fn kill(g: &Global, id: Option<&str>) -> Result<i32> {
    let p = project()?;
    let rep = reporter(g, true, false);
    let id = jobs::resolve_id(&p, id)?;
    jobs::kill(&p, &id, rep.as_ref())?;
    let _ = jobs::collect(&p, rep.as_ref());
    if g.json {
        print_json(json!({"event": "killed", "id": id}));
    } else {
        human(g).ok(&format!("stopped job {id}"));
    }
    Ok(0)
}

fn collect(g: &Global) -> Result<i32> {
    let p = project()?;
    let rep = reporter(g, true, false);
    let running = p.with_retry(rep.as_ref(), || jobs::collect(&p, rep.as_ref()))?;
    if g.json {
        print_json(json!({"event": "collected", "running": running}));
    } else if !g.quiet {
        human(g).line(&format!("{running} job(s) still running"));
    }
    Ok(0)
}

// ------------------------------------------------------------ diagnostics

fn tree(g: &Global, path: &str) -> Result<i32> {
    use devsync_client::selection::{state, State};
    let p = project()?;
    let rep = reporter(g, true, true);
    let nodes = sync::remote_tree(&p, rep.as_ref(), path)?;
    let label = |n: &devsync_proto::TreeNode| -> &'static str {
        if n.excluded {
            "excluded"
        } else {
            match state(&p.config.sync, &n.path) {
                State::Selected => "selected",
                State::Partial => "partial",
                State::NotSelected => "unselected",
            }
        }
    };
    if g.json {
        let items: Vec<_> = nodes
            .iter()
            .map(|n| {
                json!({"path": n.path, "dir": n.dir, "size": n.size, "files": n.files,
                       "complete": n.complete, "excluded": n.excluded, "selection": label(n)})
            })
            .collect();
        print_json(json!({"path": devsync_fs::norm_rel(path), "nodes": items}));
        return Ok(0);
    }
    let shown = devsync_fs::norm_rel(path);
    println!("{}:{}/{}", p.config.remote.host, p.config.remote.path.trim_end_matches('/'), shown);
    if nodes.is_empty() {
        println!("  (empty)");
    }
    for n in &nodes {
        let name = n.path.rsplit('/').next().unwrap_or(&n.path);
        let name = if n.dir { format!("{name}/") } else { name.to_string() };
        let mark = match label(n) {
            "selected" => "[x]",
            "partial" => "[~]",
            "unselected" => "[ ]",
            _ => " - ",
        };
        let approx = if n.complete { "  " } else { "≥ " };
        let detail = if n.excluded {
            "always excluded".to_string()
        } else if n.dir {
            format!("{approx}{:>9}  {approx}{} files", bytes(n.size), n.files)
        } else {
            format!("  {:>9}", bytes(n.size))
        };
        println!("  {mark} {name:<32} {detail}");
    }
    if !g.quiet {
        println!();
        println!(
            "[x] synced  [~] partly  [ ] not synced — change with: devsync select <path> / devsync unselect <path>"
        );
    }
    Ok(0)
}

fn select(g: &Global, paths: &[String], pull: bool, all: bool) -> Result<i32> {
    use devsync_client::selection;
    let p = project()?;
    if all {
        selection::reset(&p.root)?;
        human(g).ok("now syncing the whole project");
        return Ok(0);
    }
    if paths.is_empty() {
        let s = &p.config.sync;
        if g.json {
            print_json(json!({"include": s.include, "ignore": s.ignore}));
        } else if s.include.is_empty() && s.ignore.is_empty() {
            println!("syncing the whole project (except built-in excludes)");
        } else {
            println!("synced: {}", if s.include.is_empty() { "everything".into() } else { s.include.join(", ") });
            if !s.ignore.is_empty() {
                println!("except: {}", s.ignore.join(", "));
            }
        }
        return Ok(0);
    }
    for path in paths {
        selection::select(&p.root, path)?;
    }
    if !g.json {
        human(g).ok(&format!("selected {}", paths.join(", ")));
    }
    if pull {
        let p = project()?; // reload with the new selection
        let rep = reporter(g, false, false);
        let stats = sync::pull_paths(&p, rep.as_ref(), paths, false, false)?;
        if !g.json {
            human(g).ok(&format!("pulled {} file(s)", stats.updated.len()));
        }
    }
    if g.json {
        print_json(json!({"event": "selected", "paths": paths}));
    }
    Ok(0)
}

fn unselect(g: &Global, paths: &[String]) -> Result<i32> {
    let p = project()?;
    for path in paths {
        devsync_client::selection::unselect(&p.root, path)?;
    }
    if g.json {
        print_json(json!({"event": "unselected", "paths": paths}));
    } else {
        human(g).ok(&format!("no longer syncing {} (left untouched on the remote)", paths.join(", ")));
    }
    Ok(0)
}

fn changes(g: &Global) -> Result<i32> {
    let p = project()?;
    let list = sync::changes(&p)?;
    if g.json {
        print_json(json!({"root": p.root, "changes": list}));
        return Ok(0);
    }
    if list.is_empty() {
        println!("nothing to push: the remote has all local changes");
    }
    for c in &list {
        let letter = match c.status {
            sync::ChangeKind::Added => 'A',
            sync::ChangeKind::Modified => 'M',
            sync::ChangeKind::Deleted => 'D',
        };
        println!("{letter} {}", c.path);
    }
    Ok(0)
}

fn status(g: &Global) -> Result<i32> {
    let p = project()?;
    let pending = sync::pending(&p)?;
    let running = jobs::local_jobs(&p).iter().filter(|j| !j.finished()).count();
    let t0 = Instant::now();
    let conn = p.connect().map(|s| {
        let info = s.info.clone();
        s.close();
        info
    });
    let latency = t0.elapsed().as_millis() as u64;
    if g.json {
        let (connected, remote, error) = match &conn {
            Ok(i) => (true, json!(i), serde_json::Value::Null),
            Err(e) => (false, serde_json::Value::Null, json!(e.to_string())),
        };
        print_json(json!({
            "root": p.root, "host": p.config.remote.host, "path": p.config.remote.path,
            "pending": pending.len(), "pending_files": pending.iter().take(50).collect::<Vec<_>>(),
            "running_jobs": running, "connected": connected, "latency_ms": latency,
            "remote": remote, "error": error,
        }));
        return Ok(0);
    }
    println!("project   {}", p.root.display());
    println!("remote    {}:{}", p.config.remote.host, p.config.remote.path);
    match conn {
        Ok(i) => {
            println!("link      ok ({latency} ms) — agent {} on {}/{}, root {}", i.agent_version, i.os, i.arch, i.root)
        }
        Err(e) => println!("link      DOWN — {e}"),
    }
    if pending.is_empty() {
        println!("changes   none pending");
    } else {
        println!("changes   {} file(s) not yet pushed", pending.len());
        for f in pending.iter().take(10) {
            println!("            {f}");
        }
    }
    println!("jobs      {running} running");
    Ok(0)
}

fn doctor(g: &Global) -> Result<i32> {
    let h = human(g);
    let mut failures = 0;
    let mut check = |name: &str, r: Result<String>| match r {
        Ok(msg) => h.ok(&format!("✓ {name}: {msg}")),
        Err(e) => {
            failures += 1;
            h.error(&format!("✗ {name}: {e:#}"));
        }
    };
    let p = match project() {
        Ok(p) => {
            check("config", Ok(format!("{}", p.root.join(CONFIG_FILE).display())));
            p
        }
        Err(e) => {
            check("config", Err(e));
            return Ok(1);
        }
    };
    if p.config.remote.host != "local" {
        let ssh = std::env::var("DEVSYNC_SSH").unwrap_or_else(|_| "ssh".into());
        check(
            "ssh client",
            std::process::Command::new(&ssh)
                .arg("-V")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
                .map_err(|e| anyhow!("{ssh}: {e}")),
        );
        for (os, arch) in [("linux", "x86_64"), ("linux", "aarch64")] {
            let found = devsync_transport::deploy::find_agent_binary(os, arch);
            let r = found
                .map(|b| b.display().to_string())
                .ok_or_else(|| anyhow!("missing (only needed if the remote is {os}/{arch}; build with `make agents`)"));
            check(&format!("agent build {os}/{arch}"), r);
        }
    }
    let t0 = Instant::now();
    let conn = p.connect();
    match conn {
        Ok(s) => {
            let i = s.info.clone();
            s.close();
            check(
                "connect + agent",
                Ok(format!("{} ms, agent {} on {}/{}", t0.elapsed().as_millis(), i.agent_version, i.os, i.arch)),
            );
        }
        Err(e) => {
            check("connect + agent", Err(anyhow!(e)));
            return Ok(1);
        }
    }
    let rep = devsync_client::events::Silent;
    check(
        "sync",
        sync::push(&p, &rep, sync::PushOptions { dry_run: true, force: false })
            .map(|s| format!("{} file(s) would be uploaded, {} deleted", s.plan.upload.len(), s.plan.delete.len()))
            .map_err(Into::into),
    );
    let job_check = (|| -> Result<String> {
        let j = jobs::submit(&p, "echo devsync-doctor-ok", &rep)?;
        let mut buf = Vec::new();
        let done = jobs::follow(&p, &j.id, &mut buf, &rep)?;
        let out = String::from_utf8_lossy(&buf);
        if done.exit_code == Some(0) && out.contains("devsync-doctor-ok") {
            Ok(format!("ran a test command remotely (job {})", j.id))
        } else {
            bail!("unexpected result: state {} exit {:?} output {out:?}", done.state, done.exit_code)
        }
    })();
    check("remote execution", job_check);
    if failures == 0 {
        h.ok("all checks passed");
        Ok(0)
    } else {
        Ok(1)
    }
}

// ----------------------------------------------------------- interactive

fn forward(g: &Global, ports: Vec<u16>) -> Result<i32> {
    let p = project()?;
    let ports = if ports.is_empty() { p.config.exec.ports.clone() } else { ports };
    if ports.is_empty() {
        bail!("no ports given (pass them as arguments or set exec.ports in {CONFIG_FILE})");
    }
    let target = p.target();
    let h = human(g);
    let mut delay = Duration::from_secs(1);
    loop {
        let mut cmd = target.forward_command(&ports).ok_or_else(|| anyhow!("port forwarding needs an ssh remote"))?;
        h.ok(&format!(
            "forwarding {} (localhost → {}). Ctrl-C to stop",
            ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", "),
            p.config.remote.host
        ));
        let started = Instant::now();
        let mut child = cmd.spawn().context("cannot start ssh")?;
        loop {
            if devsync_client::interrupted() {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(0);
            }
            if let Some(st) = child.try_wait()? {
                if started.elapsed() > Duration::from_secs(30) {
                    delay = Duration::from_secs(1);
                }
                h.warn(&format!("tunnel closed ({st}); reconnecting in {}s", delay.as_secs()));
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if devsync_client::sleep_interruptible(delay).is_err() {
            return Ok(0);
        }
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

fn shell() -> Result<i32> {
    let p = project()?;
    let mut cmd = p.target().shell_command(&p.rel_cwd);
    if p.config.remote.host == "local" {
        cmd.current_dir(devsync_agent::expand_root(&p.config.remote.path).join(&p.rel_cwd));
    }
    let st = cmd.status().context("cannot start shell")?;
    let _ = std::io::stdout().flush();
    Ok(st.code().unwrap_or(1))
}
