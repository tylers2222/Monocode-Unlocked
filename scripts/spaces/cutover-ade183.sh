#!/bin/bash
# ADE-183 cut-over: ONE detached job that does steps 4-6 of the cut-over plan after Tyler quits
# both apps (every agent runs inside MonoCode 2, so nothing in-app is left to run them).
#
#   cutover-ade183.sh <config file>      (started by cutover-ade183-launch.sh as a launchd job)
#
# The config file sets every path (bash KEY=VALUE, sourced), so the same script runs on isolated
# data: RUN (log + backup folder), LABEL + PLIST (its launchd job), HOME_DIR, APP1 ID1 DATA1 WEBKIT1
# (the one app, the import target), APP2 ID2 DATA2 WEBKIT2 (MonoCode 2, the source), CLAUDE2
# [CODEX2] (Space 2's logins), NEW_APP (the final build to install as APP1), TOOLS (a copy of
# scripts/spaces/merge-*.py and cutover_checks.py), PYTHON (python3 >= 3.10, absolute: the job's
# PATH is /usr/bin:/bin:/usr/sbin:/sbin), REAL (1 = pass --real to the importer), POLL (s), and
# FAIL_AT (tests only: a step name forces a failure at the end of that step; compare and envcheck
# feed a bad input to that check; term sends the job SIGTERM after the apply).
#
# It runs once per RUN folder: a RUN whose log already has a result line is refused, and on every
# exit the job deletes PLIST and boots LABEL out, so launchd never starts it again.
#
# Every app open is `env -i HOME USER SHELL PATH open -a`, so no agent's env (CLAUDE_CONFIG_DIR,
# CODEX_HOME, MONOCODE_APP_TOKEN, ...) reaches a launched app; after each open `ps eww` reads the
# app's env, and any CLAUDE*/CODEX*/MONOCODE*/ANTHROPIC*/OPENAI* var that its own Info.plist
# LSEnvironment does not set with exactly that value quits the app and counts as a failure.
#
# Steps, each stopping on its first failure:
#   wait      until both apps have exited by themselves; never kills or quits them
#   backup    ditto of APP1 APP2 DATA1 DATA2 WEBKIT1 WEBKIT2 CLAUDE2 CODEX2 into RUN/backup, a
#             sha256 manifest per item (live == backup, sizes logged), codesign --strict on both
#             app copies, PRAGMA integrity_check on every SQLite file in the backup (on a scratch
#             copy, so the backup stays as copied); each item that checks out gets a marker in
#             RUN/backup/.verified
#   migrate   install NEW_APP (codesign --strict, bundle id) as APP1 and open it once: the importer
#             needs the Spaces tables, which only the app's own migration makes (the installed
#             build predates ADE-184); quit it with a normal Apple Event quit as soon as the tables
#             are there and wait until it exits; logs the open, tables and quit times and every
#             session that ran in that window
#   report    merge-dry-run.py (ADE-169) after the migrate, so its counts are the ones imported
#   dry       merge-import.py dry run; counts recorded; after == before + insert for every table
#   apply     the real import into Space 2; tables and registered rows == the dry run, and the
#             live DB counts == the predicted "after"
#   rerun     a second apply makes 0 changes
#   relaunch  open the ONE app (APP1) and wait until it is up; MonoCode 2 stays closed
# A failure before the migrate changes nothing live: the job stops, restores nothing and reopens
# the old apps. A failure from the migrate on: the job kills the app instance it opened if that
# instance did not come up or quit (by pid, path and start time), then restores each item from its
# verified backup (copy next to the live path, check, then swap by rename; an item without a
# verified backup is left as it is). Only when every item is restored does it reopen MonoCode 2 and
# MonoCode; otherwise both stay closed and the result names the items. It never waits on Tyler: if
# an app it did not open is running, it restores nothing (ROLLBACK-NEEDS-HUMAN).
# Log: RUN/log.txt; last line result=...
#
# RECOVERY after a hard kill or power loss from the migrate on, or a result other than OK or
# ROLLED-BACK: the job does not resume, and a re-start on the same RUN is refused. By hand, with
# both apps closed, for each item n in RUN/backup/.verified (its live path is in RUN/log.txt,
# "item n = <path>"):
#   1. If <live>.restore-<RUN name> exists and `cutover_checks.py manifest <it> /tmp/m` gives a
#      file equal to RUN/backup/manifest-n.txt, use it as the copy; else ditto RUN/backup/n to
#      <live>.restore-<RUN name> and check it the same way.
#   2. Move <live> (if there) and any <live>.failed-<RUN name> into RUN/failed-state/, then rename
#      the checked copy to <live>. An item whose backup manifest is ABSENT has no live path after.
#   3. When every item is done, open MonoCode 2 and MonoCode (env -i, as the job does).
set -uo pipefail
CONF=${1:?usage: cutover-ade183.sh <config file>}
# shellcheck source=/dev/null
. "$CONF"
: "${RUN:?}" "${LABEL:?}" "${PLIST:?}" "${HOME_DIR:?}" "${APP1:?}" "${ID1:?}" "${DATA1:?}" "${WEBKIT1:?}"
: "${APP2:?}" "${ID2:?}" "${DATA2:?}" "${WEBKIT2:?}" "${CLAUDE2:?}" "${NEW_APP:?}" "${TOOLS:?}" "${PYTHON:?}"
CODEX2=${CODEX2:-}; REAL=${REAL:-0}; POLL=${POLL:-10}; FAIL_AT=${FAIL_AT:-}
UP_TIMEOUT=${UP_TIMEOUT:-180}; QUIT_TIMEOUT=${QUIT_TIMEOUT:-300}
export HOME="$HOME_DIR"
BK=$RUN/backup; OKD=$RUN/backup/.verified; IMP=$RUN/import; TAG=$(basename "$RUN")
mkdir -p "$RUN" "$IMP"
exec >>"$RUN/log.txt" 2>&1
STEP=start
log() { echo "$(date '+%Y-%m-%d %H:%M:%S %Z') [$STEP] $*"; }
# On every exit: delete the plist (it lives in RUN, never in ~/Library/LaunchAgents), then boot the
# label out in the background, as the last act: a bootout of the job itself waits for it to exit.
unload() { rm -f "$PLIST"; log "launchd: removed $PLIST; bootout of $LABEL follows"; }
bootout() { case "$LABEL" in com.tyler.cutover-ade183*) launchctl bootout "gui/$(id -u)/$LABEL" >/dev/null 2>&1 & ;; esac; }
finish() { DONE=1; unload; log "result=$1"; bootout; exit "$2"; }

