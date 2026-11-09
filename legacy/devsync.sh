#!/usr/bin/env bash
# devsync — edit locally, run remotely. Built for flaky connections and coding agents.
#
#   devsync init user@host:~/project   create .devsync config in the current dir
#   devsync agent-setup                route your coding agent's commands to the remote
#
#   devsync exec <cmd...>     sync, run cmd in the matching remote dir, stream output,
#                             pull back files the command changed. Survives disconnects.
#   devsync bg <cmd...>       same, but detached; prints a job id
#   devsync logs [id]         follow a job's output (default: latest). Ctrl-C detaches.
#   devsync jobs              list recent remote jobs
#   devsync kill [id]         stop a remote job
#   devsync push              sync local -> remote (retries until it lands)
#   devsync watch             push on every save
#   devsync diff              what a push would change (dry run)
#   devsync pull <path> [dst] fetch a file/dir from the remote project
#   devsync forward <port..>  forward remote ports to localhost
#   devsync shell             interactive shell in the remote project dir
#
# .devsync (plain shell vars):
#   REMOTE=user@host   REMOTE_DIR=~/project
#   USE_GITIGNORE=0    0 = sync .gitignore'd files too (.env, configs, data); 1 = skip them
#   DELETE=0           1 = remove remote files you deleted locally
#   PULL_BACK=1        bring back files a remote command created/changed
#   PORTS=""           ports auto-forwarded on exec, e.g. "3000 8000"
#   REMOTE_PRELUDE=""  run before every command, e.g. "source ~/.nvm/nvm.sh"
#   SSH_OPTS=""        extra ssh flags, e.g. "-p 2222 -i ~/.ssh/key"
#   RETRIES=0          0 = retry forever
# Always excluded: .git node_modules .venv __pycache__ … (rebuild those on the remote).
# Add your own excludes to .syncignore (gitignore-style).

set -u

CONFIG=.devsync
JOBS=.devsync-jobs
ORIG_PWD=$PWD
SELF=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
TMP=${TMPDIR:-/tmp}; TMP=${TMP%/}
DEFAULT_EXCLUDES=".git/
.devsync
.devsync*
$JOBS/
.DS_Store
node_modules/
__pycache__/
*.pyc
.venv/
venv/
.mypy_cache/
.pytest_cache/
.idea/
.claude/"

c_red=$'\033[31m'; c_grn=$'\033[32m'; c_yel=$'\033[33m'; c_dim=$'\033[2m'; c_off=$'\033[0m'
[ -t 2 ] || { c_red=; c_grn=; c_yel=; c_dim=; c_off=; }
# All devsync chatter goes to stderr so stdout is purely the remote command's output.
log()  { printf '%s[devsync]%s %s\n' "$c_dim" "$c_off" "$*" >&2; }
ok()   { log "${c_grn}$*${c_off}"; }
warn() { log "${c_yel}$*${c_off}"; }
die()  { log "${c_red}$*${c_off}"; exit 1; }
sq()   { printf "'%s'" "$(printf %s "$1" | sed "s/'/'\\\\''/g")"; }

# ---------- config ----------

find_root() {
  local d=$ORIG_PWD
  while :; do
    [ -f "$d/$CONFIG" ] && { PROJECT_ROOT=$d; return 0; }
    [ "$d" = "/" ] && return 1
    d=$(dirname "$d")
  done
}

