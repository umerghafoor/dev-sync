//! Installs the agent binary on the remote, like VS Code's remote server:
//! detect the remote platform, pick the matching static build, upload it
//! once under a content-addressed name, reuse it afterwards.

use crate::session::classify_ssh_failure;
use crate::{SshConfig, TransportError};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Remote directory (relative to `$HOME`) holding agent binaries.
pub const REMOTE_BIN_DIR: &str = ".devsync/bin";

/// Normalises `uname -s` / `uname -m` output to Rust's `consts::OS` / `ARCH`.
pub fn normalize(os: &str, arch: &str) -> (String, String) {
    let os = match os.to_ascii_lowercase().as_str() {
        "darwin" => "macos".to_string(),
        other => other.to_string(),
    };
    let arch = match arch {
        "amd64" | "x86_64" => "x86_64",
        "arm64" | "aarch64" => "aarch64",
        other => other,
    }
    .to_string();
    (os, arch)
}

pub fn target_triple(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        _ => return None,
    })
}

/// Directories searched for prebuilt agent binaries.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = std::env::var_os("DEVSYNC_AGENT_DIR") {
        dirs.push(PathBuf::from(d));
    }
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        dirs.push(exe_dir.join("agents"));
        dirs.push(exe_dir.join("../share/devsync/agents"));
        dirs.push(exe_dir.join("../lib/devsync/agents"));
        // cargo layout: target/{release,debug}/devsync next to target/<triple>/release/devsync
        dirs.push(exe_dir.join(".."));
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".devsync/agents"));
    }
    dirs
}

/// Finds an agent binary able to run on `os`/`arch`.
pub fn find_agent_binary(os: &str, arch: &str) -> Option<PathBuf> {
    let triple = target_triple(os, arch);
    if let Some(t) = triple {
        for d in search_dirs() {
            for c in [d.join(format!("devsync-{t}")), d.join(t).join("devsync"), d.join(t).join("release/devsync")] {
                if c.is_file() {
                    return Some(c);
                }
            }
        }
    }
    if os == std::env::consts::OS && arch == std::env::consts::ARCH {
        return std::env::current_exe().ok();
    }
    None
}

/// Content-addressed remote file name for a binary.
pub fn agent_name_for(bin: &Path) -> std::io::Result<String> {
    let h = devsync_fs::hash_file(bin)?;
    Ok(format!("devsync-{}-{}", env!("CARGO_PKG_VERSION"), &devsync_proto::hash_hex(&h)[..12]))
}

pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Runs a one-shot remote command, optionally feeding a file to its stdin.
pub fn run(ssh: &SshConfig, remote_cmd: &str, stdin: Option<&Path>, timeout: Duration) -> Result<Output, TransportError> {
    let mut c: Command = ssh.command();
    c.arg("-T").arg(&ssh.host).arg(remote_cmd);
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    match stdin {
        Some(p) => {
            let f = std::fs::File::open(p).map_err(|e| TransportError::Deploy(format!("{}: {e}", p.display())))?;
            c.stdin(f);
        }
        None => {
            c.stdin(Stdio::null());
        }
    }
    let mut child = c.spawn().map_err(|e| TransportError::Fatal(format!("cannot run ssh: {e}")))?;
    let mut out = child.stdout.take().expect("stdout");
    let mut err = child.stderr.take().expect("stderr");
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code(),
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TransportError::Timeout(timeout));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(TransportError::Connect(e.to_string())),
        }
    };
    let o = Output { code, stdout: t_out.join().unwrap_or_default(), stderr: t_err.join().unwrap_or_default() };
    if o.code == Some(255) {
        return Err(classify_ssh_failure(o.code, o.stderr.trim()));
    }
    Ok(o)
}

/// Detects the remote platform as (os, arch).
pub fn probe(ssh: &SshConfig) -> Result<(String, String), TransportError> {
    let o = run(ssh, "uname -sm", None, Duration::from_secs(30))?;
    let mut it = o.stdout.split_whitespace();
    match (it.next(), it.next()) {
        (Some(os), Some(arch)) => Ok(normalize(os, arch)),
        _ => Err(TransportError::Deploy(format!("cannot detect remote platform (uname said {:?})", o.stdout.trim()))),
    }
}

/// Makes sure a compatible agent exists remotely and returns its file name.
pub fn ensure_agent(ssh: &SshConfig) -> Result<String, TransportError> {
    let (os, arch) = probe(ssh)?;
    let bin = find_agent_binary(&os, &arch).ok_or_else(|| {
        TransportError::Deploy(format!(
            "no devsync build for remote platform {os}/{arch}. Build it with `make agents` \
             (or `cargo build --release --target {}`) or point DEVSYNC_AGENT_DIR at a directory \
             containing devsync-<target-triple>",
            target_triple(&os, &arch).unwrap_or("<triple>")
        ))
    })?;
    let name = agent_name_for(&bin).map_err(|e| TransportError::Deploy(e.to_string()))?;
    let dest = format!("~/{REMOTE_BIN_DIR}/{name}");
    let check = run(ssh, &format!("test -x {dest}"), None, Duration::from_secs(30))?;
    if check.code == Some(0) {
        return Ok(name);
    }
    tracing::info!("uploading agent {} ({}) to remote {os}/{arch}", name, bin.display());
    let tmp = format!("{dest}.tmp$$");
    let cmd = format!("mkdir -p ~/{REMOTE_BIN_DIR} && cat > {tmp} && chmod 755 {tmp} && mv -f {tmp} {dest}");
    let up = run(ssh, &cmd, Some(&bin), Duration::from_secs(600))?;
    if up.code != Some(0) {
        return Err(TransportError::Deploy(format!("upload failed: {}", up.stderr.trim())));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_mapping() {
        assert_eq!(normalize("Linux", "x86_64"), ("linux".into(), "x86_64".into()));
        assert_eq!(normalize("Darwin", "arm64"), ("macos".into(), "aarch64".into()));
        assert_eq!(target_triple("linux", "aarch64"), Some("aarch64-unknown-linux-musl"));
        assert_eq!(target_triple("freebsd", "x86_64"), None);
    }
}
