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
failed=0

kill_jobs() {
  local p
  for p in $(jobs -p); do kill -KILL -- "-$p" 2>/dev/null; done
  wait 2>/dev/null
}
reset() {
  kill_jobs
  rm -rf "$HEAVY_LOCK" "$HEAVY_LOCK_QUEUE" "$HEAVY_LOCK_WAITLOG" "$root/case"
  mkdir -p "$root/case"
}
trap 'kill_jobs; rm -rf "$root"' EXIT

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

# FIFO: a loop that locks again at once must not pass a waiter that queued before it. The
# waiter polls once a second, so without the queue it would rarely hit the short gap
# between the loop's release and its next lock.
reset
order=$root/case/order
( for _ in $(seq 15); do "$lock_sh" sh -c "echo L >> '$order'; sleep 1" 2>/dev/null; done ) &
within 5 exists "$order"
echo queued >> "$order"
HEAVY_LOCK_POLL=1 "$lock_sh" sh -c "echo W >> '$order'" 2>/dev/null
passed=$(sed -n '/^queued$/,/^W$/p' "$order" | grep -c '^L$')
check "fifo: the relocking loop passes the waiter at most once (passed $passed)" [ "$passed" -le 1 ]
check "fifo: the wait is logged with the holder pid" logged "cmd=sh -c echo W .* waited=.*s holder=\[pid [0-9?]"

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
for _ in 1 2 3 4; do
  ( for _ in $(seq 25); do "$lock_sh" sh -c "$crit" _ "$root/case" 2>/dev/null; done ) &
done
wait
runs=$(wc -l < "$root/case/runs" | tr -d ' ')
overlaps=$(cat "$root/case/overlaps" 2>/dev/null | wc -l | tr -d ' ')
check "exclusion: 100 runs finished (got $runs)" [ "$runs" = 100 ]
check "exclusion: overlap counter is 0 (got $overlaps)" [ "$overlaps" = 0 ]
check "exclusion: no ticket or lock left" eval '[ "$(tickets)" = 0 ] && [ ! -e "$HEAVY_LOCK" ]'

# Exemption: python3 -m unittest|pytest on tracked paths under scripts/process or
# scripts/gate runs while another caller holds the lock; each lookalike waits.
reset
hold 30
mkdir -p "$root/case/scripts/process" "$root/case/scripts/gate" "$root/case/src"
printf 'import unittest\nclass T(unittest.TestCase):\n    def test_ok(self):\n        pass\n' > "$root/case/scripts/process/test_mark.py"
cp "$root/case/scripts/process/test_mark.py" "$root/case/scripts/gate/test_mark.py"
printf 'import subprocess, unittest\nclass T(unittest.TestCase):\n    def test_cargo_is_shimmed(self):\n        r = subprocess.run(["cargo", "--version"], capture_output=True, text=True)\n        self.assertEqual(r.returncode, 1)\n        self.assertIn("exempt run", r.stderr)\n' > "$root/case/scripts/process/test_shim.py"
printf 'import os, time, unittest\nclass T(unittest.TestCase):\n    def test_slow(self):\n        open("slow.pid", "w").write(str(os.getpid()))\n        time.sleep(30)\n' > "$root/case/scripts/process/test_slow.py"
git -C "$root/case" init -q 2>/dev/null
cp "$root/case/scripts/process/test_mark.py" "$root/case/src/test_heavy.py"
git -C "$root/case" add scripts src
git -C "$root/case" -c user.name=t -c user.email=t@example.com commit -qm tracked
printf 'import unittest\n' > "$root/case/scripts/process/test_scratch.py"
# Starts "heavy-lock.sh <form>" in the case dir; the form is shell words, quotes allowed.
start_in_case() { eval "set -- $1"; started_cmd=$*; (cd "$root/case" && exec "$lock_sh" "$@") >/dev/null 2>&1 & }
exempt_line() { grep -F -- "cmd=$1]" "$HEAVY_LOCK_WAITLOG" 2>/dev/null | grep "exempt=1"; }
for form in "python3 -m unittest scripts/process/test_mark.py" \
  "python3 -m unittest discover -s scripts/gate" \
  "python3 -m unittest discover -s scripts/process -p 'test_m*.py'" \
  "python3 -m unittest scripts.process.test_mark" \
  "python3 -m pytest -q ./scripts/process/test_mark.py"; do
  start_in_case "$form"
  check "exempt: '$form' runs while the lock is held" within 5 dead $!
  check "exempt: '$form' is logged" exempt_line "$started_cmd"
done
start_in_case "python3 -m unittest scripts/process/test_shim.py"
within 5 dead $!
check "exempt: heavy tools are shims that fail" eval 'exempt_line "$started_cmd" | grep -q " rc=0 "'
export HEAVY_LOCK_EXEMPT_SECONDS=1
start_in_case "python3 -m unittest scripts/process/test_slow.py"
check "exempt: a run past the limit is stopped" within 6 dead $!
unset HEAVY_LOCK_EXEMPT_SECONDS
check "exempt: the stop is logged" eval 'exempt_line "$started_cmd" | grep -q "timeout=1 rc=124"'
check "exempt: the stopped run leaves no process" dead "$(cat "$root/case/slow.pid" 2>/dev/null)"
exempt_lines=$(grep -c 'exempt=1' "$HEAVY_LOCK_WAITLOG")
lookalikes=("python3 scripts/process/test_mark.py" "python3 -m timeit scripts/process/test_mark.py" "python3 -m pytest src"
  "python3 -m pytest ../src" "python3 -m pytest scripts/process/../../src/test_heavy.py"
  "python3 -m unittest discover -s../src scripts/process" "python3 -m unittest discover -ssrc scripts/process"
  "python3 -m unittest scripts/process/test_scratch.py" "python3 -m unittest discover -s scripts/process -p 'x/test_*.py'"
  "bash -c 'python3 -m unittest scripts/process/test_mark.py'" "cargo --version")
pids=()
for form in "${lookalikes[@]}"; do
  start_in_case "$form"
  pids+=($!)
done
sleep 2
for i in "${!lookalikes[@]}"; do
  check "lookalike: '${lookalikes[$i]}' waits in the queue" has_ticket "${pids[$i]}"
done
check "lookalike: each waiter holds a ticket" [ "$(tickets)" = "${#lookalikes[@]}" ]
check "lookalike: none is logged as exempt" [ "$(grep -c 'exempt=1' "$HEAVY_LOCK_WAITLOG")" = "$exempt_lines" ]

exit "$failed"
