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

# The walk starts at the caller, not at this shell: run as `bash -c "$script"`, this
# shell's own command line is the script text, which contains both app names below.
instance() {
  local p=$PPID cmd
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

# --long (General 2, 2026-10-21): a single command that really needs more than 20 min
# may ask for 40 min; it is logged with long=1.
HOLD_LIMIT=1200; long=0
if [ "${1:-}" = --long ]; then HOLD_LIMIT=2400; long=1; shift; fi
# One command per hold (General 2, 2026-10-21): refuse a shell wrapper (bash/sh/zsh/dash
# -c, also after env/nohup/time/nice/command/exec and VAR=value) and a command word that
# holds && ; or |. A script that runs many steps cannot be told apart here; the 20 min
# hold limit below bounds it.
shell_wrapper() {
  local a shell=0
  for a in "$@"; do
    if [ "$shell" = 1 ]; then
      case "$a" in -c|-[!-]*c*) return 0 ;; -*) continue ;; *) return 1 ;; esac
    fi
    case "$a" in
      *'&&'*|*';'*|*'|'*) return 0 ;;
      env|nohup|time|nice|command|exec|*=*) continue ;;
      bash|sh|zsh|dash|*/bash|*/sh|*/zsh|*/dash) shell=1 ;;
      *) return 1 ;;
    esac
  done
  return 1
}
if shell_wrapper "$@"; then
  echo "heavy-lock: one command per hold: wrap each command in heavy-lock.sh (refused: $*)" >&2
  printf '%s refused=1 reason=shell-wrapper waiter=[%s cmd=%s]\n' "$(date '+%Y-%m-%d %H:%M:%S %Z')" "${HEAVY_LOCK_WHO:-$(instance)} session=${MONOCODE_SESSION_ID:-${PWD##*/}} cwd=$PWD" "$*" >> "$WAITLOG"
  exit 2
fi
# Not in the live block; for heavy-lock.test.sh only: a hold limit in seconds (--long doubles it).
if [ -n "${HEAVY_LOCK_HOLD_LIMIT:-}" ]; then HOLD_LIMIT=$((HEAVY_LOCK_HOLD_LIMIT * (long + 1))); fi

