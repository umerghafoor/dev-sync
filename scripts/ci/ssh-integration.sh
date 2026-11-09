#!/bin/sh
# End-to-end test over a real OpenSSH server on localhost.
#
# Used by CI on Linux runners; also runnable on any Linux/macOS machine where
# `ssh localhost` works without a password:
#
#   make build agents && scripts/ci/ssh-integration.sh target/release/devsync
set -eu

bin=$(cd "$(dirname "${1:-target/release/devsync}")" && pwd)/$(basename "${1:-target/release/devsync}")
host=${DEVSYNC_TEST_HOST:-$(id -un)@localhost}
work=$(mktemp -d)
remote_dir="devsync-ci-$$"
trap 'rm -rf "$work"; ssh -o BatchMode=yes "$host" "rm -rf ~/$remote_dir" >/dev/null 2>&1 || true' EXIT

step() { printf '\n==> %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

step "ssh works without prompts"
ssh -o BatchMode=yes "$host" true || fail "cannot ssh to $host"

cd "$work"
mkdir -p proj/src proj/node_modules/x
cd proj
printf 'SECRET=1\n' > .env
printf 'import os, sys\nprint("cwd:", os.getcwd())\nopen("generated.txt", "w").write("ok\\n")\nsys.exit(3)\n' > src/app.py
echo junk > node_modules/x/index.js

step "init + doctor (deploys the agent)"
"$bin" init "$host:~/$remote_dir"
"$bin" doctor

step "exec: cwd mapping, exit code, pull-back"
set +e
out=$(cd src && "$bin" exec -- python3 app.py)
code=$?
set -e
echo "$out"
[ "$code" -eq 3 ] || fail "expected exit code 3, got $code"
echo "$out" | grep -q "cwd: .*/$remote_dir/src" || fail "command did not run in the matching directory"
[ -f src/generated.txt ] || fail "generated file was not pulled back"

step "sync rules"
ssh -o BatchMode=yes "$host" "test -f ~/$remote_dir/.env" || fail ".env was not synced"
if ssh -o BatchMode=yes "$host" "test -e ~/$remote_dir/node_modules"; then fail "node_modules was synced"; fi

step "background job survives with no session open"
id=$("$bin" submit -- 'sleep 2; echo finished-alone')
sleep 4
"$bin" logs "$id" | grep -q finished-alone || fail "background job output missing"
grep -q finished-alone ".devsync/jobs/$id/output.log" || fail "results folder not populated"

step "kill"
id=$("$bin" submit -- 'sleep 60')
"$bin" kill "$id"
grep -q '"exit_code": 143' ".devsync/jobs/$id/status.json" || fail "job was not stopped"

step "agent deployed once, content-addressed"
n=$(ssh -o BatchMode=yes "$host" 'ls ~/.devsync/bin | wc -l')
[ "$n" -ge 1 ] || fail "agent not found in ~/.devsync/bin"

printf '\nssh integration test passed\n'
