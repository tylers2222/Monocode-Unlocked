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
export HEAVY_LOCK=$root/lock HEAVY_LOCK_QUEUE=$root/queue HEAVY_LOCK_WAITLOG=$root/waits.log HEAVY_LOCK_POLL=0.1
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
logged() { grep -q -- "$1" "$HEAVY_LOCK_WAITLOG" 2>/dev/null; }
hold() { "$lock_sh" sleep "$1" 2>/dev/null & holder=$!; within 5 exists "$HEAVY_LOCK/pid"; }
mark_when_run() { "$lock_sh" touch "$1" 2>/dev/null & waiter=$!; }

# FIFO: a loop that locks again at once must not pass a waiter that queued before it. The
# waiter polls once a second, so without the queue it would rarely hit the short gap
# between the loop's release and its next lock.
reset
order=$root/case/order
( for _ in $(seq 30); do "$lock_sh" sh -c "echo L >> '$order'; sleep 0.5" 2>/dev/null; done ) &
within 5 exists "$order"
echo queued >> "$order"
HEAVY_LOCK_POLL=1 "$lock_sh" sh -c "echo W >> '$order'" 2>/dev/null
passed=$(sed -n '/^queued$/,/^W$/p' "$order" | grep -c '^L$')
check "fifo: the relocking loop passes the waiter at most once (passed $passed)" [ "$passed" -le 1 ]
check "fifo: the wait is logged when the waiter gets the lock" logged "cmd=sh -c echo W .* waited=.*s holder=\[.*sleep 0.5"

# A lock whose holder pid is dead is cleared by the next caller.
reset
sh -c 'exit 0' & ghost=$!
wait "$ghost"
mkdir "$HEAVY_LOCK" && echo "$ghost" > "$HEAVY_LOCK/pid"
mark_when_run "$root/case/ran"
check "dead holder: the waiter runs" within 3 exists "$root/case/ran"
check "dead holder: the clear is logged" logged "holder pid $ghost is dead"

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
  within 5 [ "$(tickets)" = 1 ]
  kill "-$sig" "$waiter"
  check "killed waiter ($sig): exits" within 3 dead "$waiter"
  check "killed waiter ($sig): leaves no ticket" no_tickets
  check "killed waiter ($sig): never ran" [ ! -e "$root/case/ran" ]
  check "killed waiter ($sig): the wait is logged" logged "gave up on a signal"
done

# A holder killed by TERM stops its command and releases; one killed by KILL is cleared.
for sig in TERM KILL; do
  reset
  hold 30
  command_pid=$(pgrep -P "$holder" sleep)
  mark_when_run "$root/case/ran"
  sleep 1
  check "killed holder ($sig): the waiter waits while the holder lives" [ ! -e "$root/case/ran" ]
  kill "-$sig" "$holder"
  check "killed holder ($sig): the waiter runs" within 3 exists "$root/case/ran"
  [ "$sig" = TERM ] && check "killed holder (TERM): its command stopped" dead "$command_pid"
  kill "$command_pid" 2>/dev/null
done

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

exit "$failed"