# Light-job exemption (General 2, 2026-10-20/21): run WITHOUT the lock only
# `python3 -m unittest|pytest` whose every path argument is under scripts/process/
# or scripts/gate/, relative, without .., and TRACKED by git at HEAD in the cwd
# (at least one path). Exempt runs get a PATH whose first dir shims the heavy tools
# (they exit 1), and a 60 s limit (the whole process group is killed). Each exempt
# run logs one line with exempt=1. Everything else takes the lock below.
tracked() {
  # Exempt only what is COMMITTED at HEAD: a file in HEAD's tree, or a directory
  # whose working copy has no untracked, staged or modified file (so discover -s
  # cannot pick up a scratch test).
  local p=${1%/}
  [ "$(git cat-file -t "HEAD:$p" 2>/dev/null)" = blob ] && [ -z "$(git status --porcelain -- "$p" 2>/dev/null)" ] && return 0
  [ "$(git cat-file -t "HEAD:$p" 2>/dev/null)" = tree ] || return 1
  [ -z "$(git status --porcelain --untracked-files=all -- "$p" 2>/dev/null)" ]
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
HEAVY_TOOLS="cargo rustc npx npm pnpm node bun playwright tauri stryker"
SHIMS=/tmp/monocode-heavy-shims
# Fail closed: (re)build any missing shim, then check all of them. If any is not
# there and executable, the run is not exempt and takes the lock below.
shims_ready() {
  local tool tmp
  mkdir -p "$SHIMS" 2>/dev/null || return 1
  for tool in $HEAVY_TOOLS; do
    [ -x "$SHIMS/$tool" ] && continue
    tmp=$(mktemp "$SHIMS/.$tool.XXXXXX") || return 1
    printf '#!/bin/sh\necho "heavy tool inside an exempt run: use the lock ($0)" >&2\nexit 1\n' > "$tmp" && chmod 755 "$tmp" && mv -f "$tmp" "$SHIMS/$tool" || { rm -f "$tmp"; return 1; }
  done
  for tool in $HEAVY_TOOLS; do [ -x "$SHIMS/$tool" ] || return 1; done
}
# Not in the live block; kept only for heavy-lock.test.sh. Its shims cases delete the shim
# dir, remove a shim and make the dir read-only, which must not touch the live dir that
# running exempt jobs use. Unset, this line changes nothing.
SHIMS=${HEAVY_LOCK_SHIMS:-$SHIMS}
run_light_job() {
  local shims rc extra
  shims=$SHIMS
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

# A holder that gets INT, TERM or HUP passes the same signal to its command (the perl layer
# passes it on to the command's process group and kills the group after 5 s), waits for it,
# then sweeps the group with KILL and exits, so the EXIT handler writes the end line. The
# handler ignores INT, TERM and HUP from its first line on: under bash 3.2 an exit from a
# nested handler skips the EXIT trap. So a second signal of any kind waits out the 5 s grace,
# and there is always one end line and the release. A waiter logs its give-up and exits.
on_signal() {
  trap '' INT TERM HUP
  local pg
  if [ -n "$child" ]; then
    signal=1
    pg=$(pgrep -P "$child" | head -1)
    kill -"$2" "$child" 2>/dev/null
    [ -n "$pg" ] && kill -"$2" -- "-$pg" 2>/dev/null
    while kill -0 "$child" 2>/dev/null; do wait "$child"; done
    [ -n "$pg" ] && kill -KILL -- "-$pg" 2>/dev/null
  else
    log "waiter=[$who cmd=$cmd] waited=$(( $(date +%s) - started ))s gave up on a signal; holder=[${holder:-pid ? none}]"
  fi
  exit "$1"
}

# The one exit path: a holder writes exactly one end line, then releases the lock. KILL to
# the holder cannot be trapped: no end line is written, the lock stays held while the
# command runs (its pid is $LOCK/child), and once the command ends the next head clears the
# lock and logs "cleared a stale lock (holder pid P and command pid C dead)".
on_exit() {
  rm -f "$ticket"
  [ "$(cat "$LOCK/pid" 2>/dev/null)" = "$$" ] || return
  log "end=1 rc=$1 signal=$signal holder=[pid $$ $who cmd=$cmd]"
  release
}

main() {
  who="${HEAVY_LOCK_WHO:-$(instance)} session=${MONOCODE_SESSION_ID:-${PWD##*/}} cwd=$PWD"
  if light_job "$@" && shims_ready; then
    run_light_job "$@"
  fi
  cmd="$*"
  child='' signal=0
  holder=
  started=$(date +%s)
  mkdir -p "$QUEUE"
  ticket="$QUEUE/$(perl -MTime::HiRes=time -e 'printf "%017.6f", time').$$"
  trap 'on_exit $?' EXIT
  trap 'on_signal 130 INT' INT
  trap 'on_signal 143 TERM' TERM
  trap 'on_signal 129 HUP' HUP

  local waited report=0 waiting='' ahead hpid rc
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

[ "$long" = 1 ] && printf '%s long=1 holder=[pid %s %s cmd=%s]\n' "$(date '+%Y-%m-%d %H:%M:%S %Z')" "$$" "$who" "$*" >> "$WAITLOG"
HOLD_LIMIT=$HOLD_LIMIT perl -MPOSIX=:sys_wait_h -e '
  my $pid = fork; die "fork: $!" unless defined $pid;
  if (!$pid) { setpgrp(0, 0); $SIG{INT} = "DEFAULT"; exec @ARGV or exit 127 }
  sub stop_group { my ($sig, $code) = @_; kill $sig, -$pid;
    for (1 .. 20) { exit $code if waitpid($pid, WNOHANG) > 0; select(undef, undef, undef, 0.25) }
    kill "KILL", -$pid; waitpid($pid, 0); exit $code }
  $SIG{TERM} = sub { stop_group("TERM", 143) };
  $SIG{INT}  = sub { stop_group("INT", 130) };
  $SIG{HUP}  = sub { stop_group("HUP", 129) };
  $SIG{ALRM} = sub { stop_group("TERM", 124) };
  alarm $ENV{HOLD_LIMIT}; waitpid($pid, 0);
  exit($? & 127 ? 128 + ($? & 127) : $? >> 8);' -- "$@" <&0 &
  child=$!
  echo "$child" > "$LOCK/child"
  wait "$child"
  rc=$?
if [ "$rc" = 124 ]; then
  printf '%s timeout=1 limit=%ss holder=[pid %s %s cmd=%s]\n' "$(date '+%Y-%m-%d %H:%M:%S %Z')" "$HOLD_LIMIT" "$$" "$who" "$*" >> "$WAITLOG"
  echo "heavy-lock: hold limit ${HOLD_LIMIT}s reached; command killed: $*" >&2
fi
  exit "$rc"
}

# Bash reads a script as it runs. main is parsed whole before it starts, so an edit to
# this file cannot change a running caller.
main "$@"; exit
