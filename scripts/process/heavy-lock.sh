#!/bin/bash
# Run ONE heavy command at a time on this machine (cargo/tauri builds, cargo test, full
# vitest, Playwright, one mutation run), across every session of every MonoCode instance.
# Usage: heavy-lock.sh <command...>. Hold the lock for one command, never a whole batch.
#
# Lock: the directory $HEAVY_LOCK, made with mkdir (atomic). The holder writes its pid, its
# command's pid and a holder line into it. Queue: one empty file per waiter in
# $HEAVY_LOCK_QUEUE, named <arrival time>.<pid>, so sorted names are arrival order. Only
# the oldest live ticket (the head) may break a stale lock or take the lock. So a caller
# that locks again right after it releases goes to the back of the line, and, among callers
# of this script, no caller can take the lock between the head's check of a dead holder and
# its removal of that lock. These paths and names are the same as in the script this one
# replaces, so old and new callers exclude each other.
#
# A lock is removed only when its holder pid and its command pid are both dead, a ticket only
# when its pid is dead; a lock directory with no pid file counts as stale once it is 10 s
# old. Every wait writes one line to $HEAVY_LOCK_WAITLOG.
#
# One fixed exemption runs without the lock (see exempt): light python3 -m unittest|pytest
# runs on tracked files under scripts/process/ or scripts/gate/.
LOCK=${HEAVY_LOCK:-/tmp/monocode-heavy.lock}
QUEUE=${HEAVY_LOCK_QUEUE:-/tmp/monocode-heavy.queue}
WAITLOG=${HEAVY_LOCK_WAITLOG:-/tmp/monocode-heavy.waits.log}
POLL=${HEAVY_LOCK_POLL:-2}
STALE_SECONDS=10
EXEMPT_SECONDS=${HEAVY_LOCK_EXEMPT_SECONDS:-60}
SHIMS=${LOCK%.lock}-shims
export CARGO_BUILD_JOBS=4

instance() {
  local p=$$ cmd
  for _ in $(seq 1 40); do
    cmd=$(ps -o command= -p "$p" 2>/dev/null) || break
    case "$cmd" in
      *"MonoCode 2.app"*) echo "instance-2"; return ;;
      *"MonoCode.app"*) echo "instance-1"; return ;;
    esac
    p=$(ps -o ppid= -p "$p" 2>/dev/null | tr -d ' ')
    { [ -z "$p" ] || [ "$p" -le 1 ]; } && break
  done
  echo "unknown"
}

log() { printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S %Z')" "$*" >> "$WAITLOG"; }

tracked() { git ls-files --error-unmatch -- "$1" >/dev/null 2>&1; }

# A path for the exemption: relative, scripts/process or scripts/gate or under them (as a
# path, a pytest node id or a dotted module), and tracked by git in the cwd.
light_path() {
  local p=${1#./}
  case "$p" in
    scripts.process | scripts.process.* | scripts.gate | scripts.gate.*)
      p=$(printf '%s' "$p" | tr . /)
      tracked "$p" || tracked "$p.py" ;;
    scripts/process | scripts/process/* | scripts/gate | scripts/gate/*) tracked "${p%%::*}" ;;
    *) return 1 ;;
  esac
}

# True only for python3 -m unittest|pytest with at least one path, every path a light_path
# and no argument containing "..". Flags and "discover" are skipped. The value of -p, -k or
# --pattern must not contain "/". An attached start or top dir (-sDIR, -tDIR,
# --start-directory=DIR, --top-level-directory=DIR) counts as a path.
exempt() {
  [ "$1" = python3 ] && [ "$2" = -m ] || return 1
  case "$3" in unittest | pytest) ;; *) return 1 ;; esac
  shift 3
  local arg value kind next='' paths=0
  for arg in "$@"; do
    case "$arg" in *..*) return 1 ;; esac
    kind=path value=$arg
    if [ -n "$next" ]; then
      next=''
      kind=pattern
    else
      case "$arg" in
        -p | -k | --pattern) next=1; continue ;;
        -[pk]?* | --pattern=*) kind=pattern value=${arg#-?} value=${value#*=} ;;
        -[st]?*) value=${arg#-?} ;;
        --start-directory=* | --top-level-directory=*) value=${arg#*=} ;;
        -* | discover) continue ;;
      esac
    fi
    if [ "$kind" = pattern ]; then
      case "$value" in */*) return 1 ;; esac
    else
      light_path "$value" || return 1
      paths=$((paths + 1))
    fi
  done
  [ "$paths" -gt 0 ]
}

