#!/bin/bash
# ADE-183 cut-over: ONE detached job that does steps 4-6 of the cut-over plan after Tyler quits
# both apps (every agent runs inside MonoCode 2, so nothing in-app is left to run them).
#
#   cutover-ade183.sh <config file>      (started by cutover-ade183-launch.sh as a launchd job)
#
# The config file sets every path (bash KEY=VALUE, sourced), so the same script runs on isolated
# data: RUN (log + backup folder), HOME_DIR, APP1 ID1 DATA1 WEBKIT1 (the one app, the import
# target), APP2 ID2 DATA2 WEBKIT2 (MonoCode 2, the source), CLAUDE2 [CODEX2] (Space 2's logins),
# NEW_APP (the final build to install as APP1), TOOLS (a copy of scripts/spaces/merge-*.py),
# REAL (1 = pass --real to the importer), POLL (s), and FAIL_AT (tests only: force one failure).
#
# Steps, each stopping on its first failure:
#   wait      until both apps have exited by themselves; never kills or quits them
#   backup    ditto of APP1 APP2 DATA1 DATA2 WEBKIT1 WEBKIT2 CLAUDE2 CODEX2 into RUN/backup, a
#             sha256 manifest per folder (live == backup, sizes logged), PRAGMA integrity_check on
#             every SQLite file in the backup (on a scratch copy, so the backup stays as copied)
#   migrate   install NEW_APP as APP1 and open it once: the importer needs the Spaces tables, which
#             only the app's own migration makes (the installed build predates ADE-184); quit it
#             with a normal Apple Event quit when the tables are there and wait until it exits
#   report    merge-dry-run.py (ADE-169) after the migrate, so its counts are the ones imported
#   dry       merge-import.py dry run; counts recorded; after == before + insert for every table
#   apply     the real import into Space 2; tables and registered rows == the dry run, and the
#             live DB counts == the predicted "after"
#   rerun     a second apply makes 0 changes
#   relaunch  open the ONE app (APP1) and wait until it is up; MonoCode 2 stays closed
# Any failure after the backup starts: wait until no app runs, move the failed state aside into
# RUN/failed-state, restore every folder and both apps from the backup, prove restored == backup
# by the manifests, and reopen MonoCode 2 and MonoCode. Log: RUN/log.txt; last line result=...
set -uo pipefail
CONF=${1:?usage: cutover-ade183.sh <config file>}
# shellcheck source=/dev/null
. "$CONF"
: "${RUN:?}" "${HOME_DIR:?}" "${APP1:?}" "${ID1:?}" "${DATA1:?}" "${WEBKIT1:?}" "${APP2:?}" "${ID2:?}"
: "${DATA2:?}" "${WEBKIT2:?}" "${CLAUDE2:?}" "${NEW_APP:?}" "${TOOLS:?}"
CODEX2=${CODEX2:-}; REAL=${REAL:-0}; POLL=${POLL:-10}; FAIL_AT=${FAIL_AT:-}
UP_TIMEOUT=${UP_TIMEOUT:-180}; QUIT_TIMEOUT=${QUIT_TIMEOUT:-300}
LABEL=${XPC_SERVICE_NAME:-}
export HOME="$HOME_DIR"
BK=$RUN/backup; IMP=$RUN/import
mkdir -p "$RUN" "$IMP"
exec >>"$RUN/log.txt" 2>&1
STEP=start
log() { echo "$(date '+%Y-%m-%d %H:%M:%S %Z') [$STEP] $*"; }
finish() { log "result=$1"; case "$LABEL" in com.tyler.cutover-ade183*) launchctl bootout "gui/$(id -u)/$LABEL" ;; esac; exit "$2"; }

# Folders backed up and restored: name -> live path.
NAMES=(data1 data2 webkit1 webkit2 claude2); ROOTS=("$DATA1" "$DATA2" "$WEBKIT1" "$WEBKIT2" "$CLAUDE2")
[ -n "$CODEX2" ] && { NAMES+=(codex2); ROOTS+=("$CODEX2"); }
APPS=("$APP1" "$APP2"); IDS=("$ID1" "$ID2")

