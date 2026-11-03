#!/bin/bash
# Isolated dry run of the ADE-183 cut-over job: made-up data under a temp HOME, stand-in apps
# (stay-open AppleScript applets with GigaEval bundle ids), a dryrun launchd label. Never real data.
#
#   cutover-ade183-dryrun.sh <temp home under /tmp> [FAIL_AT=<step>]
#
# The stand-ins tick mono_clock.last_wall_ms every second like the app; the "new app" stand-in runs
# a made-up Spaces migration when it starts. This harness quits both stand-ins itself (Tyler's
# quit), then waits for the job's result line. Prints the run folder.
set -euo pipefail
TH=${1:?temp home}; shift
case "$TH" in /tmp/*|/private/tmp/*) ;; *) echo "temp home must be under /tmp" >&2; exit 2;; esac
[ -e "$TH" ] && { echo "$TH exists; use a new folder" >&2; exit 2; }
HERE=$(cd "$(dirname "$0")/.." && pwd)
ID1=com.monocode.desktop.gigaeval; ID2=com.monocode.second.gigaeval
SUP="$TH/Library/Application Support"; D1="$SUP/$ID1"; D2="$SUP/$ID2"
W1="$TH/Library/WebKit/$ID1"; W2="$TH/Library/WebKit/$ID2"
APPS="$TH/Applications"; FX="$TH/fixture"
mkdir -p "$D1" "$D2" "$W1/WebsiteData" "$W2/WebsiteData" "$APPS" "$FX" "$TH/ship183-art/app" "$TH/.claude-2" "$TH/.codex-2"

OLD='CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, cwd TEXT, provider_account_id TEXT);
CREATE TABLE monos (id TEXT PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE notes (id TEXT PRIMARY KEY, slug TEXT NOT NULL UNIQUE, source_session_id TEXT);
CREATE TABLE approvals (number INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE);
CREATE TABLE mono_transcripts (session_id TEXT PRIMARY KEY REFERENCES sessions(id));
CREATE TABLE mono_clock (id INTEGER PRIMARY KEY CHECK (id = 1), active_ms INTEGER NOT NULL, last_wall_ms INTEGER NOT NULL DEFAULT 0);
CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
INSERT INTO schema_migrations VALUES (1, 1);'
sqlite3 "$D1/monocode.db" "PRAGMA journal_mode=WAL; $OLD
INSERT INTO mono_clock VALUES (1, 5000, 0);
INSERT INTO sessions VALUES ('s1-a', 'Space 1 work', '/tmp/made-up/proj-one', NULL), ('s1-b', 'Space 1 more', '/tmp/made-up/proj-one', NULL);
INSERT INTO monos VALUES ('m1-a', 'Mono One');
INSERT INTO notes VALUES ('n1-a', 'note-one', 's1-a');
INSERT INTO approvals (id) VALUES ('ap1-a');
INSERT INTO mono_transcripts VALUES ('s1-a');" >/dev/null
sqlite3 "$D2/monocode.db" "PRAGMA journal_mode=WAL; $OLD
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
echo '{"made":"up"}' > "$TH/.claude-2/settings.json"; echo 'model = "made-up"' > "$TH/.codex-2/config.toml"

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
# hook <data dir> <migrate 0|1> start|tick
cat > "$FX/hook.sh" <<'SH'
#!/bin/bash
db="$1/monocode.db"
if [ "$3" = start ] && [ "$2" = 1 ] && [ -z "$(sqlite3 "$db" "SELECT name FROM sqlite_master WHERE name='spaces'")" ]; then
  sqlite3 "$db" < "$(dirname "$0")/migrate.sql"
fi
sqlite3 "$db" "UPDATE mono_clock SET last_wall_ms = $(python3 -c 'import time; print(int(time.time()*1000))'), active_ms = active_ms + 1000"
SH
# applet <out .app> <bundle id> <version> <data dir> <migrate>
applet() {
  osacompile -s -o "$1" -e "on run
  do shell script \"/bin/bash '$FX/hook.sh' '$4' $5 start\"
end run
on idle
  do shell script \"/bin/bash '$FX/hook.sh' '$4' $5 tick\"
  return 1
end idle"
  /usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $2" "$1/Contents/Info.plist" 2>/dev/null \
    || /usr/libexec/PlistBuddy -c "Add :CFBundleIdentifier string $2" "$1/Contents/Info.plist"
  /usr/libexec/PlistBuddy -c "Add :CFBundleShortVersionString string $3" "$1/Contents/Info.plist" 2>/dev/null \
    || /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $3" "$1/Contents/Info.plist"
  /usr/libexec/PlistBuddy -c "Add :LSUIElement bool true" "$1/Contents/Info.plist"
  codesign --force --deep --sign - "$1" 2>/dev/null
}
applet "$APPS/MonoCode GigaEval.app" "$ID1" 0.13.0-old "$D1" 0
applet "$APPS/MonoCode 2 GigaEval.app" "$ID2" 0.13.0-old "$D2" 0
# Like the real MonoCode 2: its own LSEnvironment points its logins at Space 2's folders (allowed vars).
P2="$APPS/MonoCode 2 GigaEval.app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Add :LSEnvironment dict" -c "Add :LSEnvironment:CLAUDE_SECURESTORAGE_CONFIG_DIR string $TH/.claude-2" \
  -c "Add :LSEnvironment:CODEX_HOME string $TH/.codex-2" "$P2"
codesign --force --deep --sign - "$APPS/MonoCode 2 GigaEval.app" 2>/dev/null
applet "$TH/ship183-art/app/MonoCode GigaEval.app" "$ID1" 0.14.0-new "$D1" 1

open -a "$APPS/MonoCode GigaEval.app"; open -a "$APPS/MonoCode 2 GigaEval.app"
LABEL="com.tyler.cutover-ade183-dryrun-$(date +%H%M%S)"
RUN=$(bash "$HERE/cutover-ade183-launch.sh" LABEL="$LABEL" RUN_ROOT="$TH/MonoCode-cutover" HOME_DIR="$TH" \
  APP1="$APPS/MonoCode GigaEval.app" ID1="$ID1" DATA1="$D1" WEBKIT1="$W1" \
  APP2="$APPS/MonoCode 2 GigaEval.app" ID2="$ID2" DATA2="$D2" WEBKIT2="$W2" \
  CLAUDE2="$TH/.claude-2" CODEX2="$TH/.codex-2" NEW_APP="$TH/ship183-art/app/MonoCode GigaEval.app" \
  PYTHON="$(command -v python3)" REAL=0 POLL=2 "$@")
echo "run folder $RUN (label $LABEL)"
# Wait until the job is in its wait step, then quit both stand-ins like Tyler does.
until grep -q '\[wait\] waiting' "$RUN/log.txt" 2>/dev/null; do sleep 1; done
osascript -e "tell application id \"$ID1\" to quit"; osascript -e "tell application id \"$ID2\" to quit"
echo "quit both stand-ins; waiting for the result line"
until grep -q 'result=' "$RUN/log.txt" 2>/dev/null; do sleep 2; done
grep 'result=' "$RUN/log.txt"
# Clean up: quit whatever stand-in the job left open (the relaunched or reopened apps).
for id in "$ID1" "$ID2"; do osascript -e "tell application id \"$id\" to quit" >/dev/null 2>&1 || true; done