if grep -q '\] result=' "$RUN/log.txt"; then
  log "REFUSED: $RUN already has a result line; the job runs once per run folder and changed nothing"
  unload; bootout; exit 1
fi

# Items backed up and restored: name -> live path. Both apps are items too.
NAMES=(app1 app2 data1 data2 webkit1 webkit2 claude2); ROOTS=("$APP1" "$APP2" "$DATA1" "$DATA2" "$WEBKIT1" "$WEBKIT2" "$CLAUDE2")
[ -n "$CODEX2" ] && { NAMES+=(codex2); ROOTS+=("$CODEX2"); }
APPS=("$APP1" "$APP2"); IDS=("$ID1" "$ID2"); DATAS=("$DATA1" "$DATA2")

py() { "$PYTHON" -I "$TOOLS/cutover_checks.py" "$@"; }
main_exe() { echo "$(cd "$1" 2>/dev/null && pwd -P)/Contents/MacOS/$(plutil -extract CFBundleExecutable raw "$1/Contents/Info.plist" 2>/dev/null)"; }
# The app's own process: argv is exactly its main executable (symlinks resolved: /tmp is /private/tmp).
pid_of() { local m; m=$(main_exe "$1")
  ps -axo pid=,args= | awk -v m="$m" '{p=$1; $1=""; sub(/^ /,""); if ($0==m) {print p; exit}}'; }
