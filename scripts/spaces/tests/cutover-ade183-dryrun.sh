#!/bin/bash
# Isolated dry run of the ADE-183 cut-over job: made-up data under a new temp HOME in /tmp,
# stand-in apps (stay-open AppleScript applets with made-up com.example bundle ids, new per run).
# Never real data, and no plist is loaded: the launcher runs with LOAD=0 and this harness starts the
# job itself with the launchd job's env.
#
#   cutover-ade183-dryrun.sh <case>... | all
#
# Each case states its expected result line and checks the outcome itself, outside the job:
# every item (both apps, both data folders, both WebKit folders, .claude-2, .codex-2) is hashed
# after Tyler's quit and again after the job; SQLite files by their content without mono_clock,
# which the reopened stand-ins tick. Exits 1 if any case fails.
#   ok              result=OK; Space 2 unchanged, Space 1 has the import, the new app installed;
#                   then the job is started again on the same run folder and must refuse
#   backup-partial  an unreadable file in .claude-2 stops the backup: claude2 backed up partly,
#                   codex2 not at all; nothing live may move (FAILED-BEFORE-ANY-CHANGE)
#   backup          forced failure at the end of the backup: FAILED-BEFORE-ANY-CHANGE
#   migrate report dry apply rerun relaunch
#                   forced failure at the end of that step: ROLLED-BACK, every item == before
#   compare         one dry-run count corrupted before the apply compare: ROLLED-BACK (apply)
#   envcheck        MONOCODE_APP_TOKEN planted on the migration open: ROLLED-BACK (migrate)
#   term            SIGTERM to the job after the apply (the EXIT-trap path): ROLLED-BACK (apply)
#   restore-fail    forced failure in the migrate, and one file in backup/data1 made unreadable
#                   once the backup is done: ROLLBACK-INCOMPLETE naming data1; no app reopened
#   stuck-quit      the new-app stand-in ignores quit: the job kills its own instance (TERM, then
#                   KILL) and restores without waiting on anyone: ROLLED-BACK (migrate)
#   stuck-up        the new-app stand-in never ticks its clock: the same, from the up() timeout
# In every case the job's plist must be gone and the log must hold exactly one result line.
# shellcheck disable=SC2329  # helpers run through check and wait_for
set -uo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
ALL=(ok backup-partial backup migrate report dry apply rerun relaunch compare envcheck term restore-fail stuck-quit stuck-up)
[ $# -gt 0 ] || { echo "usage: cutover-ade183-dryrun.sh <case>... | all (${ALL[*]})" >&2; exit 2; }
[ "$1" = all ] && set -- "${ALL[@]}"
ROLLED='result=ROLLED-BACK \(restored == backup, old apps reopened\) after failure in'
UNCHANGED='result=FAILED-BEFORE-ANY-CHANGE \(backup\): nothing restored, old apps reopened$'

expect_of() {
  case $1 in
    ok) echo 'result=OK \(.*; agent logins NOT checked\)$' ;;
    backup|backup-partial) echo "$UNCHANGED" ;;
    compare|term) echo "$ROLLED apply$" ;;
    envcheck|stuck-quit|stuck-up) echo "$ROLLED migrate$" ;;
    restore-fail) echo 'result=ROLLBACK-INCOMPLETE after failure in migrate: not restored: data1; apps left closed' ;;
    migrate|report|dry|apply|rerun|relaunch) echo "$ROLLED $1$" ;;
    *) return 1 ;;
  esac; }
fail_at_of() { case $1 in ok|backup-partial|stuck-*) ;; restore-fail) echo migrate ;; *) echo "$1" ;; esac; }
# An extra launcher key: the stuck cases shorten the timeout they hit.
keys_of() { case $1 in stuck-quit) echo QUIT_TIMEOUT=5 ;; stuck-up) echo UP_TIMEOUT=8 ;; esac; }

# wait_for <seconds> <command...>: poll every second until the command succeeds.
wait_for() { local t=$1; shift
  for _ in $(seq 1 "$t"); do "$@" && return 0; sleep 1; done; return 1; }