py() { python3 -I - "$@" <<'PY'
import hashlib, json, os, sqlite3, sys
from pathlib import Path
cmd, args = sys.argv[1], sys.argv[2:]
def ro(db):
    return sqlite3.connect(f"{Path(db).resolve().as_uri()}?mode=ro", uri=True)
if cmd == "manifest":  # <root> <out>: one line per entry: path, kind, size, sha256 or link target
    root, lines = Path(args[0]), []
    if not root.exists() and not root.is_symlink():
        lines = ["ABSENT"]
    else:
        for dirpath, dirs, files in os.walk(root):
            dirs.sort()
            for name in sorted(dirs + files):
                p = Path(dirpath, name); rel = p.relative_to(root)
                if p.is_symlink():
                    lines.append(f"{rel}\tL\t-\t{os.readlink(p)}")
                elif p.is_dir():
                    lines.append(f"{rel}\tD\t-\t-")
                elif p.is_file():  # sockets and pipes are skipped: ditto does not copy them
                    h = hashlib.sha256()
                    with open(p, "rb") as f:
                        for chunk in iter(lambda: f.read(1 << 20), b""):
                            h.update(chunk)
                    lines.append(f"{rel}\tF\t{p.stat().st_size}\t{h.hexdigest()}")
    Path(args[1]).write_text("\n".join(lines) + "\n")
    files = [l.split("\t") for l in lines if "\tF\t" in l]
    print(f"entries={len(lines)} files={len(files)} bytes={sum(int(f[2]) for f in files)}")
elif cmd == "sqlite-files":  # <root>: every SQLite file under root, by its header
    for dirpath, dirs, files in os.walk(args[0]):
        for name in files:
            p = Path(dirpath, name)
            if not p.is_symlink():
                with open(p, "rb") as f:
                    if f.read(16) == b"SQLite format 3\0":
                        print(p)
elif cmd == "has-table":  # <db> <table>
    try:
        db = ro(args[0])
        ok = db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (args[1],)).fetchone()
        sys.exit(0 if ok else 1)
    except sqlite3.Error:
        sys.exit(1)
elif cmd == "clock":  # <db>: mono_clock.last_wall_ms
    try:
        print(ro(args[0]).execute("SELECT last_wall_ms FROM mono_clock").fetchone()[0])
    except Exception:
        print(0)
elif cmd == "counts":  # <result.json>: the importer's counts, one line per table
    r = json.loads(Path(args[0]).read_text())
    for t, c in r["tables"].items():
        print(f"  {t}: space2={c['source']} before={c['before']} insert={c['insert']} after={c['after']}")
    print(f"  registered={len(r['registered'])} folders_copy={sum(len(f['copy']) for f in r['folders'].values())}"
          f" links={sum(a == 'link' for a in r['links'].values())} changes={r['changes']}")
elif cmd == "check-dry":  # <dry.json>: after == before + insert, insert <= space2 rows
    bad = [t for t, c in json.loads(Path(args[0]).read_text())["tables"].items()
           if c["after"] != c["before"] + c["insert"] or c["insert"] > c["source"]]
    print("mismatch: " + ", ".join(bad) if bad else "ok")
    sys.exit(1 if bad else 0)
elif cmd == "check-apply":  # <dry.json> <real.json> <live db>: real == dry, live counts == after
    dry, real = (json.loads(Path(a).read_text()) for a in args[:2])
    bad = [t for t in dry["tables"].keys() | real["tables"].keys()
           if dry["tables"].get(t) != real["tables"].get(t)]
    if dry["registered"] != real["registered"]:
        bad.append("registered")
    db = ro(args[2])
    bad += [f"{t}(live)" for t, c in real["tables"].items()
            if db.execute(f'SELECT count(*) FROM "{t}"').fetchone()[0] != c["after"]]
    print("mismatch: " + ", ".join(sorted(bad)) if bad else "ok")
    sys.exit(1 if bad else 0)
