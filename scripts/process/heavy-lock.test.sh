#!/usr/bin/env bash
# Behaviour tests for heavy-lock.sh. Each case runs real callers against a temporary lock
# path, so it never touches the machine's real lock.
# Usage: bash scripts/process/heavy-lock.test.sh
set -uo pipefail
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) echo "skip: heavy-lock.sh runs on macOS and Linux only"; exit 0 ;;
esac
# Job control: background callers keep INT (a non-interactive shell ignores it in its
# background jobs) and each one gets its own process group, so cleanup can kill a tree.
set -m

lock_sh=$(cd "$(dirname "$0")" && pwd)/heavy-lock.sh
root=$(mktemp -d)
export HEAVY_LOCK=$root/lock HEAVY_LOCK_QUEUE=$root/queue HEAVY_LOCK_WAITLOG=$root/waits.log HEAVY_LOCK_POLL=0.1 HEAVY_LOCK_WHO=test
export HEAVY_LOCK_SHIMS=$root/shims
# A __pycache__ dir is untracked, so it would make a test dir count as changed and take the
# lock; the repo does not ignore __pycache__.
export PYTHONDONTWRITEBYTECODE=1
failed=0

# Kills every background job but the 60 s exempt run, which runs across the other cases.
kill_jobs() {
  local p
  for p in $(jobs -p); do
    [ "$p" = "${slow:-}" ] && continue
    kill -KILL -- "-$p" 2>/dev/null
    wait "$p" 2>/dev/null
  done
}
reset() {
  kill_jobs
  rm -rf "$HEAVY_LOCK" "$HEAVY_LOCK_QUEUE" "$HEAVY_LOCK_WAITLOG" "$root/case"
  mkdir -p "$root/case"
}
trap 'slow=; kill_jobs; rm -rf "$root"' EXIT