gone() { ! pgrep -f "$1/Contents/MacOS/applet" >/dev/null; }
# One osascript per app: a script that names an app which is not running fails as a whole.
quit_both() { local id; for id in "$ID1" "$ID2"; do osascript -e "tell application id \"$id\" to quit" >/dev/null 2>&1; done; }
both_gone() { gone "$APP1" && gone "$APP2"; }

# snap <item paths...>: one line per entry, hashed; *.db by content without mono_clock.
snap() { local root
  for root in "$@"; do
    echo "== ${root#"$TH"}"
    [ -e "$root" ] || { echo ABSENT; continue; }
    (cd "$root" && find . -print | LC_ALL=C sort | while IFS= read -r p; do
      case $p in *-wal|*-shm) continue ;; esac
      if [ -d "$p" ]; then echo "$p/"
      elif [ ! -r "$p" ]; then echo "$p unreadable"
      elif [[ $p == *.db ]]; then echo "$p db $(sqlite3 -readonly "$p" .dump | grep -v mono_clock | shasum -a 256 | cut -c1-16)"
      else echo "$p $(shasum -a 256 < "$p" | cut -c1-16)"; fi
    done)
  done; }

fixtures() {
  mkdir -p "$D1" "$D2" "$W1/WebsiteData" "$W2/WebsiteData" "$APPS" "$FX" "$TH/ship183-art/app" "$C2" "$X2"
  local old='CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, cwd TEXT, provider_account_id TEXT);
CREATE TABLE monos (id TEXT PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE notes (id TEXT PRIMARY KEY, slug TEXT NOT NULL UNIQUE, source_session_id TEXT);
CREATE TABLE approvals (number INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE);
CREATE TABLE mono_transcripts (session_id TEXT PRIMARY KEY REFERENCES sessions(id));
CREATE TABLE mono_clock (id INTEGER PRIMARY KEY CHECK (id = 1), active_ms INTEGER NOT NULL, last_wall_ms INTEGER NOT NULL DEFAULT 0);
CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
INSERT INTO schema_migrations VALUES (1, 1);'
  sqlite3 "$D1/monocode.db" "PRAGMA journal_mode=WAL; $old
INSERT INTO mono_clock VALUES (1, 5000, 0);
INSERT INTO sessions VALUES ('s1-a', 'Space 1 work', '/tmp/made-up/proj-one', NULL), ('s1-b', 'Space 1 more', '/tmp/made-up/proj-one', NULL);
INSERT INTO monos VALUES ('m1-a', 'Mono One');
INSERT INTO notes VALUES ('n1-a', 'note-one', 's1-a');
INSERT INTO approvals (id) VALUES ('ap1-a');
INSERT INTO mono_transcripts VALUES ('s1-a');" >/dev/null
  sqlite3 "$D2/monocode.db" "PRAGMA journal_mode=WAL; $old
INSERT INTO mono_clock VALUES (1, 2000, 0);
INSERT INTO sessions VALUES ('s2-a', 'Space 2 work', '/tmp/made-up/proj-two', NULL), ('s2-b', 'Space 2 more', '/tmp/made-up/proj-two', NULL), ('s2-c', 'Space 2 shared', '/tmp/made-up/proj-one', NULL);
INSERT INTO monos VALUES ('m2-a', 'Mono Two'), ('m2-b', 'Mono Three');
INSERT INTO notes VALUES ('n2-a', 'note-two', 's2-a');
INSERT INTO approvals (id) VALUES ('ap2-a'), ('ap2-b');
INSERT INTO mono_transcripts VALUES ('s2-a'), ('s2-b');" >/dev/null
  mkdir -p "$D1/attachments" "$D2/attachments" "$D2/monos/m2-a" "$D2/checkpoints/s2-a"
  echo "space one file" > "$D1/attachments/one.txt"
  echo "space two file" > "$D2/attachments/two.txt"
  echo "mono two memory" > "$D2/monos/m2-a/memory.md"
  echo "checkpoint" > "$D2/checkpoints/s2-a/cp1.json"
  echo "made-up" > "$W1/WebsiteData/marker"; echo "made-up" > "$W2/WebsiteData/marker"
  echo '{"made":"up"}' > "$C2/settings.json"; echo 'model = "made-up"' > "$X2/config.toml"

  # The made-up Spaces migration the "new app" runs at start (ADE-184 shape the importer writes to).
  cat > "$FX/migrate.sql" <<'SQL'
ALTER TABLE sessions ADD COLUMN space_id TEXT NOT NULL DEFAULT 'space-1';
ALTER TABLE monos ADD COLUMN space_id TEXT NOT NULL DEFAULT 'space-1';
CREATE TABLE spaces (id TEXT PRIMARY KEY, name TEXT NOT NULL, color TEXT, created_at INTEGER NOT NULL);
CREATE TABLE provider_accounts (provider TEXT NOT NULL, id TEXT NOT NULL, label TEXT, created_at INTEGER NOT NULL, PRIMARY KEY (provider, id));
CREATE TABLE space_accounts (space_id TEXT NOT NULL, provider TEXT NOT NULL, account_id TEXT NOT NULL, PRIMARY KEY (space_id, provider));
CREATE TABLE space_projects (project_path TEXT PRIMARY KEY, space_id TEXT NOT NULL);
INSERT INTO spaces VALUES ('space-1', 'Space 1', NULL, 1);
INSERT INTO provider_accounts VALUES ('claude', 'default', 'Default', 1), ('codex', 'default', 'Default', 1);
INSERT INTO schema_migrations VALUES (2, 2);
SQL
  # hook <data dir> <migrate 0|1> start|tick: the stand-ins tick mono_clock.last_wall_ms every second
  # like the app; the "new app" stand-in runs the made-up Spaces migration when it starts.
  cat > "$FX/hook.sh" <<'SH'
#!/bin/bash
db="$1/monocode.db"
if [ "$3" = start ] && [ "$2" = 1 ] && [ -z "$(sqlite3 "$db" "SELECT name FROM sqlite_master WHERE name='spaces'")" ]; then
  sqlite3 "$db" < "$(dirname "$0")/migrate.sql"
fi
sqlite3 "$db" "UPDATE mono_clock SET last_wall_ms = $(python3 -c 'import time; print(int(time.time()*1000))'), active_ms = active_ms + 1000"
SH
  applet "$APPS/MonoCode GigaEval.app" "$ID1" 0.13.0-old "$D1" 0
  applet "$APPS/MonoCode 2 GigaEval.app" "$ID2" 0.13.0-old "$D2" 0
  # Like the real MonoCode 2: its own LSEnvironment points its logins at Space 2's folders (allowed vars).
  /usr/libexec/PlistBuddy -c "Add :LSEnvironment dict" -c "Add :LSEnvironment:CLAUDE_SECURESTORAGE_CONFIG_DIR string $C2" \
    -c "Add :LSEnvironment:CODEX_HOME string $X2" "$APP2/Contents/Info.plist"
  codesign --force --deep --sign - "$APP2" 2>/dev/null
  applet "$NEW" "$ID1" 0.14.0-new "$D1" 1 "$1"
}
# applet <out .app> <bundle id> <version> <data dir> <migrate> [stuck-quit|stuck-up]
# stuck-quit: a quit handler that never quits; stuck-up: never runs the hook, so the clock never ticks.
applet() { local hook="/bin/bash '$FX/hook.sh' '$4' $5" extra=""
  [ "${6:-}" = stuck-up ] && hook=true
  [ "${6:-}" = stuck-quit ] && extra=$'\non quit\nend quit'
  osacompile -s -o "$1" -e "on run
  do shell script \"$hook start\"
end run
on idle
  do shell script \"$hook tick\"
  return 1
end idle$extra"
  /usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $2" "$1/Contents/Info.plist" 2>/dev/null \
    || /usr/libexec/PlistBuddy -c "Add :CFBundleIdentifier string $2" "$1/Contents/Info.plist"
  /usr/libexec/PlistBuddy -c "Add :CFBundleShortVersionString string $3" "$1/Contents/Info.plist" 2>/dev/null \
    || /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $3" "$1/Contents/Info.plist"
  /usr/libexec/PlistBuddy -c "Add :LSUIElement bool true" "$1/Contents/Info.plist"
  codesign --force --deep --sign - "$1" 2>/dev/null
}

# run_job: the job as launchd starts it (EnvironmentVariables only), in the background.
run_job() { env -i HOME="$TH" USER="$(id -un)" LOGNAME="$(id -un)" SHELL=/bin/zsh PATH=/usr/bin:/bin:/usr/sbin:/sbin \
  /bin/bash "$RUN/cutover-ade183.sh" "$RUN/config.sh" >>"$RUN/launchd.out" 2>&1; }

check() { if "${@:2}"; then echo "  ok   $1"; else echo "  FAIL $1"; BAD=1; fi; }
same() { cmp -s "$1" "$2" || { diff "$1" "$2" | head -20 | sed 's/^/       /'; return 1; }; }
results() { grep -c '\] result=' "$RUN/log.txt"; }
partly() { [ -d "$1" ] && [ ! -e "$1/$2" ]; }
none_of() { local p; for p in "$@"; do [ ! -e "$p" ] || return 1; done; }
drop_d1() { awk -v h="== ${D1#"$TH"}" '/^== /{on=($0!=h)} on' "$1"; }
version() { plutil -extract CFBundleShortVersionString raw "$1/Contents/Info.plist" 2>/dev/null; }

one_case() { local case=$1 expect fail_at job key
  expect=$(expect_of "$case") || { echo "unknown case $case" >&2; return 1; }
  fail_at=$(fail_at_of "$case"); key=$(keys_of "$case")
  TH=$(mktemp -d "/tmp/cutover183-dry-$case.XXXXXX"); BAD=0
  local tag; tag=$(basename "$TH" | tr -cd '[:alnum:]' | tr '[:upper:]' '[:lower:]')
  ID1=com.example.ade183dry.$tag.one; ID2=com.example.ade183dry.$tag.two
  SUP="$TH/Library/Application Support"; D1="$SUP/$ID1"; D2="$SUP/$ID2"
  W1="$TH/Library/WebKit/$ID1"; W2="$TH/Library/WebKit/$ID2"; C2="$TH/.claude-2"; X2="$TH/.codex-2"
  APPS="$TH/Applications"; FX="$TH/fixture"; APP1="$APPS/MonoCode GigaEval.app"; APP2="$APPS/MonoCode 2 GigaEval.app"
  NEW="$TH/ship183-art/app/MonoCode GigaEval.app"
  ITEMS=("$APP1" "$APP2" "$D1" "$D2" "$W1" "$W2" "$C2" "$X2")
  echo "== case $case ($TH)"
  fixtures "$case"
  if [ "$case" = backup-partial ]; then echo "made-up" > "$C2/zz-unreadable"; chmod 000 "$C2/zz-unreadable"; fi

  open -a "$APP1"; open -a "$APP2"
  RUN=$(bash "$HERE/cutover-ade183-launch.sh" LOAD=0 LABEL="com.tyler.cutover-ade183-dryrun-$tag" RUN_ROOT="$TH/MonoCode-cutover" \
    HOME_DIR="$TH" APP1="$APP1" ID1="$ID1" DATA1="$D1" WEBKIT1="$W1" APP2="$APP2" ID2="$ID2" DATA2="$D2" WEBKIT2="$W2" \
    CLAUDE2="$C2" CODEX2="$X2" NEW_APP="$NEW" PYTHON="$(command -v python3)" REAL=0 POLL=3 ${fail_at:+FAIL_AT=$fail_at} \
    ${key:+"$key"}) \
    || { echo "  FAIL launcher"; return 1; }
  run_job & job=$!
  # Tyler's quit, once the job waits for it; then the "before" picture, while the job polls twice.
  check "job reaches its wait step" wait_for 60 grep -qs '\[wait\] waiting' "$RUN/log.txt"
  quit_both
  check "stand-ins quit" wait_for 30 both_gone
  snap "${ITEMS[@]}" > "$TH/before.txt"
  if [ "$case" = restore-fail ]; then
    check "backup done" wait_for 60 grep -qs 'backup complete' "$RUN/log.txt"
    chmod 000 "$RUN/backup/data1/attachments/one.txt"
  fi
  check "result line within 300 s" wait_for 300 grep -qs '\] result=' "$RUN/log.txt"
  wait "$job"
  snap "${ITEMS[@]}" > "$TH/after.txt"
  echo "  $(grep '\] result=' "$RUN/log.txt" | sed 's/^.*\] //')"

  check "result matches /$expect/" grep -Eq "\] $expect" "$RUN/log.txt"
  check "exactly one result line" [ "$(results)" = 1 ]
  check "plist deleted" [ ! -e "$RUN/com.tyler.cutover-ade183-dryrun-$tag.plist" ]
  case $case in
    restore-fail)
      check "no app reopened" both_gone
      check "every item but data1 == before" same <(drop_d1 "$TH/before.txt") <(drop_d1 "$TH/after.txt")
      check "data1 left as it is (still migrated)" [ "$(sqlite3 -readonly "$D1/monocode.db" "SELECT count(*) FROM sqlite_master WHERE name='spaces'")" = 1 ]
      ;;
    stuck-quit|stuck-up)
      check "the job killed its own stuck instance" grep -Eqs '\] (TERM to MonoCode GigaEval.app pid [0-9]+ \(opened by this job|MonoCode GigaEval.app pid [0-9]+ is gone)' "$RUN/log.txt"
      check "every live item == before (restored)" same "$TH/before.txt" "$TH/after.txt"
      check "old app back in place" [ "$(version "$APP1")" = 0.13.0-old ]
      ;;
    ok)
      check "Space 2 == before" same <(awk -v h="== ${D2#"$TH"}" '/^== /{on=($0==h)} on' "$TH/before.txt") <(snap "$D2")
      check "Space 1 has the import (3 monos)" [ "$(sqlite3 -readonly "$D1/monocode.db" 'SELECT count(*) FROM monos')" = 3 ]
      check "new app installed" [ "$(version "$APP1")" = 0.14.0-new ]
      snap "$RUN/backup" > "$TH/backup-before-rerun.txt"
      run_job
      check "second start on the same run folder refused" grep -q 'REFUSED: .* already has a result line' "$RUN/log.txt"
      check "still one result line after the second start" [ "$(results)" = 1 ]
      check "backup untouched by the second start" same "$TH/backup-before-rerun.txt" <(snap "$RUN/backup")
      ;;
    backup-partial)
      check "every live item == before" same "$TH/before.txt" "$TH/after.txt"
      check "claude2 backed up partly" partly "$RUN/backup/claude2" zz-unreadable
      check "codex2 not backed up" [ ! -e "$RUN/backup/codex2" ]
      check "no claude2/codex2 verified marker" none_of "$RUN/backup/.verified/claude2" "$RUN/backup/.verified/codex2"
      check "nothing moved to failed-state" [ ! -e "$RUN/failed-state" ]
      ;;
    backup)
      check "every live item == before" same "$TH/before.txt" "$TH/after.txt"
      check "nothing moved to failed-state" [ ! -e "$RUN/failed-state" ]
      ;;
    *)
      check "every live item == before (restored)" same "$TH/before.txt" "$TH/after.txt"
      check "old app back in place" [ "$(version "$APP1")" = 0.13.0-old ]
      check "failed state kept" [ -d "$RUN/failed-state/data1" ]
      ;;
  esac
  # Quit whatever stand-in the job left open (the relaunched or reopened apps); made-up ids only.
  quit_both
  check "no stand-in left running" wait_for 30 both_gone
  # A stand-in that ignores quit (a failed case) is killed by its pid under this temp HOME only.
  pgrep -f "${TH#/private}/" | while read -r p; do kill -KILL "$p"; done
  [ -e "$C2/zz-unreadable" ] && chmod 600 "$C2/zz-unreadable"
  [ -e "$RUN/backup/data1/attachments/one.txt" ] && chmod 600 "$RUN/backup/data1/attachments/one.txt"
  [ "$BAD" = 0 ] && echo "  PASS $case" || echo "  CASE FAILED $case (log $RUN/log.txt)"
  return "$BAD"
}

failed=()
for c in "$@"; do one_case "$c" || failed+=("$c"); done
[ ${#failed[@]} = 0 ] && { echo "ALL PASS: $*"; exit 0; }
echo "FAILED: ${failed[*]}"; exit 1
