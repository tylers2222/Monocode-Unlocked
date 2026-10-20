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
# One fixed exemption runs without the lock (see light_job). Its code is the same as the
# exemption block of the live projects/shared/briefs/heavy-lock.sh (General 2, binding).
LOCK=${HEAVY_LOCK:-/tmp/monocode-heavy.lock}
QUEUE=${HEAVY_LOCK_QUEUE:-/tmp/monocode-heavy.queue}
WAITLOG=${HEAVY_LOCK_WAITLOG:-/tmp/monocode-heavy.waits.log}
POLL=${HEAVY_LOCK_POLL:-2}
STALE_SECONDS=10
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

# Light-job exemption (General 2, 2026-10-20/21): run WITHOUT the lock only
# `python3 -m unittest|pytest` whose every path argument is under scripts/process/
# or scripts/gate/, relative, without .., and TRACKED by git at HEAD in the cwd
# (at least one path). Exempt runs get a PATH whose first dir shims the heavy tools
# (they exit 1), and a 60 s limit (the whole process group is killed). Each exempt
# run logs one line with exempt=1. Everything else takes the lock.
tracked() {
  git ls-files --error-unmatch -- "$1" >/dev/null 2>&1 && return 0
  [ -n "$(git ls-files -- "$1" 2>/dev/null | head -1)" ]
}
light_job() {
  [ "$1" = python3 ] && [ "$2" = -m ] || return 1
  case "$3" in unittest|pytest) ;; *) return 1 ;; esac
  shift 3
  local arg path paths=0 pattern_next=0
  for arg in "$@"; do
    if [ "$pattern_next" = 1 ]; then
      pattern_next=0
      case "$arg" in */*|*..*) return 1 ;; *) continue ;; esac
    fi
    case "$arg" in
      -p|-k|--pattern) pattern_next=1; continue ;;
      -*|discover) continue ;;
    esac
    case "$arg" in *..*|/*) return 1 ;; esac
    path=${arg#./}
    case "$path" in
      scripts.process|scripts.process.*|scripts.gate|scripts.gate.*)
        path=$(printf '%s' "$path" | tr . /)
        tracked "$path" || tracked "$path.py" || return 1 ;;
      scripts/process|scripts/process/*|scripts/gate|scripts/gate/*)
        tracked "${path%%::*}" || return 1 ;;
      *) return 1 ;;
    esac
    paths=$((paths + 1))
  done
  [ "$paths" -ge 1 ]
}
run_light_job() {
  local shims tmp tool rc extra
  shims=/tmp/monocode-heavy-shims
  if [ ! -x "$shims/cargo" ]; then
    tmp=$(mktemp -d /tmp/monocode-heavy-shims.XXXXXX)
    for tool in cargo rustc npx npm pnpm node bun playwright tauri stryker; do
      printf '#!/bin/sh\necho "heavy tool inside an exempt run: use the lock ($0)" >&2\nexit 1\n' > "$tmp/$tool"
      chmod 755 "$tmp/$tool"
    done
    mv "$tmp" "$shims" 2>/dev/null || rm -rf "$tmp"
  fi
  PATH="$shims:$PATH" perl -e '
    my $pid = fork; die "fork: $!" unless defined $pid;
    if (!$pid) { setpgrp(0, 0); exec @ARGV or exit 127 }
    $SIG{ALRM} = sub { kill "TERM", -$pid; sleep 2; kill "KILL", -$pid; waitpid($pid, 0); exit 124 };
    alarm 60; waitpid($pid, 0);
    exit($? & 127 ? 128 + ($? & 127) : $? >> 8);' -- "$@"
  rc=$?
  extra=""; [ "$rc" = 124 ] && extra=" timeout=1"
  printf '%s exempt=1%s rc=%s waiter=[%s cmd=%s]\n' "$(date '+%Y-%m-%d %H:%M:%S %Z')" "$extra" "$rc" "$who" "$*" >> "$WAITLOG"
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
  light_job "$@" && run_light_job "$@"
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