check() {
  local name=$1
  shift
  if "$@"; then echo "ok   $name"; else echo "FAIL $name"; failed=1; fi
}
# Polls every 0.1 s until the command succeeds; fails after $1 seconds.
within() {
  local deadline=$(( $(date +%s) + $1 ))
  shift
  until "$@"; do
    [ "$(date +%s)" -lt "$deadline" ] || return 1
    sleep 0.1
  done
}
dead() { [ -n "$1" ] && ! kill -0 "$1" 2>/dev/null; }
exists() { [ -e "$1" ]; }
tickets() { ls "$HEAVY_LOCK_QUEUE" 2>/dev/null | wc -l | tr -d ' '; }
no_tickets() { [ "$(tickets)" = 0 ]; }
has_ticket() { set -- "$HEAVY_LOCK_QUEUE"/*."$1"; [ -e "$1" ]; }
logged() { grep -q -- "$1" "$HEAVY_LOCK_WAITLOG" 2>/dev/null; }
hold() { "$lock_sh" sleep "$1" 2>/dev/null & holder=$!; within 5 exists "$HEAVY_LOCK/pid"; }
# perl resets INT to its default first: a suite started with INT ignored (a background job
# of a non-interactive shell) would pass the ignore on, and bash cannot trap an ignored signal.
mark_when_run() { perl -e '$SIG{INT} = "DEFAULT"; exec @ARGV or exit 127' "$lock_sh" touch "$1" 2>/dev/null & waiter=$!; }

# A git repo for the exemption cases: committed tests under scripts/process and scripts/gate,
# a committed test outside them (src), and in scripts/process an untracked scratch test, a
# staged-only test and a modified committed test.
exempt_dir=$root/exempt
mkdir -p "$exempt_dir/scripts/process" "$exempt_dir/scripts/gate" "$exempt_dir/src"
printf 'import unittest\nclass T(unittest.TestCase):\n    def test_ok(self):\n        pass\n' > "$exempt_dir/scripts/process/test_mark.py"
cp "$exempt_dir/scripts/process/test_mark.py" "$exempt_dir/scripts/gate/test_mark.py"
cp "$exempt_dir/scripts/process/test_mark.py" "$exempt_dir/src/test_heavy.py"
printf 'import subprocess, unittest\nclass T(unittest.TestCase):\n    def test_cargo(self):\n        subprocess.run(["cargo", "--version"], check=True)\n' > "$exempt_dir/scripts/process/test_cargo.py"
cp "$exempt_dir/scripts/process/test_mark.py" "$exempt_dir/scripts/process/test_modified.py"
cp "$exempt_dir/scripts/process/test_mark.py" "$exempt_dir/scripts/process/test..dots.py"
printf 'import os, time, unittest\nclass T(unittest.TestCase):\n    def test_slow(self):\n        open("slow.pid", "w").write(str(os.getpid()))\n        time.sleep(61)\n' > "$exempt_dir/scripts/process/test_slow.py"
git -C "$exempt_dir" init -q 2>/dev/null
git -C "$exempt_dir" add scripts src
git -C "$exempt_dir" -c user.name=t -c user.email=t@example.com commit -qm tracked
printf 'import subprocess\nsubprocess.run(["cargo", "build"])\n' > "$exempt_dir/scripts/process/test_scratch.py"
cp "$exempt_dir/scripts/process/test_scratch.py" "$exempt_dir/scripts/process/test_staged.py"
git -C "$exempt_dir" add scripts/process/test_staged.py
cat "$exempt_dir/scripts/process/test_scratch.py" >> "$exempt_dir/scripts/process/test_modified.py"
# Starts "heavy-lock.sh <form>" in the exempt repo; the form is shell words, quotes allowed.
start_in_exempt() { eval "set -- $1"; started_cmd=$*; (cd "$exempt_dir" && exec "$lock_sh" "$@") >/dev/null 2>&1 & }
exempt_line() { grep -F -- "cmd=$1]" "$HEAVY_LOCK_WAITLOG" 2>/dev/null | grep "exempt=1"; }
started_logged() { exempt_line "$started_cmd" | grep -q " rc=[0-9]"; }
# The 60 s limit case starts now, with its own log, and is checked at the end.
(cd "$exempt_dir" && HEAVY_LOCK_WAITLOG=$root/slow.log exec "$lock_sh" python3 -m unittest scripts/process/test_slow.py) >/dev/null 2>&1 &
slow=$!

# FIFO: a loop that locks again at once must not pass a waiter that queued before it. The
# waiter polls once a second, so without the queue it would rarely hit the short gap
# between the loop's release and its next lock.
reset
order=$root/case/order
( for _ in $(seq 15); do "$lock_sh" sh -c "echo L >> '$order'; sleep 3" 2>/dev/null; done ) &
within 5 exists "$order"
echo queued >> "$order"
HEAVY_LOCK_POLL=1 "$lock_sh" sh -c "echo W >> '$order'" 2>/dev/null
passed=$(sed -n '/^queued$/,/^W$/p' "$order" | grep -c '^L$')
check "fifo: the relocking loop passes the waiter at most once (passed $passed)" [ "$passed" -le 1 ]
check "fifo: the wait is logged with the holder pid" logged "cmd=sh -c echo W .* waited=.*s holder=\[pid [0-9][0-9]* .*sleep 3"

# A lock whose holder pid is dead is cleared by the next caller.
reset
sh -c 'exit 0' & ghost=$!
wait "$ghost"
mkdir "$HEAVY_LOCK" && echo "$ghost" > "$HEAVY_LOCK/pid"
mark_when_run "$root/case/ran"
check "dead holder: the waiter runs" within 3 exists "$root/case/ran"
check "dead holder: the clear is logged" logged "holder pid $ghost dead"

# A lock directory with no pid file is stale only after 10 s.
reset
mkdir "$HEAVY_LOCK"
mark_when_run "$root/case/ran"
sleep 8
check "no pid: not cleared at 8 s" [ ! -e "$root/case/ran" ]
check "no pid: cleared by 13 s" within 5 exists "$root/case/ran"
check "no pid: the clear is logged" logged "no pid file for"

# A waiter killed by INT, TERM or HUP exits and leaves no ticket.
for sig in INT TERM HUP; do
  reset
  hold 30
  mark_when_run "$root/case/ran"
  within 5 has_ticket "$waiter"
  kill "-$sig" "$waiter"
  check "killed waiter ($sig): exits" within 5 dead "$waiter"
  check "killed waiter ($sig): leaves no ticket" no_tickets
  check "killed waiter ($sig): never ran" [ ! -e "$root/case/ran" ]
  check "killed waiter ($sig): the wait is logged" logged "gave up on a signal"
done

# A holder killed by TERM stops its command and releases. A holder killed by KILL leaves its
# command running, so the lock stays held until that command ends.
for sig in TERM KILL; do
  reset
  hold 30
  command_pid=
  within 5 eval 'command_pid=$(pgrep -P "$holder" sleep)'
  mark_when_run "$root/case/ran"
  sleep 1
  check "killed holder ($sig): the waiter waits while the holder lives" [ ! -e "$root/case/ran" ]
  kill "-$sig" "$holder"
  if [ "$sig" = KILL ]; then
    sleep 2
    check "killed holder (KILL): no overlap while its command lives" [ ! -e "$root/case/ran" ]
    kill "$command_pid"
  fi
  check "killed holder ($sig): the waiter runs" within 3 exists "$root/case/ran"
  check "killed holder ($sig): its command stopped" dead "$command_pid"
done
check "killed holder (KILL): the clear names the command pid" logged "and command pid $command_pid dead"

# 4 callers x 25 runs: the critical section never has two runs inside it.
reset
crit='mkdir "$1/inside" 2>/dev/null || echo overlap >> "$1/overlaps"; echo run >> "$1/runs"; sleep 0.1; rmdir "$1/inside" 2>/dev/null'
callers=()
for _ in 1 2 3 4; do
  ( for _ in $(seq 25); do "$lock_sh" sh -c "$crit" _ "$root/case" 2>/dev/null; done ) &
  callers+=($!)
done
wait "${callers[@]}"
runs=$(wc -l < "$root/case/runs" | tr -d ' ')
overlaps=$(cat "$root/case/overlaps" 2>/dev/null | wc -l | tr -d ' ')
check "exclusion: 100 runs finished (got $runs)" [ "$runs" = 100 ]
check "exclusion: overlap counter is 0 (got $overlaps)" [ "$overlaps" = 0 ]
check "exclusion: no ticket or lock left" eval '[ "$(tickets)" = 0 ] && [ ! -e "$HEAVY_LOCK" ]'

# The holder line's instance label comes from the caller's ancestors, never from the lock's
# own command line. Run as `bash -c "$script"`, that command line holds both app names.
reset
export LOCK_SH=$lock_sh LOCK_SCRIPT
LOCK_SCRIPT=$(cat "$lock_sh")
# The fake parent's command line names the app; the lock runs as a file or as bash -c.
label_of() {
  local run='"$LOCK_SH" "$@"'
  [ "$2" = bash-c ] && run='/bin/bash -c "$LOCK_SCRIPT" heavy-lock.sh "$@"'
  env -u HEAVY_LOCK_WHO /bin/bash -c "$run; :" "/fake/$1/Contents/MacOS/monocode" \
    sh -c 'cut -d" " -f1 "$HEAVY_LOCK/holder"' 2>/dev/null
}
for app in MonoCode.app:instance-1 "MonoCode 2.app:instance-2"; do
  for form in direct bash-c; do
    label=$(label_of "${app%%:*}" "$form")
    check "instance: a caller under ${app%%:*} ($form) is labelled ${app#*:} (got $label)" [ "$label" = "${app#*:}" ]
  done
done

# Exemption: python3 -m unittest|pytest on tracked paths under scripts/process or
# scripts/gate runs while another caller holds the lock; each lookalike waits.
reset
hold 30
for form in "python3 -m unittest scripts/process/test_mark.py" \
  "python3 -m unittest discover -s scripts/gate" \
  "python3 -m unittest discover -s scripts/gate -p 'test_m*.py'" \
  "python3 -m unittest scripts.process.test_mark" \
  "python3 -m pytest -q ./scripts/process/test_mark.py"; do
  start_in_exempt "$form"
  check "exempt: committed test '$form' runs while the lock is held" within 5 dead $!
  check "exempt: '$form' is logged with its rc" started_logged
done
(cd "$exempt_dir" && exec "$lock_sh" python3 -m unittest scripts/process/test_cargo.py) > "$root/cargo.out" 2>&1
cargo_rc=$?
check "exempt: a tracked test that calls cargo fails (rc $cargo_rc)" [ "$cargo_rc" = 1 ]
check "exempt: it fails with the shim message" grep -q "heavy tool inside an exempt run: use the lock" "$root/cargo.out"
check "exempt: it is logged with rc=1" eval 'exempt_line "python3 -m unittest scripts/process/test_cargo.py" | grep -q " rc=1 "'
rm -rf "$HEAVY_LOCK_SHIMS"
(cd "$exempt_dir" && exec "$lock_sh" python3 -m unittest scripts/process/test_cargo.py) > "$root/cargo.out" 2>&1
cargo_rc=$?
check "shims: a deleted shim dir is rebuilt and cargo still fails (rc $cargo_rc)" [ "$cargo_rc" = 1 ]
check "shims: the rebuilt shim prints its message" grep -q "heavy tool inside an exempt run: use the lock" "$root/cargo.out"
check "shims: all 10 heavy tools are shimmed again" [ "$(find "$HEAVY_LOCK_SHIMS" -type f -perm -u+x ! -name '.*' | wc -l | tr -d ' ')" = 10 ]
exempt_lines=$(grep -c 'exempt=1' "$HEAVY_LOCK_WAITLOG")
lookalikes=("python3 scripts/process/test_mark.py" "python3 -m timeit scripts/process/test_mark.py" "python3 -m pytest src"
  "python3 -m pytest ../src" "python3 -m pytest scripts/process/../../src/test_heavy.py"
  "python3 -m unittest scripts/process/test_scratch.py" "python3 -m unittest discover -s scripts/gate -p 'x/test_*.py'"
  "python3 -m unittest discover -s scripts/process" "python3 -m unittest scripts/process/test_staged.py"
  "python3 -m unittest scripts/process/test_modified.py" "python3 -m unittest scripts/process/test..dots.py"
  "bash -c 'python3 -m unittest scripts/process/test_mark.py'" "cargo --version")
pids=()
for form in "${lookalikes[@]}"; do
  start_in_exempt "$form"
  pids+=($!)
done
sleep 2
for i in "${!lookalikes[@]}"; do
  check "lookalike: '${lookalikes[$i]}' waits in the queue" has_ticket "${pids[$i]}"
done
check "lookalike: each waiter holds a ticket" [ "$(tickets)" = "${#lookalikes[@]}" ]
check "lookalike: none is logged as exempt" [ "$(grep -c 'exempt=1' "$HEAVY_LOCK_WAITLOG")" = "$exempt_lines" ]
rm -f "$HEAVY_LOCK_SHIMS/npm"
chmod 555 "$HEAVY_LOCK_SHIMS"
start_in_exempt "python3 -m unittest scripts/process/test_mark.py"
unshimmed=$!
sleep 2
check "shims: with the shim dir unwritable and npm missing, a light job waits in the queue" has_ticket "$unshimmed"
chmod 755 "$HEAVY_LOCK_SHIMS"

check "exempt: a test that sleeps 61 s is stopped at the 60 s limit" within 80 dead "$slow"
check "exempt: the stop is logged" grep -q "exempt=1 timeout=1 rc=124 .*cmd=python3 -m unittest scripts/process/test_slow.py" "$root/slow.log"
check "exempt: the stopped run leaves no process" dead "$(cat "$exempt_dir/slow.pid" 2>/dev/null)"

exit "$failed"