# An exempt run: heavy tools on PATH are shims that fail, and the whole process group is
# killed after $EXEMPT_SECONDS. One line with exempt=1 goes to the wait log.
run_exempt() {
  local tmp tool rc
  if [ ! -x "$SHIMS/cargo" ]; then
    tmp=$(mktemp -d "$SHIMS.XXXXXX")
    for tool in cargo rustc npx npm pnpm node bun playwright tauri stryker; do
      printf '#!/bin/sh\necho "heavy tool inside an exempt run: use the lock ($0)" >&2\nexit 1\n' > "$tmp/$tool"
      chmod 755 "$tmp/$tool"
    done
    mv "$tmp" "$SHIMS" 2>/dev/null || rm -rf "$tmp"
  fi
  PATH="$SHIMS:$PATH" perl -e '
    my $limit = shift;
    my $pid = fork // die "fork: $!";
    if (!$pid) { setpgrp(0, 0); exec @ARGV or exit 127 }
    $SIG{ALRM} = sub { kill "TERM", -$pid; sleep 2; kill "KILL", -$pid; waitpid($pid, 0); exit 124 };
    alarm $limit;
    waitpid($pid, 0);
    exit($? & 127 ? 128 + ($? & 127) : $? >> 8);' "$EXEMPT_SECONDS" "$@"
  rc=$?
  log "exempt=1$([ "$rc" = 124 ] && echo ' timeout=1') rc=$rc waiter=[$who cmd=$*]"
  exit "$rc"
}

first_live_ticket() {
  local t
  for t in $(ls "$QUEUE" 2>/dev/null | sort); do
    if kill -0 "${t##*.}" 2>/dev/null; then echo "$QUEUE/$t"; return; fi
    rm -f "$QUEUE/$t"
  done
}

clear_stale_lock() {
  local pid cpid reason age stale=$LOCK.stale.$$
  pid=$(cat "$LOCK/pid" 2>/dev/null)
  if [ -n "$pid" ]; then
    cpid=$(cat "$LOCK/child" 2>/dev/null)
    kill -0 "$pid" 2>/dev/null && return
    [ -n "$cpid" ] && kill -0 "$cpid" 2>/dev/null && return
    reason="holder pid $pid${cpid:+ and command pid $cpid} dead"
  else
    age=$(perl -e '@s = stat shift or exit 1; print time - $s[9]' "$LOCK" 2>/dev/null) || return
    [ "$age" -ge "$STALE_SECONDS" ] || return
    reason="no pid file for ${age}s"
  fi
  mv "$LOCK" "$stale" 2>/dev/null || return
  log "heavy-lock: cleared a stale lock ($reason; holder [$(cat "$stale/holder" 2>/dev/null)]) for [$who]"
  rm -rf "$stale"
}

release() {
  [ "$(cat "$LOCK/pid" 2>/dev/null)" = "$$" ] || return
  mv "$LOCK" "$LOCK.done.$$" && rm -rf "$LOCK.done.$$"
}

on_signal() {
  if [ -n "$child" ]; then
    kill -TERM "$child" 2>/dev/null
    wait "$child"
  else
    log "waiter=[$who cmd=$cmd] waited=$(( $(date +%s) - started ))s gave up on a signal; holder=[${holder:-pid ? none}]"
  fi
  exit "$1"
}

main() {
  who="${HEAVY_LOCK_WHO:-$(instance)} session=${MONOCODE_SESSION_ID:-${PWD##*/}} cwd=$PWD"
  exempt "$@" && run_exempt "$@"
  cmd="$*"
  child=
  holder=
  started=$(date +%s)
  mkdir -p "$QUEUE"
  ticket="$QUEUE/$(perl -MTime::HiRes=time -e 'printf "%017.6f", time').$$"
  trap 'rm -f "$ticket"; release' EXIT
  trap 'on_signal 130' INT
  trap 'on_signal 143' TERM
  trap 'on_signal 129' HUP

  local waited report=0 waiting='' ahead hpid
  while :; do
    [ -e "$ticket" ] || : > "$ticket"
    if [ "$(first_live_ticket)" = "$ticket" ]; then
      clear_stale_lock
      mkdir "$LOCK" 2>/dev/null && break
    fi
    waiting=1
    hpid=$(cat "$LOCK/pid" 2>/dev/null) && holder="pid $hpid $(cat "$LOCK/holder" 2>/dev/null)"
    waited=$(( $(date +%s) - started ))
    if [ "$waited" -ge "$report" ]; then
      ahead=$(ls "$QUEUE" | sort | grep -n "^${ticket##*/}\$" | cut -d: -f1)
      echo "heavy-lock: waiting ${waited}s, $(( ${ahead:-1} - 1 )) ahead in line; holder: ${holder:-none}" >&2
      report=$(( waited + 20 ))
    fi
    sleep "$POLL"
  done
  rm -f "$ticket"
  echo $$ > "$LOCK/pid"
  echo "$who cmd=$cmd since=$(date '+%H:%M:%S')" > "$LOCK/holder"
  [ -n "$waiting" ] && log "waiter=[$who cmd=$cmd] waited=$(( $(date +%s) - started ))s holder=[${holder:-pid ? none}]"

  "$@" <&0 &
  child=$!
  echo "$child" > "$LOCK/child"
  wait "$child"
}

# Bash reads a script as it runs. main is parsed whole before it starts, so an edit to
# this file cannot change a running caller.
main "$@"; exit
