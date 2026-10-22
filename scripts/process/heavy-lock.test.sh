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
alive() { [ -n "$1" ] && kill -0 "$1" 2>/dev/null; }
dead() { [ -n "$1" ] && ! kill -0 "$1" 2>/dev/null; }
exists() { [ -e "$1" ]; }
tickets() { ls "$HEAVY_LOCK_QUEUE" 2>/dev/null | wc -l | tr -d ' '; }
no_tickets() { [ "$(tickets)" = 0 ]; }
has_ticket() { set -- "$HEAVY_LOCK_QUEUE"/*."$1"; [ -e "$1" ]; }
logged() { grep -q -- "$1" "$HEAVY_LOCK_WAITLOG" 2>/dev/null; }
hold() { "$lock_sh" sleep "$1" 2>/dev/null & holder=$!; within 5 exists "$HEAVY_LOCK/pid"; }
# The lock refuses `sh -c`, so multi-step test commands are small scripts.
bin=$root/bin
mkdir -p "$bin"
printf '#!/bin/sh\necho "$2" >> "$1"\nsleep "${3:-0}"\n' > "$bin/append"
printf '#!/bin/sh\necho held > "$1"\nread -r _ < "$2"\n' > "$bin/fifo-hold"
printf '#!/bin/sh\nmkdir "$1/inside" 2>/dev/null || echo overlap >> "$1/overlaps"\necho run >> "$1/runs"\nsleep 0.1\nrmdir "$1/inside" 2>/dev/null\n' > "$bin/crit"
printf '#!/bin/sh\ncut -d" " -f1 "$HEAVY_LOCK/holder"\n' > "$bin/holder-label"
printf '#!/bin/sh\nsleep "$1" &\nsleep "$1"\nwait\n' > "$bin/spawn"
cat > "$bin/leaky" <<'EOF'
#!/bin/sh
perl -e '$SIG{TERM} = "IGNORE"; sleep $ARGV[0]' "$1" &
sleep "$1"
wait
EOF
cat > "$bin/stubborn" <<'EOF'
#!/bin/sh
exec perl -e '$SIG{TERM} = $SIG{HUP} = $SIG{INT} = "IGNORE"; sleep $ARGV[0]' "$1"
EOF
chmod +x "$bin"/*
# Runs a command with INT at its default, even when this suite started with INT ignored. An
# array, not a function: a function run with & is a subshell, so $! would not be the command.
int_default=(perl -e '$SIG{INT} = "DEFAULT"; exec @ARGV or exit 127')
# The pid of the command itself: the holder runs a perl layer ($LOCK/child) that runs it.
command_of() { pgrep -P "$(cat "$HEAVY_LOCK/child" 2>/dev/null)" 2>/dev/null | head -1; }
end_lines() { grep -c "end=1 .*holder=\[pid $1 " "$HEAVY_LOCK_WAITLOG" 2>/dev/null; }
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
( for _ in $(seq 15); do "$lock_sh" "$bin/append" "$order" L 3 2>/dev/null; done ) &
within 5 exists "$order"
echo queued >> "$order"
HEAVY_LOCK_POLL=1 "$lock_sh" "$bin/append" "$order" W 2>/dev/null
passed=$(sed -n '/^queued$/,/^W$/p' "$order" | grep -c '^L$')
check "fifo: the relocking loop passes the waiter at most once (passed $passed)" [ "$passed" -le 1 ]

# The wait line names the holder's pid. No sleeps: the holder blocks on a FIFO, and the
# test releases it only after the waiter has printed its first "waiting" line. 20 runs.
# The FIFOs are opened read-write, which never blocks, and each read gives up after 10 s,
# so a broken lock fails these checks instead of hanging the suite.
reset
mkfifo "$root/case/ready" "$root/case/gate" "$root/case/wait"
logged_runs=0
for i in $(seq 20); do
  "$lock_sh" "$bin/fifo-hold" "$root/case/ready" "$root/case/gate" 2>/dev/null &
  holder=$!
  exec 6<> "$root/case/ready"
  read -t 10 -r _ <&6
  exec 6<&-
  "$lock_sh" true "run$i" 2> "$root/case/wait" &
  waiter=$!
  exec 7<> "$root/case/wait"
  read -t 10 -r _ <&7
  echo go > "$root/case/gate"
  wait "$waiter" "$holder"
  exec 7<&-
  logged "cmd=true run$i\] waited=[0-9]*s holder=\[pid $holder " && logged_runs=$((logged_runs + 1))
done
check "fifo: the wait is logged with the holder pid ($logged_runs of 20 runs)" [ "$logged_runs" = 20 ]

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
  within 5 eval 'command_pid=$(command_of)'
  child_pid=$(cat "$HEAVY_LOCK/child")
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
check "killed holder (KILL): the clear names the command pid" logged "and command pid $child_pid dead"

# TERM to the holder's pid only (not its group): the command and its children stop (one of
# them ignores TERM, so only the holder's group KILL sweep stops it), exactly one end line
# says signal=1, and the waiter runs.
reset
"$lock_sh" "$bin/leaky" 43.$$ 2>/dev/null &
holder=$!
within 5 eval '[ "$(pgrep -f "(sleep|IGNORE.*) 43[.]$$\$" | wc -l | tr -d " ")" = 2 ]'
mark_when_run "$root/case/ran"
within 5 has_ticket "$waiter"
kill -TERM "$holder"
check "TERM to the holder only: the holder exits" within 8 dead "$holder"
check "TERM to the holder only: no process of the command is left, also one that ignores TERM" eval '! pgrep -f "(sleep|IGNORE.*) 43[.]$$\$" >/dev/null'
check "TERM to the holder only: exactly one end line (got $(end_lines "$holder"))" [ "$(end_lines "$holder")" = 1 ]
check "TERM to the holder only: the end line says rc=143 signal=1" logged "end=1 rc=143 signal=1 holder=\[pid $holder test session=.* cwd=.* cmd=$bin/leaky 43.$$\]"
check "TERM to the holder only: the waiter runs" within 5 exists "$root/case/ran"
within 5 dead "$waiter"
check "TERM to the holder only: the waiter's end line says signal=0" logged "end=1 rc=0 signal=0 holder=\[pid $waiter "

# A second signal of any kind during the 5 s grace (the command ignores TERM, HUP and INT) is
# ignored: the holder waits out the grace (still alive 3 s after the second signal, 4 s into
# the grace), then writes one end line for the first signal, releases the lock itself (no
# stale clear), the waiter runs, and nothing is left.
for pair in TERM:TERM TERM:HUP TERM:INT HUP:HUP INT:INT; do
  sig1=${pair%:*} sig2=${pair#*:}
  case "$sig1" in TERM) rc1=143 ;; HUP) rc1=129 ;; INT) rc1=130 ;; esac
  reset
  "${int_default[@]}" "$lock_sh" "$bin/stubborn" 45.$$ 2>/dev/null &
  holder=$!
  within 5 eval 'pgrep -f "IGNORE.* 45[.]$$\$" >/dev/null'
  mark_when_run "$root/case/ran"
  within 5 has_ticket "$waiter"
  kill "-$sig1" "$holder"
  sleep 1
  kill "-$sig2" "$holder"
  sleep 3
  check "$sig1 then $sig2 in the grace: the holder waits out the grace" alive "$holder"
  check "$sig1 then $sig2 in the grace: the holder exits after it" within 5 dead "$holder"
  check "$sig1 then $sig2 in the grace: exactly one end line (got $(end_lines "$holder"))" [ "$(end_lines "$holder")" = 1 ]
  check "$sig1 then $sig2 in the grace: the end line says rc=$rc1 signal=1" logged "end=1 rc=$rc1 signal=1 holder=\[pid $holder "
  check "$sig1 then $sig2 in the grace: the holder released the lock (no stale clear)" eval '! logged "cleared a stale lock"'
  check "$sig1 then $sig2 in the grace: the waiter runs" within 5 exists "$root/case/ran"
  check "$sig1 then $sig2 in the grace: no process of the command is left" eval '! pgrep -f "IGNORE.* 45[.]$$\$" >/dev/null'
done

# INT to the holder reaches the command: the command starts with INT at its default, so it
# stops at once, not by KILL after the 5 s grace.
reset
"${int_default[@]}" "$lock_sh" "$bin/spawn" 46.$$ 2>/dev/null &
holder=$!
within 5 eval '[ "$(pgrep -f "sleep 46[.]$$\$" | wc -l | tr -d " ")" = 2 ]'
kill -INT "$holder"
check "INT to the holder: the command stops before the 5 s grace ends" within 3 dead "$holder"
check "INT to the holder: one end line rc=130 signal=1" eval '[ "$(end_lines "$holder")" = 1 ] && logged "end=1 rc=130 signal=1 holder=\[pid $holder "'
check "INT to the holder: no process of the command is left" within 3 eval '! pgrep -f "sleep 46[.]$$\$" >/dev/null'

# The end line comes before the release: with the log a FIFO that nobody reads yet, the
# holder blocks in its end-line write and must still hold the lock at that moment.
reset
mkfifo "$root/case/log"
HEAVY_LOCK_WAITLOG=$root/case/log "$lock_sh" true 2>/dev/null &
holder=$!
child_pid=
within 5 eval 'child_pid=$(cat "$HEAVY_LOCK/child" 2>/dev/null)'
within 5 dead "$child_pid"
sleep 1
check "end before release: the lock is still held while the end line is written" eval '[ "$(cat "$HEAVY_LOCK/pid" 2>/dev/null)" = "$holder" ]'
exec 8<> "$root/case/log"
read -t 10 -r end_line <&8
exec 8<&-
check "end before release: the end line is the holder's" grep -q "end=1 rc=0 signal=0 holder=\[pid $holder " <<< "$end_line"
check "end before release: then the lock is released" within 5 eval '[ ! -e "$HEAVY_LOCK" ]'

# 4 callers x 25 runs: the critical section never has two runs inside it.
reset
callers=()
for _ in 1 2 3 4; do
  ( for _ in $(seq 25); do "$lock_sh" "$bin/crit" "$root/case" 2>/dev/null; done ) &
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
    "$bin/holder-label" 2>/dev/null
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
  "cargo --version")
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

# One command per hold: a shell wrapper or a command word with && ; | is refused with rc 2
# at once, even while another caller holds the lock, and takes no ticket.
reset
hold 30
refused=("bash -c true" "sh -lc true" "env bash -ec true" "'cargo build&&cargo test'"
  "bash -c 'python3 -m unittest scripts/process/test_mark.py'")
for form in "${refused[@]}"; do
  eval "set -- $form"
  "$lock_sh" "$@" 2>/dev/null
  rc=$?
  check "refused: '$form' exits rc=2 at once (got $rc)" [ "$rc" = 2 ]
  check "refused: '$form' is logged" logged "refused=1 reason=shell-wrapper .*cmd=$*\]"
done
check "refused: no refused command took a ticket" no_tickets
kill "$holder"
wait "$holder"
printf 'touch "$1"\n' > "$root/case/step.sh"
"$lock_sh" bash "$root/case/step.sh" "$root/case/script-ran" 2>/dev/null
check "allowed: 'bash script.sh' runs under the lock" exists "$root/case/script-ran"

# Hold limit (2 s here; HEAVY_LOCK_HOLD_LIMIT is for tests only): the command's group is
# killed with rc 124, the kill is logged timeout=1, and the next waiter runs. --long doubles it.
reset
(HEAVY_LOCK_HOLD_LIMIT=2 exec "$lock_sh" "$bin/spawn" 44.$$ 2>/dev/null) &
holder=$!
within 5 exists "$HEAVY_LOCK/pid"
mark_when_run "$root/case/ran"
wait "$holder"
rc=$?
check "hold limit: the holder exits rc=124 (got $rc)" [ "$rc" = 124 ]
check "hold limit: the kill is logged timeout=1" logged "timeout=1 limit=2s holder=\[pid $holder .*cmd=$bin/spawn 44.$$\]"
check "hold limit: one end line, rc=124 signal=0" eval '[ "$(end_lines "$holder")" = 1 ] && logged "end=1 rc=124 signal=0 holder=\[pid $holder "'
check "hold limit: no process of the command is left" eval '! pgrep -f "sleep 44[.]$$\$" >/dev/null'
check "hold limit: the next waiter runs" within 5 exists "$root/case/ran"
HEAVY_LOCK_HOLD_LIMIT=2 "$lock_sh" --long "$bin/spawn" 3 2>/dev/null
rc=$?
check "--long: a 3 s command under a 2 s limit finishes with --long (rc $rc)" [ "$rc" = 0 ]
check "--long: the hold is logged long=1" logged "long=1 holder=\[pid [0-9]* .*cmd=$bin/spawn 3\]"

check "exempt: a test that sleeps 61 s is stopped at the 60 s limit" within 80 dead "$slow"
check "exempt: the stop is logged" grep -q "exempt=1 timeout=1 rc=124 .*cmd=python3 -m unittest scripts/process/test_slow.py" "$root/slow.log"
check "exempt: the stopped run leaves no process" dead "$(cat "$exempt_dir/slow.pid" 2>/dev/null)"

exit "$failed"