pstate() { ps -o stat= -p "$1" 2>/dev/null | tr -d ' '; }
now_ms() { "$PYTHON" -c 'import time; print(int(time.time()*1000))'; }
# Running = its process is there OR its data folder's scheduler.lock is held (a second signal).
any_running() { local a
  for a in "${APPS[@]}"; do [ -n "$(pid_of "$a")" ] && return 0; done
  py lock-held "$DATA1" || py lock-held "$DATA2"; }
# An app may only start again after the import is over.
stay_closed() { any_running && fail "an app started during $STEP; the migrate and import need both apps closed"; }
forced() { [ "$FAIL_AT" = "$STEP" ] && { log "FORCED FAILURE (test hook FAIL_AT=$FAIL_AT)"; return 0; }; return 1; }
app_ok() { codesign --verify --deep --strict "$1" && [ "$(plutil -extract CFBundleIdentifier raw "$1/Contents/Info.plist")" = "$2" ]; }
# signed <item name> <path>: an app item must pass app_ok with its bundle id; other items pass.
signed() { case $1 in app*) app_ok "$2" "${IDS[${1#app} - 1]}" ;; esac; }

# up <app> <data> <launch ms>: the process left state T and its mono_clock ticked after the launch.
up() { local p st w
  for _ in $(seq 1 "$UP_TIMEOUT"); do
    p=$(pid_of "$1"); st=$([ -n "$p" ] && pstate "$p"); w=$(py clock "$2/monocode.db")
    if [ -n "$p" ] && [ "${st:0:1}" != T ] && [ "${w:-0}" -ge "$3" ]; then
      log "$(basename "$1") up: pid $p state $st clock $w >= launch $3"; return 0; fi
    sleep 1
  done
  log "$(basename "$1") NOT up: pid=${p:-none} state=${st:-none} clock=${w:-none} launch=$3"; return 1; }

# The job owns only the app instance it opened itself. When its open of app i ends, claim records
# the one instance that started at or after that open: OWN_PID[i] and its exact `ps lstart`
# OWN_START[i]. A successful quit clears the record, so an instance Tyler starts later is never
# the job's. own_pid <i>: the recorded pid, while it still has that start time and app i's main
# executable as its argv.
OWN_PID=("" ""); OWN_START=("" "")
start_of() { LC_ALL=C ps -o lstart= -p "$1" 2>/dev/null | sed 's/ *$//'; }
claim() { local p s t; p=$(pid_of "${APPS[$1]}"); [ -n "$p" ] || return 0
  s=$(start_of "$p"); t=$(LC_ALL=C date -j -f '%a %b %e %T %Y' "$s" +%s 2>/dev/null) || return 0
  [ "$t" -ge "$2" ] || return 0
  OWN_PID[$1]=$p; OWN_START[$1]=$s; log "own instance of $(basename "${APPS[$1]}"): pid $p, started $s"; }
disown_app() { OWN_PID[$1]=""; OWN_START[$1]=""; }
own_pid() { local p=${OWN_PID[$1]}; [ -n "$p" ] || return 1
  [ "$(start_of "$p")" = "${OWN_START[$1]}" ] && [ "$(ps -o args= -p "$p" 2>/dev/null | sed 's/ *$//')" = "$(main_exe "${APPS[$1]}")" ] && echo "$p"; }
# kill_own <i>: TERM, then after 10 s KILL, the instance of app i the job opened, re-checked by
# own_pid before each signal. Never by name; never an instance the job did not open.
kill_own() { local p
  p=$(own_pid "$1") || return 0
  log "TERM to $(basename "${APPS[$1]}") pid $p (opened by this job; started ${OWN_START[$1]})"; kill -TERM "$p"
  for _ in $(seq 1 10); do [ "$(own_pid "$1")" = "$p" ] || break; sleep 1; done
  if [ "$(own_pid "$1")" = "$p" ]; then log "KILL to pid $p"; kill -KILL "$p"; sleep 1; fi
  [ -z "$(start_of "$p")" ] && { disown_app "$1"; log "$(basename "${APPS[$1]}") pid $p is gone"; }; }

