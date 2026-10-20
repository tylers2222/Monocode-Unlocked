#!/bin/bash
# Run ONE heavy command at a time on this machine (cargo/tauri builds, cargo test, full
# vitest, Playwright, one mutation run), across every session of every MonoCode instance.
# Usage: heavy-lock.sh <command...>. Hold the lock for one command, never a whole batch.
#
# Lock: the directory $HEAVY_LOCK, made with mkdir (atomic). The holder writes its pid
# and a holder line into it. Queue: one empty file per waiter in $HEAVY_LOCK_QUEUE, named
# <arrival time>.<pid>, so sorted names are arrival order. Only the oldest live ticket
# (the head) may break a stale lock or take the lock. So a caller that locks again right
# after it releases goes to the back of the line, and no caller can take the lock between
# the head's check of a dead holder and its removal of that lock. These paths and names
# are the same as in the script this one replaces, so old and new callers exclude each other.
#
# A ticket or lock is removed only when its pid is dead; a lock directory with no pid file
# counts as stale once it is 10 s old. Every wait writes one line to $HEAVY_LOCK_WAITLOG.
#
# One fixed exemption runs without the lock (see exempt): python3 -m unittest|pytest
# where every path argument is under scripts/process/ or scripts/gate/.
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

# True only for python3 -m unittest|pytest with at least one path argument and every path
# argument relative, without "..", and under scripts/process or scripts/gate. Every
# argument that is not a bare flag counts as a path: an option value (--opt=value, the
# token after --opt or -s, or the rest of -sVALUE) too. So any doubt keeps the lock.
exempt() {
  [ "$1" = python3 ] && [ "$2" = -m ] || return 1
  case "$3" in unittest | pytest) ;; *) return 1 ;; esac
  local module=$3 arg path paths=0
  shift 3
  [ "$module ${1-}" = "unittest discover" ] && shift
  for arg in "$@"; do
    case "$arg" in
      --*=*) path=${arg#*=} ;;
      --* | -?) continue ;;
      -*) path=${arg#-?} ;;
      *) path=$arg ;;
    esac
    path=${path#./}
    case "/$path/" in */../*) return 1 ;; esac
    case "$path" in
      scripts/process | scripts/process/* | scripts/gate | scripts/gate/*) paths=$((paths + 1)) ;;
      *) return 1 ;;
    esac
  done
  [ "$paths" -gt 0 ]
}

first_live_ticket() {
  local t
  for t in $(ls "$QUEUE" 2>/dev/null | sort); do
    if kill -0 "${t##*.}" 2>/dev/null; then echo "$QUEUE/$t"; return; fi
    rm -f "$QUEUE/$t"
  done
}

clear_stale_lock() {
  local pid reason age stale=$LOCK.stale.$$
  pid=$(cat "$LOCK/pid" 2>/dev/null)
  if [ -n "$pid" ]; then
    kill -0 "$pid" 2>/dev/null && return
    reason="holder pid $pid is dead"
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
    log "waiter=[$who cmd=$cmd] waited=$(( $(date +%s) - started ))s gave up on a signal; holder=[${holder:-none}]"
  fi
  exit "$1"
}

main() {
  if exempt "$@"; then
    log "exempt=1 cwd=$PWD cmd=$*"
    exec "$@"
  fi
  who="${HEAVY_LOCK_WHO:-$(instance)} session=${MONOCODE_SESSION_ID:-${PWD##*/}} cwd=$PWD"
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

  local waited report=0 waiting='' ahead
  while :; do
    [ -e "$ticket" ] || : > "$ticket"
    if [ "$(first_live_ticket)" = "$ticket" ]; then
      clear_stale_lock
      mkdir "$LOCK" 2>/dev/null && break
    fi
    waiting=1
    holder=$(cat "$LOCK/holder" 2>/dev/null)
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
  [ -n "$waiting" ] && log "waiter=[$who cmd=$cmd] waited=$(( $(date +%s) - started ))s holder=[${holder:-none}]"

  "$@" <&0 &
  child=$!
  wait "$child"
}

# Bash reads a script as it runs. main is parsed whole before it starts, so an edit to
# this file cannot change a running caller.
main "$@"; exit