PY
}

main_exe() { echo "$1/Contents/MacOS/$(plutil -extract CFBundleExecutable raw "$1/Contents/Info.plist" 2>/dev/null)"; }
# The app's own process: argv is its main executable (or ends in it, for a script stand-in).
pid_of() { local m; m=$(main_exe "$1")
  ps -axo pid=,args= | awk -v m="$m" '{p=$1; $1=""; sub(/^ /,"");
    if ($0==m || (length($0)>length(m) && substr($0,length($0)-length(m))==" " m)) {print p; exit}}'; }
pstate() { ps -o stat= -p "$1" 2>/dev/null | tr -d ' '; }
now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }
any_running() { local a; for a in "${APPS[@]}"; do [ -n "$(pid_of "$a")" ] && return 0; done; return 1; }
forced() { [ "$FAIL_AT" = "$STEP" ] && { log "FORCED FAILURE (test hook FAIL_AT=$FAIL_AT)"; return 0; }; return 1; }

# up <app> <data> <launch ms>: the process left state T and its mono_clock ticked after the launch.
up() { local p st w
  for _ in $(seq 1 "$UP_TIMEOUT"); do
    p=$(pid_of "$1"); st=$([ -n "$p" ] && pstate "$p"); w=$(py clock "$2/monocode.db")
    if [ -n "$p" ] && [ "${st:0:1}" != T ] && [ "${w:-0}" -ge "$3" ]; then
      log "$(basename "$1") up: pid $p state $st clock $w >= launch $3"; return 0; fi
    sleep 1
  done
  log "$(basename "$1") NOT up: pid=${p:-none} state=${st:-none} clock=${w:-none} launch=$3"; return 1; }

# Restore the backup and reopen the old apps. Runs only after every app has exited.
rollback() { local i n ok=1
  STEP=rollback
  log "restore starts; waiting until no app runs (never killing one)"
  while any_running; do sleep "$POLL"; done
  mkdir -p "$RUN/failed-state"
  for i in "${!NAMES[@]}"; do
    n=${NAMES[$i]}
    if [ -e "${ROOTS[$i]}" ] || [ -L "${ROOTS[$i]}" ]; then mv "${ROOTS[$i]}" "$RUN/failed-state/$n" || ok=0; fi
    if [ -e "$BK/$n" ]; then mkdir -p "$(dirname "${ROOTS[$i]}")"; ditto "$BK/$n" "${ROOTS[$i]}" || ok=0; fi
    py manifest "${ROOTS[$i]}" "$RUN/manifest-restored-$n.txt" >/dev/null
    if cmp -s "$RUN/manifest-restored-$n.txt" "$BK/manifest-$n.txt"; then
      log "restored $n: manifest sha256 $(shasum -a 256 < "$BK/manifest-$n.txt" | cut -c1-16) == backup (${ROOTS[$i]})"
    else
      log "RESTORE MISMATCH $n: $RUN/manifest-restored-$n.txt != $BK/manifest-$n.txt"; ok=0
    fi
  done
  for i in 1 0; do
    rm -rf "$RUN/failed-state/app$((i + 1)).app"; mv "${APPS[$i]}" "$RUN/failed-state/app$((i + 1)).app" 2>/dev/null
    ditto "$BK/app$((i + 1)).app" "${APPS[$i]}" || ok=0
    log "restored app$((i + 1)) ${APPS[$i]} ($(plutil -extract CFBundleShortVersionString raw "${APPS[$i]}/Contents/Info.plist" 2>/dev/null))"
  done
  for i in 1 0; do
    t0=$(now_ms); open -a "${APPS[$i]}"
    up "${APPS[$i]}" "$([ "$i" = 0 ] && echo "$DATA1" || echo "$DATA2")" "$t0" || ok=0
  done
  [ "$ok" = 1 ] && finish "ROLLED-BACK (restored == backup, old apps reopened) after failure in $FAILED" 1
  finish "ROLLBACK-INCOMPLETE after failure in $FAILED: see the lines above; backup in $BK" 1
}
# fail <message>: before the backup starts nothing changed; after it, restore.
fail() { log "FAIL: $*"; FAILED=$STEP
  case "$STEP" in
    preflight) finish "FAILED-BEFORE-ANY-CHANGE ($STEP)" 1 ;;
    *) rollback ;;
  esac; }