# quit_app <i>: a normal Apple Event quit to app i, then wait until it is gone; past QUIT_TIMEOUT
# the job kills its own instance and reports the quit as failed.
quit_app() { local q app=${APPS[$1]}
  osascript -e "tell application id \"${IDS[$1]}\" to quit" >/dev/null 2>&1 & q=$!
  for _ in $(seq 1 "$QUIT_TIMEOUT"); do [ -z "$(pid_of "$app")" ] && break; sleep 1; done
  kill "$q" 2>/dev/null; wait "$q" 2>/dev/null
  [ -z "$(pid_of "$app")" ] && { disown_app "$1"; log "$(basename "$app") quit"; return 0; }
  log "$(basename "$app") did not quit in ${QUIT_TIMEOUT}s"; kill_own "$1"; return 1; }

# open_app <i> [open args]: clean-env open of app i, wait until up, check its env. An instance that
# does not come up or fails the env check is not left running.
open_app() { local app=${APPS[$1]} i=$1 since t0 p r; shift
  since=$(date +%s); t0=$(now_ms)
  env -i HOME="$HOME" USER="${USER:-$(id -un)}" SHELL=/bin/zsh PATH=/usr/bin:/bin open -a "$app" "$@" \
    || { log "open $app failed"; return 1; }
  if ! up "$app" "${DATAS[$i]}" "$t0"; then claim "$i" "$since"; kill_own "$i"; return 1; fi
  claim "$i" "$since"
  p=$(pid_of "$app")
  if r=$(py env-check "$p" "$app"); then log "env check PASS $(basename "$app") pid $p: $r"; return 0; fi
  log "env check FAIL $(basename "$app") pid $p: $r; quitting it at once"
  quit_app "$i"; return 1; }

reopen_old() { local i ok=0
  for i in 1 0; do open_app "$i" || ok=1; done
  return "$ok"; }

# restore <i>: copy item i's verified backup next to its live path, check the copy, then swap by
# two renames in one folder. The live path is never moved before its copy checks out, so a kill
# at any point leaves the live item, or the checked copy plus the moved-aside item, in place.
restore() { local n=${NAMES[$1]} live=${ROOTS[$1]} tmp old
  tmp=$live.restore-$TAG; old=$live.failed-$TAG
  [ -f "$OKD/$n" ] || { log "NOT restored $n: no verified backup; $live left as it is"; return 1; }
  rm -rf "$tmp"
  if [ -e "$BK/$n" ]; then ditto "$BK/$n" "$tmp" || { log "NOT restored $n: copy to $tmp failed; $live left as it is"; return 1; }; fi
  py manifest "$tmp" "$RUN/manifest-restored-$n.txt" >/dev/null
  cmp -s "$RUN/manifest-restored-$n.txt" "$BK/manifest-$n.txt" \
    || { log "NOT restored $n: $tmp != backup manifest; $live left as it is"; return 1; }
  signed "$n" "$tmp" || { log "NOT restored $n: $tmp fails codesign --strict"; return 1; }
  if [ -e "$live" ] || [ -L "$live" ]; then mv "$live" "$old" || { log "NOT restored $n: cannot move $live aside"; return 1; }; fi
  if [ -e "$tmp" ]; then mv "$tmp" "$live" || { log "RESTORE BROKEN $n: $live is at $old, the checked copy at $tmp"; return 1; }; fi
  if [ -e "$old" ] || [ -L "$old" ]; then mv "$old" "$RUN/failed-state/$n" || log "failed state of $n stays at $old"; fi
  log "restored $n: manifest sha256 $(shasum -a 256 < "$BK/manifest-$n.txt" | cut -c1-16) == backup ($live)"; }