load_config() {
  find_root || die "no $CONFIG found. Run: devsync init user@host:~/path"
  cd "$PROJECT_ROOT" || die "cannot cd to $PROJECT_ROOT"
  REMOTE=""; REMOTE_DIR=""; DELETE=0; PULL_BACK=1; PORTS=""; REMOTE_PRELUDE=""; SSH_OPTS=""; RETRIES=0
  USE_GITIGNORE=0
  # shellcheck disable=SC1090
  . "./$CONFIG"
  # Re-read REMOTE_DIR raw: sourcing expands a leading ~ to the LOCAL home, but it means the remote one.
  local raw; raw=$(sed -n 's/^[[:space:]]*REMOTE_DIR=//p' "$CONFIG" | tail -n 1)
  raw=${raw#[\"\']}; raw=${raw%[\"\']}
  [ -n "$raw" ] && REMOTE_DIR=$raw
  [ -n "$REMOTE" ] && [ -n "$REMOTE_DIR" ] || die "REMOTE and REMOTE_DIR must be set in $CONFIG"
  REMOTE_DIR=${REMOTE_DIR%/}

  # Relative path of where we were invoked, so commands run in the matching remote dir.
  REL=${ORIG_PWD#"$PROJECT_ROOT"}; REL=${REL#/}

  # Prefer real rsync 3.x (Homebrew) over macOS openrsync.
  RSYNC=${DEVSYNC_RSYNC:-}
  if [ -z "$RSYNC" ]; then
    for c in /opt/homebrew/bin/rsync /usr/local/bin/rsync rsync; do
      command -v "$c" >/dev/null 2>&1 && { RSYNC=$c; break; }
    done
  fi
  [ -n "$RSYNC" ] || die "rsync not found"

  # One persistent multiplexed SSH connection; keepalives detect a dead link in ~15s.
  CTL_DIR="$TMP/devsync-ctl"; mkdir -p "$CTL_DIR" && chmod 700 "$CTL_DIR"
  SSH_CMD="${DEVSYNC_SSH:-ssh} -o ControlMaster=auto -o ControlPath=$CTL_DIR/%C -o ControlPersist=10m \
-o ServerAliveInterval=5 -o ServerAliveCountMax=3 -o ConnectTimeout=10 $SSH_OPTS"

  EXCL_FILE="$TMP/devsync-excl.$$"
  {
    printf '%s\n' "$DEFAULT_EXCLUDES"
    # .gitignore'd files (.env, local configs, data) ARE synced unless USE_GITIGNORE=1.
    local files=.syncignore
    [ "$USE_GITIGNORE" = "1" ] && files=".gitignore .syncignore"
    for f in $files; do
      [ -f "$f" ] && grep -vE '^[[:space:]]*(#|!|$)' "$f"
    done
  } > "$EXCL_FILE"
  CLEANUP="$EXCL_FILE"
  trap 'rm -f $CLEANUP' EXIT
}

# Run a POSIX sh script on the remote. Shipped base64-encoded: no quoting surprises.
remote_script() {
  local b64; b64=$(printf '%s\n' "$1" | base64 | tr -d '\n')
  $SSH_CMD "$REMOTE" "echo $b64 | base64 -d | sh"
}

# Prefix for remote scripts: resolves REMOTE_DIR (incl. ~) into $R.
remote_hdr() {
  printf 'R=%s; case $R in "~"|"~/"*) R="$HOME${R#\\~}";; esac\n' "$(sq "$REMOTE_DIR")"
}

is_net_err() { case $1 in 255|10|12|30|35) return 0;; *) return 1;; esac; }

# Retry a command on network errors with backoff.
with_retry() {
  local attempt=1 delay=1 rc
  while :; do
    "$@"; rc=$?
    [ $rc -eq 0 ] && return 0
    is_net_err $rc || return $rc
    [ "$RETRIES" != "0" ] && [ $attempt -ge "$RETRIES" ] && { warn "gave up after $attempt attempts"; return $rc; }
    warn "connection problem — retry #$attempt in ${delay}s"
    sleep "$delay"
    attempt=$((attempt + 1)); delay=$((delay * 2)); [ $delay -gt 30 ] && delay=30
  done
}

# ---------- sync ----------

rsync_push() {
  local del=""
  [ "$DELETE" = "1" ] && del="--delete --max-delete=200"
  # shellcheck disable=SC2086
  "$RSYNC" -rlptz --partial --timeout=30 --exclude-from="$EXCL_FILE" $del \
    -e "$SSH_CMD" --rsync-path="mkdir -p $REMOTE_DIR && rsync" \
    "$@" ./ "$REMOTE:$REMOTE_DIR/"
}

do_push() {   # $1 = quiet?
  local out="$TMP/devsync-push.$$" n
  with_retry rsync_push --out-format='%n' > "$out"; local rc=$?
  n=$(grep -cv '/$' "$out" 2>/dev/null); n=${n:-0}
  [ "${1:-}" = quiet ] || sed 's/^/  /' "$out" | grep -v '/$' >&2
  rm -f "$out"
  [ $rc -eq 0 ] || die "sync failed (rsync $rc)"
  PUSHED=$n
}

cmd_push() { local t0; t0=$(date +%s); do_push; ok "synced $PUSHED file(s) in $(( $(date +%s) - t0 ))s"; }