STEP=preflight
log "config $CONF; run folder $RUN; label ${LABEL:-none}; FAIL_AT=${FAIL_AT:-none}"
for i in "${!NAMES[@]}"; do log "folder ${NAMES[$i]} = ${ROOTS[$i]}"; done
log "app1 $APP1 ($ID1) -> $NEW_APP; app2 $APP2 ($ID2)"
[ -f "$TOOLS/merge-import.py" ] && [ -f "$TOOLS/merge-dry-run.py" ] || fail "missing importer in $TOOLS"
for i in 0 1; do
  [ -d "${APPS[$i]}" ] || fail "missing ${APPS[$i]}"
  [ "$(plutil -extract CFBundleIdentifier raw "${APPS[$i]}/Contents/Info.plist")" = "${IDS[$i]}" ] || fail "${APPS[$i]} is not ${IDS[$i]}"
done
[ "$(plutil -extract CFBundleIdentifier raw "$NEW_APP/Contents/Info.plist" 2>/dev/null)" = "$ID1" ] || fail "$NEW_APP is not $ID1"
codesign --verify --deep "$NEW_APP" || fail "bad signature $NEW_APP"
[ -f "$DATA1/monocode.db" ] && [ -f "$DATA2/monocode.db" ] || fail "a monocode.db is missing"
[ -d "$CLAUDE2" ] || fail "missing $CLAUDE2"
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

# From here on any failure restores the backup.
STEP=backup
mkdir -p "$BK"
for i in 0 1; do ditto "${APPS[$i]}" "$BK/app$((i + 1)).app" || fail "copy ${APPS[$i]}"; done
for i in "${!NAMES[@]}"; do
  n=${NAMES[$i]}
  if [ -e "${ROOTS[$i]}" ]; then ditto "${ROOTS[$i]}" "$BK/$n" || fail "copy ${ROOTS[$i]}"; fi
  live=$(py manifest "${ROOTS[$i]}" "$RUN/manifest-live-$n.txt"); copy=$(py manifest "$BK/$n" "$BK/manifest-$n.txt")
  cmp -s "$RUN/manifest-live-$n.txt" "$BK/manifest-$n.txt" || fail "backup of $n differs from ${ROOTS[$i]}"
  log "$n: live $live == backup $copy (du $(du -sk "$BK/$n" 2>/dev/null | cut -f1) KiB)"
done
mkdir -p "$RUN/scratch"
while IFS= read -r db; do
  s="$RUN/scratch/check.db"; rm -f "$s" "$s-wal" "$s-shm"
  cp "$db" "$s"; for x in wal shm; do [ -f "$db-$x" ] && cp "$db-$x" "$s-$x"; done
  r=$(sqlite3 "$s" "PRAGMA integrity_check" 2>&1 | tr '\n' ' ')
  [ "$r" = "ok " ] || fail "integrity_check ${db#"$BK"/}: $r"
  log "integrity_check ok: ${db#"$BK"/}"
done < <(for n in "${NAMES[@]}"; do [ -d "$BK/$n" ] && py sqlite-files "$BK/$n"; done)
rm -rf "$RUN/scratch"
forced && fail "forced"
log "backup complete: $BK"