# Restore the backup, then reopen the old apps only if every item was restored. It never waits on
# Tyler: the job's own instances are killed first, and an app still running 30 s later is one the
# job did not open, so nothing is restored under it. A SIGTERM (bootout, logout) no longer stops
# the rollback; the plist gives it a long ExitTimeOut before a SIGKILL.
rollback() { local i bad=()
  trap '' TERM
  STEP=rollback
  log "restore starts (manual steps if this stops: RECOVERY in the header of $RUN/cutover-ade183.sh)"
  for i in 0 1; do kill_own "$i"; done
  for _ in $(seq 1 30); do any_running || break; sleep 1; done
  if any_running; then
    finish "ROLLBACK-NEEDS-HUMAN after failure in $FAILED: an app this job did not open is running; nothing restored, apps left as they are; quit it, then RECOVERY" 1
  fi
  mkdir -p "$RUN/failed-state"
  for i in "${!NAMES[@]}"; do restore "$i" || bad+=("${NAMES[$i]}"); done
  if [ ${#bad[@]} -gt 0 ]; then
    finish "ROLLBACK-INCOMPLETE after failure in $FAILED: not restored: ${bad[*]}; apps left closed; see RECOVERY; backup in $BK" 1
  fi
  reopen_old && finish "ROLLED-BACK (restored == backup, old apps reopened) after failure in $FAILED" 1
  finish "ROLLBACK-INCOMPLETE after failure in $FAILED: restored == backup, but an old app did not reopen clean" 1
}
# Before the migrate nothing live has changed: stop and restore nothing. In the backup step Tyler
# has quit both apps, so reopen them. From the migrate on, restore.
DONE=0
stop() { FAILED=$STEP
  case $STEP in
    start|preflight|wait) finish "FAILED-BEFORE-ANY-CHANGE ($FAILED)" 1 ;;
    backup) reopen_old && finish "FAILED-BEFORE-ANY-CHANGE ($FAILED): nothing restored, old apps reopened" 1
      finish "FAILED-BEFORE-ANY-CHANGE ($FAILED): nothing restored; an old app did not reopen clean" 1 ;;
    *) rollback ;;
  esac; }
fail() { log "FAIL: $*"; stop; }
# An exit that is not finish (a bash error, a signal) goes the same way as a failure.
# shellcheck disable=SC2329  # run by the EXIT trap
on_exit() { [ "$DONE" = 1 ] && return; set +eu; log "FAIL: unexpected exit in $STEP"; stop; }
trap on_exit EXIT
trap '[ "$DONE" = 1 ] || log "signal TERM"; exit 1' TERM

STEP=preflight
log "config $CONF; run folder $RUN; label $LABEL; FAIL_AT=${FAIL_AT:-none}"
for i in "${!NAMES[@]}"; do log "item ${NAMES[$i]} = ${ROOTS[$i]}"; done
log "app1 $APP1 ($ID1) -> $NEW_APP; app2 $APP2 ($ID2)"
for f in merge-import.py merge-dry-run.py cutover_checks.py; do [ -f "$TOOLS/$f" ] || fail "missing $TOOLS/$f"; done
[ -e "$BK" ] && fail "$BK already exists; a backup is never merged over"
for i in 0 1; do
  [ -d "${APPS[$i]}" ] || fail "missing ${APPS[$i]}"
  [ "$(plutil -extract CFBundleIdentifier raw "${APPS[$i]}/Contents/Info.plist")" = "${IDS[$i]}" ] || fail "${APPS[$i]} is not ${IDS[$i]}"
done
app_ok "$NEW_APP" "$ID1" || fail "$NEW_APP is not a $ID1 app with a valid signature (codesign --strict)"
[ -f "$DATA1/monocode.db" ] && [ -f "$DATA2/monocode.db" ] || fail "a monocode.db is missing"
[ -d "$CLAUDE2" ] || fail "missing $CLAUDE2"
log "running now: $(for a in "${APPS[@]}"; do printf '%s=pid %s ' "$(basename "$a")" "$(pid_of "$a")"; done)"
log "ok; new app $(plutil -extract CFBundleShortVersionString raw "$NEW_APP/Contents/Info.plist") $(codesign -dvv "$NEW_APP" 2>&1 | grep -E '^(Identifier|Authority)' | tr '\n' ' ')"