cmd_diff() { rsync_push -n --itemize-changes | grep -v '^\.' || log "nothing to push"; }

cmd_pull() {
  [ $# -ge 1 ] || die "usage: devsync pull <remote-path> [local-dest]"
  with_retry "$RSYNC" -rlptz --partial --timeout=30 -e "$SSH_CMD" \
    "$REMOTE:$REMOTE_DIR/$1" "${2:-./}" && ok "pulled $1"
}

cmd_watch() {
  local stamp="$TMP/devsync-stamp.$$" dirty=0
  CLEANUP="$CLEANUP $stamp $stamp.next"
  trap 'echo >&2; log "stopped"; exit 0' INT TERM

  do_push; ok "in sync — watching for changes (Ctrl-C to stop)"
  touch "$stamp"
  while :; do
    if [ $dirty -eq 0 ]; then
      if command -v fswatch >/dev/null 2>&1; then
        fswatch -1 -r -l 0.3 --exclude '/\.git/' --exclude 'node_modules' --exclude "$JOBS" . >/dev/null
      else
        while [ -z "$(find . -newer "$stamp" -type f -not -path './.git/*' -not -path '*/node_modules/*' 2>/dev/null | head -1)" ]; do
          sleep 1
        done
      fi
      sleep 0.3   # debounce editor writes
    fi
    touch "$stamp.next"
    if with_retry rsync_push --out-format='  %n' >&2; then
      mv -f "$stamp.next" "$stamp"; dirty=0; ok "synced"
    else
      rm -f "$stamp.next"; dirty=1; warn "sync failed — will retry"; sleep 5
    fi
  done
}

# ---------- remote jobs ----------
# Each command runs detached on the remote under .devsync-jobs/<id>/ (cmd, log, rc, pid, mark).
# The client just tails the log by byte offset, so a dropped connection loses nothing:
# reconnect and continue from where we were.

job_start() {   # $1 = command; sets JOB
  JOB=$(date +%Y%m%d-%H%M%S)-$$
  local body="$1"
  [ -n "$REMOTE_PRELUDE" ] && body="$REMOTE_PRELUDE
$body"
  local cmd64; cmd64=$(printf '%s\n' "$body" | base64 | tr -d '\n')
  local s
  s="$(remote_hdr)
J=\"\$R/$JOBS/$JOB\"; D=\"\$R\"$( [ -n "$REL" ] && printf '/%s' "$(sq "$REL")" )
mkdir -p \"\$J\" \"\$D\" || exit 1
find \"\$R/$JOBS\" -mindepth 1 -maxdepth 1 -mtime +2 -exec rm -rf {} + 2>/dev/null
echo $cmd64 | base64 -d > \"\$J/cmd\" || exit 1
touch \"\$J/mark\"; cd \"\$D\" || exit 1
if command -v bash >/dev/null 2>&1; then SH='bash -l'; else SH='sh -l'; fi
RUN=\"\$SH '\$J/cmd' > '\$J/log' 2>&1 < /dev/null; echo \\\$? > '\$J/rc'\"
if command -v setsid >/dev/null 2>&1; then setsid sh -c \"\$RUN\" >/dev/null 2>&1 < /dev/null &
else nohup sh -c \"\$RUN\" >/dev/null 2>&1 < /dev/null & fi
echo \$! > \"\$J/pid\""
  with_retry remote_script "$s" || die "could not start remote job"
  echo "$JOB" > "$PROJECT_ROOT/.devsync.last"
}

# Stream job output to stdout, resuming across disconnects. Returns when the job ends.
job_follow() {   # $1 = id
  local id=$1 local_log="$TMP/devsync-$1.out" off st delay=1
  [ -f "$local_log" ] || : > "$local_log"
  while :; do
    off=$(wc -c < "$local_log" | tr -d ' ')
    remote_script "$(remote_hdr)
J=\"\$R/$JOBS/$id\"; n=$off
[ -d \"\$J\" ] || { echo 'devsync: no such job $id' >&2; exit 3; }
while :; do
  fin=0; [ -f \"\$J/rc\" ] && fin=1
  if [ -f \"\$J/log\" ]; then
    sz=\$(wc -c < \"\$J/log\" | tr -d ' ')
    if [ \"\$sz\" -gt \"\$n\" ]; then tail -c +\$((n+1)) \"\$J/log\" | head -c \$((sz-n)); n=\$sz; fi
  fi
  [ \$fin = 1 ] && exit 0
  sleep 0.2
done" | tee -a "$local_log"
    st=${PIPESTATUS[0]}
    [ "$st" -eq 0 ] && return 0
    [ "$st" -eq 3 ] && return 3
    warn "connection lost — job keeps running remotely; reconnecting in ${delay}s"
    sleep $delay; delay=$((delay * 2)); [ $delay -gt 15 ] && delay=15
  done
}

job_rc() { with_retry remote_script "$(remote_hdr)
cat \"\$R/$JOBS/$1/rc\" 2>/dev/null || echo 1"; }

job_kill() {
  remote_script "$(remote_hdr)
J=\"\$R/$JOBS/$1\"; [ -f \"\$J/rc\" ] && exit 0
p=\$(cat \"\$J/pid\" 2>/dev/null) || exit 0
kill -TERM -\$p 2>/dev/null || { pkill -TERM -P \$p 2>/dev/null; kill -TERM \$p 2>/dev/null; }
echo 143 > \"\$J/rc\"" 2>/dev/null
}

# Bring back files the command created/changed (lockfiles, codegen, migrations…).
# --update: never overwrite a local file that is newer (you edited it meanwhile).
job_pullback() {
  [ "$PULL_BACK" = "1" ] || return 0
  local list="$TMP/devsync-changed.$$" got="$TMP/devsync-got.$$" n
  CLEANUP="$CLEANUP $list $got"
  remote_script "$(remote_hdr)
cd \"\$R\" || exit 0
find . \\( -name .git -o -name node_modules -o -name .venv -o -name venv -o -name __pycache__ \
  -o -name $JOBS -o -name .mypy_cache -o -name .pytest_cache \\) -prune \
  -o -type f -newer \"$JOBS/$1/mark\" -print 2>/dev/null | head -n 2001" > "$list" 2>/dev/null || return 0
  n=$(wc -l < "$list" | tr -d ' ')
  [ "$n" -eq 0 ] && return 0
  if [ "$n" -gt 2000 ]; then warn "command changed >2000 remote files — not pulling back (use devsync pull)"; return 0; fi
  sed -i '' 's|^\./||' "$list" 2>/dev/null || sed -i 's|^\./||' "$list"
  with_retry "$RSYNC" -lptz --update --timeout=30 --files-from="$list" --exclude-from="$EXCL_FILE" \
    --out-format='%n' -e "$SSH_CMD" "$REMOTE:$REMOTE_DIR/" ./ > "$got" 2>/dev/null
  if [ -s "$got" ]; then log "pulled back from remote: $(tr '\n' ' ' < "$got")"; fi
}

cmd_exec() {
  [ $# -gt 0 ] || die "usage: devsync exec <command...>"
  [ "$1" = "--" ] && shift
  local cmd="$*" rc
  do_push quiet
  forward_ports
  log "→ $REMOTE:${REL:-.} (synced $PUSHED) \$ $cmd"
  job_start "$cmd"
  trap 'echo >&2; warn "interrupted — stopping remote job $JOB"; job_kill "$JOB"; exit 130' INT TERM
  job_follow "$JOB"
  trap - INT TERM
  rc=$(job_rc "$JOB"); rc=${rc:-1}
  job_pullback "$JOB"
  rm -f "$TMP/devsync-$JOB.out"
  [ "$rc" = 0 ] || log "remote exit code $rc"
  exit "$rc"
}

cmd_bg() {
  [ $# -gt 0 ] || die "usage: devsync bg <command...>"
  [ "$1" = "--" ] && shift
  do_push quiet; forward_ports
  job_start "$*"
  ok "started job $JOB on $REMOTE (keeps running if you disconnect)"
  log "follow: devsync logs $JOB    stop: devsync kill $JOB"
  echo "$JOB"
}

last_job() { cat "$PROJECT_ROOT/.devsync.last" 2>/dev/null || die "no jobs yet"; }

cmd_logs() {
  local id=${1:-$(last_job)}
  trap 'echo >&2; log "detached (job still running). Resume: devsync logs $id"; exit 0' INT
  job_follow "$id"; local st=$?
  [ $st -eq 3 ] && die "no such job $id"
  log "job $id finished with exit code $(job_rc "$id")"
}

cmd_kill() { local id=${1:-$(last_job)}; job_kill "$id"; ok "stopped $id"; }

cmd_jobs() {
  remote_script "$(remote_hdr)
cd \"\$R/$JOBS\" 2>/dev/null || exit 0
for j in \$(ls -1 | tail -n 15); do
  if [ -f \"\$j/rc\" ]; then s=\"exit \$(cat \$j/rc)\"; else s=running; fi
  printf '%-24s %-9s %s\n' \"\$j\" \"\$s\" \"\$(tail -n 1 \$j/cmd | cut -c1-60)\"
done"
}

# ---------- misc ----------

forward_ports() {
  local p
  for p in ${PORTS:-}; do
    $SSH_CMD -O forward -L "$p:localhost:$p" "$REMOTE" 2>/dev/null
  done
}

cmd_forward() {
  [ $# -gt 0 ] || die "usage: devsync forward <port> [port...]"
  $SSH_CMD "$REMOTE" true || die "cannot connect"
  PORTS="$*"; forward_ports
  ok "forwarded: $* (localhost:<port> → $REMOTE:<port>)"
}

cmd_shell() { $SSH_CMD -t "$REMOTE" "cd $REMOTE_DIR/${REL} 2>/dev/null || cd $REMOTE_DIR; exec \$SHELL -l"; }

cmd_init() {
  local target=${1:-}
  case $target in *:*) ;; *) die "usage: devsync init user@host:~/remote/path" ;; esac
  [ -f "$CONFIG" ] && die "$CONFIG already exists here"
  cat > "$CONFIG" <<EOF
REMOTE=${target%%:*}
REMOTE_DIR='${target#*:}'
USE_GITIGNORE=0
DELETE=0
PULL_BACK=1
PORTS=""
REMOTE_PRELUDE=""
SSH_OPTS=""
RETRIES=0
EOF
  [ -f .syncignore ] || printf '# extra excludes (gitignore-style), one per line\n' > .syncignore
  if [ -f .gitignore ] && ! grep -q '^\.devsync' .gitignore; then
    printf '.devsync*\n' >> .gitignore
  fi
  ok "created $CONFIG -> $target"
  log "next: devsync push   (then optionally: devsync agent-setup)"
}

# ---------- agent integration ----------

AGENT_MD='<!-- devsync:start -->
## Remote execution (devsync)
Code is edited on this machine but runs on a remote machine (see `.devsync`).
- Shell commands that execute code (tests, builds, package installs, scripts, servers,
  ps/kill) are routed to the remote automatically: files are synced first, the command
  runs in the matching remote directory, output streams back, and files the command
  created or changed (lockfiles, generated code) are pulled back here.
- Reading, searching, editing files and git stay local.
- Force a command to run locally: end it with `# local`. Force remote: `# remote`.
- Long-running processes (dev servers, watchers, training): start with
  `devsync bg "<cmd>"`, check with `devsync logs <id>` (Ctrl-C detaches), stop with
  `devsync kill <id>`. Never block on a server in the foreground.
- Ports listed in PORTS in `.devsync` are forwarded, so `curl localhost:<port>` reaches the remote.
- The connection is flaky: if it drops, devsync reconnects and resumes output; the
  remote command keeps running. Do not retry a command just because of a reconnect message.
<!-- devsync:end -->'

cmd_agent_setup() {
  find_root || die "run devsync init first"
  cd "$PROJECT_ROOT" || exit 1
  command -v python3 >/dev/null || die "python3 needed for agent-setup"
  mkdir -p .claude
  DEVSYNC_BIN=$SELF python3 -I - <<'PY'
import json, os
p = ".claude/settings.json"
s = json.load(open(p)) if os.path.exists(p) else {}
cmd = json.dumps(os.environ["DEVSYNC_BIN"]) + " hook"
pre = s.setdefault("hooks", {}).setdefault("PreToolUse", [])
pre[:] = [e for e in pre if not any("devsync" in h.get("command", "") and h.get("command", "").endswith(" hook") for h in e.get("hooks", []))]
pre.append({"matcher": "Bash", "hooks": [{"type": "command", "command": cmd, "timeout": 10}]})
json.dump(s, open(p, "w"), indent=2); open(p, "a").write("\n")
PY
  local f
  for f in CLAUDE.md AGENTS.md; do
    [ "$f" = AGENTS.md ] && [ ! -f AGENTS.md ] && continue
    if [ -f "$f" ] && grep -q 'devsync:start' "$f"; then
      python3 -I -c 'import re,sys; p=sys.argv[1]; t=open(p).read(); open(p,"w").write(re.sub(r"<!-- devsync:start -->.*?<!-- devsync:end -->", lambda m: sys.argv[2], t, flags=re.S))' "$f" "$AGENT_MD"
    else
      { [ -s "$f" ] && echo; printf '%s\n' "$AGENT_MD"; } >> "$f"
    fi
  done
  ok "installed Claude Code hook in .claude/settings.json and instructions in CLAUDE.md"
  log "restart your agent session (or open /hooks once) so it picks up the hook"
}

# PreToolUse hook: rewrite Bash commands that execute code into `devsync exec`.
cmd_hook() {
  DEVSYNC_HOOK_INPUT=$(cat) DEVSYNC_BIN=$SELF exec python3 -I - <<'PY'
import json, os, re, shlex, sys
d = json.loads(os.environ["DEVSYNC_HOOK_INPUT"] or "{}")
if d.get("tool_name") != "Bash": sys.exit(0)
ti = d.get("tool_input") or {}
cmd = ti.get("command") or ""
p = d.get("cwd") or os.getcwd()
while not os.path.isfile(os.path.join(p, ".devsync")):
    if p == os.path.dirname(p): sys.exit(0)      # not a devsync project: leave alone
    p = os.path.dirname(p)
if re.search(r"#\s*local\s*$", cmd.strip()): sys.exit(0)
force = re.search(r"#\s*remote\s*$", cmd.strip())

LOCAL = set("""git ls cat head tail less more grep egrep fgrep rg ag find fd sed awk wc diff cmp
echo printf pwd cd pushd popd mkdir rmdir mv cp rm touch chmod chown ln tree file stat du df
jq yq sort uniq cut tr tee xargs basename dirname realpath readlink true false test [ sleep
date which type command export unset source . alias devsync open code cursor gh curl wget
ssh scp rsync tar zip unzip gzip gunzip base64 shasum md5 env clear history""".split())
WRAPPERS = {"sudo", "time", "nohup", "nice", "env", "command", "exec"}

def first_words(c):
    # drop heredoc bodies so their lines are not mistaken for commands
    c = re.sub(r"<<-?\s*['\"]?(\w+)['\"]?[^\n]*\n.*?\n\s*\1\s*(\n|$)", "\n", c, flags=re.S)
    lx = shlex.shlex(c.replace("\n", " ; "), posix=True, punctuation_chars=True)
    lx.whitespace_split = True
    words, start = [], True
    try:
        for t in lx:
            if t in ("&&", "||", ";", "|", "&", ";;", "(", ")", "|&"): start = True; continue
            if not start: continue
            if re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", t) or t in WRAPPERS: continue
            words.append(os.path.basename(t)); start = False
    except ValueError:
        return None
    return words

if not force:
    w = first_words(cmd)
    if w is not None and all(x in LOCAL for x in w): sys.exit(0)

new = dict(ti)
new["command"] = shlex.quote(os.environ["DEVSYNC_BIN"]) + " exec -- " + shlex.quote(cmd)
print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "updatedInput": new}}))
PY
}

# ---------- main ----------

sub=${1:-help}; shift 2>/dev/null || true
case $sub in
  init)        cmd_init "$@" ;;
  agent-setup) cmd_agent_setup ;;
  hook)        cmd_hook ;;
  push)        load_config; cmd_push ;;
  diff)        load_config; cmd_diff ;;
  watch)       load_config; cmd_watch ;;
  exec|run)    load_config; cmd_exec "$@" ;;
  bg)          load_config; cmd_bg "$@" ;;
  logs)        load_config; cmd_logs "$@" ;;
  jobs)        load_config; cmd_jobs ;;
  kill)        load_config; cmd_kill "$@" ;;
  pull)        load_config; cmd_pull "$@" ;;
  forward)     load_config; cmd_forward "$@" ;;
  shell)       load_config; cmd_shell ;;
  *)           sed -n '2,31p' "$0" | sed 's/^# \{0,1\}//' ;;
esac