STEP=migrate
rm -rf "$APP1" && ditto "$NEW_APP" "$APP1" || fail "install $NEW_APP"
log "installed $APP1: $(codesign -dvv "$APP1" 2>&1 | grep -E '^(Identifier|Authority)' | tr '\n' ' ')"
t0=$(now_ms); open -a "$APP1" || fail "open $APP1"
up "$APP1" "$DATA1" "$t0" || fail "the new app did not come up"
for _ in $(seq 1 "$UP_TIMEOUT"); do py has-table "$DATA1/monocode.db" spaces && break; sleep 1; done
py has-table "$DATA1/monocode.db" spaces || fail "no Spaces tables in $DATA1 after the new app started"
log "Spaces tables present; quitting the new app (Apple Event quit)"
osascript -e "tell application id \"$ID1\" to quit" & qpid=$!
for _ in $(seq 1 "$QUIT_TIMEOUT"); do [ -z "$(pid_of "$APP1")" ] && break; sleep 1; done
kill "$qpid" 2>/dev/null; wait "$qpid" 2>/dev/null
[ -z "$(pid_of "$APP1")" ] || fail "the new app did not quit in ${QUIT_TIMEOUT}s"
forced && fail "forced"
log "migrated; the new app exited"

STEP=report
WK=(); [ -d "$WEBKIT1" ] && WK+=(--webkit1 "$WEBKIT1"); [ -d "$WEBKIT2" ] && WK+=(--webkit2 "$WEBKIT2")
python3 -I "$TOOLS/merge-dry-run.py" --out "$IMP/report" --space1 "$DATA1" --space2 "$DATA2" "${WK[@]}" \
  > "$IMP/report.out" 2>&1 || fail "merge-dry-run.py: $(tail -3 "$IMP/report.out")"
log "report: $IMP/report/merge-dry-run.json"

IMPORT=(python3 -I "$TOOLS/merge-import.py" --space1 "$DATA1" --space2 "$DATA2" --report "$IMP/report/merge-dry-run.json"
  --claude-home "$CLAUDE2" --backup "$IMP/backups" --json)
[ -n "$CODEX2" ] && IMPORT+=(--codex-home "$CODEX2")
[ "$REAL" = 1 ] && IMPORT+=(--real)
STEP=dry
"${IMPORT[@]}" > "$IMP/dry.json" 2> "$IMP/dry.err" || fail "importer dry run: $(cat "$IMP/dry.err")"
log "importer dry run counts:"; py counts "$IMP/dry.json"
r=$(py check-dry "$IMP/dry.json") || fail "dry-run counts: $r"
forced && fail "forced"
log "dry-run counts consistent: $r"

STEP=apply
"${IMPORT[@]}" --apply > "$IMP/real.json" 2> "$IMP/real.err" || fail "importer apply: $(cat "$IMP/real.err")"
log "importer apply counts (importer backup $(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["backup"])' "$IMP/real.json")):"
py counts "$IMP/real.json"
cp "$IMP/dry.json" "$IMP/dry.compare.json"
if [ "$FAIL_AT" = compare ]; then
  log "FORCED FAILURE (test hook FAIL_AT=compare): one dry-run count corrupted before the compare"
  python3 -c 'import json,sys; p=sys.argv[1]; r=json.load(open(p)); t=next(iter(r["tables"])); r["tables"][t]["insert"]+=1; json.dump(r,open(p,"w"))' "$IMP/dry.compare.json"
fi
r=$(py check-apply "$IMP/dry.compare.json" "$IMP/real.json" "$DATA1/monocode.db") || fail "apply counts != dry run: $r"
forced && fail "forced"
log "apply counts == dry run and the live DB: $r"

STEP=rerun
"${IMPORT[@]}" --apply > "$IMP/rerun.json" 2> "$IMP/rerun.err" || fail "importer re-run: $(cat "$IMP/rerun.err")"
c=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["changes"])' "$IMP/rerun.json")
[ "$c" = 0 ] || fail "the re-run made $c changes"
forced && fail "forced"
log "re-run made 0 changes"

STEP=relaunch
forced && fail "forced"
t0=$(now_ms); open -a "$APP1" || fail "open $APP1"
up "$APP1" "$DATA1" "$t0" || fail "the one app did not come up"
finish "OK (backup $BK; one app $APP1 relaunched; MonoCode 2 stays closed)" 0