STEP="wait"
log "waiting until both apps have exited by themselves (poll every ${POLL}s)"
gone=0; last=0
while [ "$gone" -lt 2 ]; do
  if any_running; then gone=0; else gone=$((gone + 1)); fi
  [ $(( $(date +%s) - last )) -ge 300 ] && { log "still running: $(for a in "${APPS[@]}"; do p=$(pid_of "$a"); [ -n "$p" ] && printf '%s(pid %s) ' "$(basename "$a")" "$p"; done)"; last=$(date +%s); }
  [ "$gone" -lt 2 ] && sleep "$POLL"
done
log "both apps have exited"

STEP=backup
mkdir -p "$OKD"
for i in "${!NAMES[@]}"; do
  n=${NAMES[$i]}
  if [ -e "${ROOTS[$i]}" ]; then ditto "${ROOTS[$i]}" "$BK/$n" || fail "copy ${ROOTS[$i]}"; fi
  live=$(py manifest "${ROOTS[$i]}" "$RUN/manifest-live-$n.txt"); copy=$(py manifest "$BK/$n" "$BK/manifest-$n.txt")
  cmp -s "$RUN/manifest-live-$n.txt" "$BK/manifest-$n.txt" || fail "backup of $n differs from ${ROOTS[$i]}"
  signed "$n" "$BK/$n" || fail "backup of $n fails codesign --strict"
  touch "$OKD/$n"
  log "$n: live $live == backup $copy (du $(du -sk "$BK/$n" 2>/dev/null | cut -f1) KiB); verified"
done
mkdir -p "$RUN/scratch"
while IFS= read -r db; do
  s="$RUN/scratch/check.db"; rm -f "$s" "$s-wal" "$s-shm"
  cp "$db" "$s"; for x in wal shm; do [ -f "$db-$x" ] && cp "$db-$x" "$s-$x"; done
  r=$(sqlite3 "$s" "PRAGMA integrity_check" 2>&1 | tr '\n' ' ')
  [ "$r" = "ok " ] || fail "integrity_check ${db#"$BK"/}: $r"
  log "integrity_check ok: ${db#"$BK"/}"
done < <(for n in "${NAMES[@]}"; do [[ $n != app* ]] && [ -d "$BK/$n" ] && py sqlite-files "$BK/$n"; done)
rm -rf "$RUN/scratch"
forced && fail "forced"
stay_closed
log "backup complete: $BK"

STEP=migrate
if ! { rm -rf "$APP1" && ditto "$NEW_APP" "$APP1"; }; then fail "install $NEW_APP"; fi
app_ok "$APP1" "$ID1" || fail "bad signature after install $APP1"
log "installed $APP1: $(codesign -dvv "$APP1" 2>&1 | grep -E '^(Identifier|Authority)' | tr '\n' ' ')"
PLANT=(); [ "$FAIL_AT" = envcheck ] && { PLANT=(--env MONOCODE_APP_TOKEN=dummy); log "FORCED FAILURE (test hook FAIL_AT=envcheck): open with a planted MONOCODE_APP_TOKEN=dummy"; }
opened=$(date '+%H:%M:%S')
log "migration open at $opened"
open_app 0 ${PLANT[@]+"${PLANT[@]}"} || fail "the new app's migration open failed"
for _ in $(seq 1 "$UP_TIMEOUT"); do py has-table "$DATA1/monocode.db" spaces && break; sleep 1; done
py has-table "$DATA1/monocode.db" spaces || { quit_app 0; fail "no Spaces tables in $DATA1 after the new app started"; }
log "Spaces tables present at $(date '+%H:%M:%S'); quitting the new app (Apple Event quit)"
quit_app 0 || fail "the new app did not quit"
log "migration window: open $opened, quit $(date '+%H:%M:%S'); sessions that ran in it (step 7 checks them):"
mkdir -p "$RUN/scratch"; s="$RUN/scratch/before.db"
cp "$BK/data1/monocode.db" "$s"; for x in wal shm; do [ -f "$BK/data1/monocode.db-$x" ] && cp "$BK/data1/monocode.db-$x" "$s-$x"; done
py window "$s" "$DATA1/monocode.db"; rm -rf "$RUN/scratch"
forced && fail "forced"
log "migrated; the new app exited"

STEP=report
WK=(); [ -d "$WEBKIT1" ] && WK+=(--webkit1 "$WEBKIT1"); [ -d "$WEBKIT2" ] && WK+=(--webkit2 "$WEBKIT2")
"$PYTHON" -I "$TOOLS/merge-dry-run.py" --out "$IMP/report" --space1 "$DATA1" --space2 "$DATA2" ${WK[@]+"${WK[@]}"} \
  > "$IMP/report.out" 2>&1 || fail "merge-dry-run.py: $(tail -3 "$IMP/report.out")"
forced && fail "forced"
log "report: $IMP/report/merge-dry-run.json"

IMPORT=("$PYTHON" -I "$TOOLS/merge-import.py" --space1 "$DATA1" --space2 "$DATA2" --report "$IMP/report/merge-dry-run.json"
  --claude-home "$CLAUDE2" --backup "$IMP/backups" --json)
[ -n "$CODEX2" ] && IMPORT+=(--codex-home "$CODEX2")
[ "$REAL" = 1 ] && IMPORT+=(--real)
STEP=dry
stay_closed
"${IMPORT[@]}" > "$IMP/dry.json" 2> "$IMP/dry.err" || fail "importer dry run: $(cat "$IMP/dry.err")"
log "importer dry run counts:"; py counts "$IMP/dry.json"
r=$(py check-dry "$IMP/dry.json") || fail "dry-run counts: $r"
forced && fail "forced"
log "dry-run counts consistent: $r"

STEP=apply
stay_closed
"${IMPORT[@]}" --apply > "$IMP/real.json" 2> "$IMP/real.err" || fail "importer apply: $(cat "$IMP/real.err")"
log "importer apply counts (importer backup $(py json-field "$IMP/real.json" backup)):"
py counts "$IMP/real.json"
cp "$IMP/dry.json" "$IMP/dry.compare.json"
if [ "$FAIL_AT" = compare ]; then
  log "FORCED FAILURE (test hook FAIL_AT=compare): one dry-run count corrupted before the compare"
  "$PYTHON" -c 'import json,sys; p=sys.argv[1]; r=json.load(open(p)); t=next(iter(r["tables"])); r["tables"][t]["insert"]+=1; json.dump(r,open(p,"w"))' "$IMP/dry.compare.json"
fi
r=$(py check-apply "$IMP/dry.compare.json" "$IMP/real.json" "$DATA1/monocode.db") || fail "apply counts != dry run: $r"
forced && fail "forced"
[ "$FAIL_AT" = term ] && { log "FORCED FAILURE (test hook FAIL_AT=term): SIGTERM to the job"; kill -TERM $$; }
log "apply counts == dry run and the live DB: $r"

STEP=rerun
"${IMPORT[@]}" --apply > "$IMP/rerun.json" 2> "$IMP/rerun.err" || fail "importer re-run: $(cat "$IMP/rerun.err")"
c=$(py json-field "$IMP/rerun.json" changes)
[ "$c" = 0 ] || fail "the re-run made $c changes"
forced && fail "forced"
log "re-run made 0 changes"

STEP=relaunch
forced && fail "forced"
open_app 0 || fail "the one app did not come up clean"
finish "OK (backup $BK; one app $APP1 relaunched; MonoCode 2 stays closed; agent logins NOT checked)" 0
