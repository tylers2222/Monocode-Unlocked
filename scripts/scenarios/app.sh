#!/usr/bin/env bash
# Real-app scenarios on a GigaEval instance with the fake-clock build and the
# puppet agent (scripts/scenarios/puppet/puppet.mjs) as fx. No model runs.
#
#   app.sh build [checkout]   build the instance (under heavy-lock.sh)
#   app.sh s0 [runs]          S0 through the real app, `runs` times (default 1)
#   app.sh d1                 D1: the agent sandbox on, off, and on without srt
#   app.sh build-two          the same instance with a second window at launch
#   app.sh ade89 MODE [runs]  ADE-89: a waiting completion result survives
#                             two exits with no quit save (MODE kill or
#                             term, or two: kill with two windows, after
#                             build-two) and arrives once
#
# GIGA_EVAL_OUT and GIGA_EVAL_HOME default to their own /tmp folders, apart
# from other GigaEval runs. The installed apps and their data are never used.
export GIGA_EVAL_OUT="${GIGA_EVAL_OUT:-/tmp/scenario-app}"
export GIGA_EVAL_HOME="${GIGA_EVAL_HOME:-/tmp/scenario-app-home}"
SCENARIOS="$(cd "$(dirname "$0")" && pwd)"
source "$SCENARIOS/../giga-eval/lib.sh"
INSTANCE="$EVAL_DIR/instance.sh"
APP_DATA="$EVAL_HOME/Library/Application Support/$EVAL_ID"
DB="$APP_DATA/monocode.db"
CLOCK="$APP_DATA/fake-clock.json"
PUPPET="$EVAL_HOME/.puppet"

# A query on a .backup copy of the instance DB, never the live file.
q() {
  local copy="$EVAL_OUT/read.db"
  rm -f "$copy"
  sqlite3 "file:$DB?mode=ro" ".backup '$copy'" && sqlite3 "$copy" "$1"
}

# wait_until <seconds> <what> <command...>: until the command succeeds, or
# the run stops with "timed out".
wait_until() {
  local secs="$1" what="$2" _
  shift 2
  for _ in $(seq 1 "$secs"); do
    "$@" && return 0
    sleep 1
  done
  die "timed out after $secs s: $what"
}

# A query on the DB copy that prints 1.
holds() { [[ "$(q "$1" 2>/dev/null)" == 1 ]]; }

# clock <uptime_ms> <wall_ms>: one atomic write; monotonic moves with uptime
# (no sleep) and boot time stays.
clock_write() {
  jq -n --argjson up "$1" --argjson wall "$2" --argjson boot "$BOOT_MS" \
    '{uptime_ns: ($up * 1000000), monotonic_ns: ($up * 1000000), wall_ms: $wall, boot_wall_ms: $boot}' >"$CLOCK.tmp"
  mv "$CLOCK.tmp" "$CLOCK"
}

# Awake time while the app runs, in steps under the 30 s tick cap, one
# 1 s tick per step.
advance() {
  local left="$1" step want
  want="$(active_ms)"
  while ((left > 0)); do
    step=$((left < 20000 ? left : 20000))
    UPTIME=$((UPTIME + step)); WALL=$((WALL + step)); left=$((left - step))
    want=$((want + step))
    clock_write "$UPTIME" "$WALL"
    # One write per tick: two writes in one tick read as one jump, which
    # the app caps at 30 s. Each step is 5 s or more, so the app stores it.
    wait_until 30 "the app stores active_ms $want" \
      holds "SELECT active_ms = $want FROM mono_clock WHERE id = 1"
  done
}

active_ms() { q "SELECT active_ms FROM mono_clock WHERE id = 1"; }

kill9() {
  local pid; pid="$(target_pid instance)"
  [[ -n "$pid" ]] || die "instance not running"
  local kids; kids="$(descendants "$pid")"
  kill -KILL "$pid" $kids 2>/dev/null || true
  rm -f "$EVAL_OUT/instance.pid"
  log "kill -9 pid $pid and $(echo "$kids" | grep -c . || true) descendants"
}

expect() {
  local what="$1" got="$2" want="$3"
  if [[ "$got" == "$want" ]]; then echo "  ok   $what: $got"; else echo "  FAIL $what: $got, expected $want"; FAILED=1; fi
}

# fresh_pair <scenario>: a fresh temp HOME with the puppet as fx, launched
# once so the DB is created and migrated, then a Giga `<scenario>-giga` and a
# Mono `<scenario>-mono` on the scratch project, each with an fx chat.
fresh_pair() {
  "$INSTANCE" stop
  "$INSTANCE" home --fresh
  mkdir -p "$APP_DATA"
  UPTIME=0; WALL="$(date +%s)000"; BOOT_MS=$((WALL - 3600000))
  clock_write "$UPTIME" "$WALL"
  install_puppet
  "$INSTANCE" start
  wait_until 30 "the first launch writes mono_clock" holds "SELECT count(*) = 1 FROM mono_clock"
  "$INSTANCE" stop
  local id="$1" name giga_chat mono_chat scratch="$EVAL_HOME/$SCRATCH_NAME" now
  name="$(tr '[:lower:]' '[:upper:]' <<<"$1")"; now="$(date +%s)000"
  giga_chat="$(uuidgen | tr '[:upper:]' '[:lower:]')"; mono_chat="$(uuidgen | tr '[:upper:]' '[:lower:]')"
  sqlite3 "$DB" <<SQL
INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at) VALUES
  ('$id-giga', '$name Giga', 'giga', json_object('name', '$name Giga', 'sessionId', '$giga_chat',
    'gigaSlug', '$id-giga', 'projects', json_array('$scratch')), 0, $now, $now),
  ('$id-mono', '$name Mono', 'mono', json_object('name', '$name Mono', 'sessionId', '$mono_chat',
    'projects', json_array('$scratch')), 1, $now, $now);
INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at) VALUES
  ('$giga_chat', '$scratch', 'fx', 'fx:zai/glm-5.2-fast', 'auto', '$name Giga', $now, $now),
  ('$mono_chat', '$scratch', 'fx', 'fx:zai/glm-5.2-fast', 'auto', '$name Mono', $now, $now);
SQL
}

# request_habit <mono> <name> <instructions>: the Mono's one habit, which runs
# at the next start.
request_habit() {
  local now; now="$(date +%s)000"
  mkdir -p "$APP_DATA/monos/$1"
  jq -n --argjson now "$now" --arg id "$1-habit" --arg name "$2" --arg text "$3" '[{id: $id, name: $name, instructions: $text,
    schedule: {scheduleKind: "daily"}, enabled: true, createdAt: $now, nextRunAt: ($now + 86400000), runRequested: true}]' \
    >"$APP_DATA/monos/$1/habits.json"
}

install_puppet() {
  mkdir -p "$EVAL_HOME/.local/bin" "$PUPPET"
  cp "$SCENARIOS/puppet/puppet.mjs" "$EVAL_HOME/.local/bin/fx"
  chmod +x "$EVAL_HOME/.local/bin/fx"
  : >"$PUPPET/log.jsonl"
  # The answer rule goes first: a prompt carrying the answer may quote the ask.
  jq -n --arg app "$(target_bin instance)" '{app: $app, rules: [
    {match: "S0-HABIT", calls: [{tool: "send_message", arguments:
      {message: "S0-Q Is CI green?", send_to: "s0-mono", wanting_reply: false, intent: "new"}}], reply: "Asked."},
    {match: "S0-A", reply: "Noted."},
    {match: "S0-Q", calls: [{tool: "send_message", arguments:
      {message: "S0-A Yes, CI is green.", send_to: "s0-giga", wanting_reply: false, intent: "new"}}], reply: "Answered."}]}' >"$PUPPET/script.json"
}

prompts() { jq -s --arg rule "$1" '[.[] | select(.rule == $rule)] | length' "$PUPPET/log.jsonl"; }
saw() { (($(prompts "$1") > 0)); }

run_s0() {
  local run="$1"
  FAILED=0
  log "S0 run $run"
  fresh_pair s0
  request_habit s0-giga "Ask about CI" "S0-HABIT: ask S0 Mono if CI is green."
  "$INSTANCE" start
  wait_until 90 "the ask and the answer are delivered" \
    holds "SELECT count(*) = 2 FROM mono_messages WHERE delivered_at IS NOT NULL"
  wait_until 30 "the Giga's puppet gets the answer" saw S0-A
  echo "run $run readings:"
  expect "messages written" "$(q "SELECT count(*) FROM mono_messages")" 2
  local a0 a1 a2 a3
  a0="$(active_ms)"
  advance 60000
  a1="$(active_ms)"
  expect "active clock after +60 s" "$((a1 - a0))" 60000
  kill9
  WALL=$((WALL + 30000)); clock_write "$UPTIME" "$WALL"
  "$INSTANCE" start
  wait_until 30 "the restart logs app_down" holds "SELECT count(*) = 2 FROM mono_downtime WHERE cause = 'app_down'"
  a2="$(active_ms)"
  expect "active clock after kill -9 and restart" "$a2" "$a1"
  expect "app_down rows" "$(q "SELECT count(*) FROM mono_downtime WHERE cause = 'app_down'")" 2
  advance 10000
  a3="$(active_ms)"
  expect "active clock after +10 s more" "$((a3 - a2))" 10000
  expect "Giga habit prompts" "$(prompts S0-HABIT)" 1
  expect "Mono prompts with the ask" "$(prompts S0-Q)" 1
  expect "Giga prompts with the answer" "$(prompts S0-A)" 1
  expect "steers" "$(jq -s '[.[] | select(.steer == true)] | length' "$PUPPET/log.jsonl")" 0
  expect "tool calls that failed" "$(jq -s '[.[] | select(.tool and .result.ok != true)] | length' "$PUPPET/log.jsonl")" 0
  echo "  clock: a0=$a0 a1=$a1 a2=$a2 a3=$a3"
  q "SELECT '  message ' || from_mono_id || ' -> ' || coalesce(to_mono_id, to_session_id) || ' ' || status
       || ' delivered ' || (delivered_at IS NOT NULL) FROM mono_messages ORDER BY created_at"
  q "SELECT '  downtime ' || cause || ' ' || (end_wall_ms - start_wall_ms) || ' ms at active ' || active_ms FROM mono_downtime ORDER BY id"
  "$INSTANCE" stop
  return "$FAILED"
}

SANDBOX_ON="$APP_DATA/agent-sandbox-on"
SANDBOX_SETTINGS="$APP_DATA/agent-sandbox.json"
# A stand-in for the live MonoCode 2 data folder, next to the instance's own.
SECOND_DB="$(dirname "$APP_DATA")/com.monocode.second/monocode.db"
SRT_PIN=0.0.81
SRT_PREFIX="$EVAL_OUT/srt-$SRT_PIN"
SRT_MODULES="$SRT_PREFIX/node_modules"

# srt at the pinned version, under the temp HOME's ~/.local/bin, where the
# app's search PATH finds it.
install_srt() {
  [[ -x "$SRT_MODULES/.bin/srt" ]] \
    || npm install --prefix "$SRT_PREFIX" --no-audit --no-fund "@anthropic-ai/sandbox-runtime@$SRT_PIN" >/dev/null
  ln -sf "$SRT_MODULES/.bin/srt" "$EVAL_HOME/.local/bin/srt"
}

# d1_phase <name>: the Giga's habit runs once more on the next start, and
# its puppet runs every probe in its project folder.
d1_phase() {
  : >"$PUPPET/log.jsonl"
  request_habit d1-giga "Probe" "D1-HABIT: probe the sandbox."
  jq -n --arg app "$(target_bin instance)" --arg db "$DB" --arg second "$SECOND_DB" --arg on "$SANDBOX_ON" --arg srt "$SRT_MODULES" --arg phase "$1" '{app: $app, rules: [
    {match: "D1-HABIT", exec: [
      ["sqlite3", $db, ".tables"],
      ["sqlite3", $second, "select v from s"],
      ["python3", "-c", "import sys; print(open(sys.argv[1], \"rb\").read()[:15].decode())", $second],
      ["cat", $db],
      ["node", "-e", "try { require(\"fs\").readFileSync(process.argv[1]); console.log(\"read\") } catch (e) { console.log(e.message); process.exit(1) }", $db],
      ["sh", "-c", "echo d1-\($phase) > d1-own.txt && cat d1-own.txt"],
      ["sh", "-c", "mkdir -p ~/Library/LaunchAgents && echo x > ~/Library/LaunchAgents/d1-probe.plist"],
      ["sh", "-c", "echo x > ~/.local/bin/d1-probe"],
      ["sh", "-c", "echo x > \"$1\"/d1-probe", "sh", $srt],
      ["sh", "-c", "mkdir -p /tmp/claude-$(id -u)/d1-\($phase) && echo made"],
      ["sh", "-c", "echo sandbox=${SANDBOX_RUNTIME:-none}"],
      ["rm", $on]],
     calls: [{tool: "send_message", arguments:
      {message: "D1-Q The probes ran.", send_to: "d1-mono", wanting_reply: false, intent: "new"}}], reply: "Probed."},
    {match: "D1-Q", reply: "Noted."}]}' >"$PUPPET/script.json"
}

# probe <part of the argv> <jq on its log line>
probe() {
  jq -rs --arg p "$1" "[.[] | select(.exec and (.exec | join(\" \") | contains(\$p)))] | last | $2" "$PUPPET/log.jsonl"
}
denied() { probe "$1" '.code != 0 and (.stdout + .stderr | test("[Oo]peration not permitted|authorization denied"))'; }
habit_failed() { jq -e '.[0].lastOutcome == "failed"' "$APP_DATA/monos/d1-giga/habits.json" >/dev/null; }

run_d1() {
  FAILED=0
  log "D1"
  fresh_pair d1
  install_srt
  mkdir -p "$(dirname "$SECOND_DB")"
  sqlite3 "$SECOND_DB" "CREATE TABLE s (v TEXT); INSERT INTO s VALUES ('SECRET-SECOND')"

  echo "on:"
  touch "$SANDBOX_ON"
  # The temp HOME and srt are under /tmp, which the app's defaults allow, so
  # only the deny lists keep ~/.local/bin and srt's code out. (The temp HOME's
  # ~/.config and ~/.gitconfig are links to the real ones: no probe writes there.)
  d1_phase on
  "$INSTANCE" start
  wait_until 90 "the Mono gets the Giga's message" saw D1-Q
  expect "sqlite3 .tables on the DB is denied" "$(denied "sqlite3 $DB")" true
  expect "sqlite3 on another instance's DB is denied" "$(denied "sqlite3 $SECOND_DB")" true
  expect "a python open of another instance's DB is denied" "$(denied "python3")" true
  expect "cat of the DB is denied" "$(denied "cat $DB")" true
  expect "a node read of the DB is denied" "$(denied 'readFileSync')" true
  expect "rm of the setting is denied" "$(denied "rm $SANDBOX_ON")" true
  expect "the setting is still there" "$([[ -e "$SANDBOX_ON" ]] && echo yes)" yes
  expect "a write to ~/Library/LaunchAgents is denied" "$(denied LaunchAgents)" true
  expect "a write to ~/.local/bin, under an allowed /tmp, is denied" "$(denied .local/bin/d1-probe)" true
  expect "a write into srt's own code is denied" "$(denied "$SRT_MODULES")" true
  expect "a mkdir under /tmp, as Claude Code does" "$(probe /tmp/claude- '"\(.code) \(.stdout | rtrimstr("\n"))"')" "0 made"
  expect "the worktree write" "$(probe d1-own '"\(.code) \(.stdout | rtrimstr("\n"))"')" "0 d1-on"
  expect "the puppet runs under srt" "$(probe SANDBOX_RUNTIME '.stdout | rtrimstr("\n")')" "sandbox=1"
  expect "tool calls that failed" "$(jq -s '[.[] | select(.tool and .result.ok != true)] | length' "$PUPPET/log.jsonl")" 0
  cp "$PUPPET/log.jsonl" "$EVAL_OUT/d1-on.puppet.jsonl"
  "$INSTANCE" stop

  echo "off:"
  rm -f "$SANDBOX_ON" "$SANDBOX_SETTINGS"
  d1_phase off
  "$INSTANCE" start
  wait_until 90 "the Mono gets the Giga's message" saw D1-Q
  expect "sqlite3 .tables on the DB" "$(probe "sqlite3 $DB" '"\(.code) \(.stdout | test("mono_clock"))"')" "0 true"
  expect "sqlite3 on another instance's DB" "$(probe "sqlite3 $SECOND_DB" '"\(.code) \(.stdout | rtrimstr("\n"))"')" "0 SECRET-SECOND"
  expect "a python open of another instance's DB" "$(probe python3 '"\(.code) \(.stdout | rtrimstr("\n"))"')" "0 SQLite format 3"
  expect "cat of the DB" "$(probe "cat $DB" '"\(.code) \(.bytes > 0)"')" "0 true"
  expect "a node read of the DB" "$(probe readFileSync '"\(.code) \(.stdout | rtrimstr("\n"))"')" "0 read"
  expect "the worktree write" "$(probe d1-own '"\(.code) \(.stdout | rtrimstr("\n"))"')" "0 d1-off"
  expect "the puppet runs without srt" "$(probe SANDBOX_RUNTIME '.stdout | rtrimstr("\n")')" "sandbox=none"
  expect "tool calls that failed" "$(jq -s '[.[] | select(.tool and .result.ok != true)] | length' "$PUPPET/log.jsonl")" 0
  expect "no settings file is written" "$([[ -e "$SANDBOX_SETTINGS" ]] && echo written || echo none)" none
  cp "$PUPPET/log.jsonl" "$EVAL_OUT/d1-off.puppet.jsonl"
  "$INSTANCE" stop

  echo "on without srt:"
  rm "$EVAL_HOME/.local/bin/srt"
  ! command -v srt >/dev/null && ! HOME="$EVAL_HOME" "$SHELL" -lic 'command -v srt' >/dev/null 2>&1 \
    || die "an srt outside the temp HOME would stand in for the missing one"
  touch "$SANDBOX_ON"
  d1_phase missing
  "$INSTANCE" start
  wait_until 60 "the habit run fails" habit_failed
  expect "the run's error" "$(jq -r '.[0].lastError | test("srt is not installed")' "$APP_DATA/monos/d1-giga/habits.json")" true
  expect "prompts the puppet got" "$(prompts D1-HABIT)" 0
  "$INSTANCE" stop
  return "$FAILED"
}

# The notice rule goes first: the notice quotes the session's prompt. The
# Mono's agent holds its ADE89-BUSY turn, so a notice waits in its queue; the
# work session's second Continue ends once that turn has started.
NOTICE="MonoCode completion notification"
ade89_script() {
  jq -n --arg app "$(target_bin instance)" --arg notice "$NOTICE" --arg log "$PUPPET/log.jsonl" '{app: $app, rules: [
    {match: "ADE89-BUSY-HABIT", calls: [{tool: "send_message", arguments:
      {message: "ADE89-BUSY Wait for the work.", send_to: "ade89-mono", wanting_reply: false, intent: "new"}}], reply: "Asked."},
    {match: "ADE89-HABIT", calls: [{tool: "send_message", arguments:
      {message: "ADE89-GO Start the work session.", send_to: "ade89-mono", wanting_reply: false, intent: "new"}}], reply: "Asked."},
    {match: $notice, reply: "Reviewed."},
    {match: "Continue from where you left off.", hold: 1, reply: "Done.", exec: [["sh", "-c",
      "for _ in $(seq 1 300); do grep -q \"\\\"rule\\\":\\\"ADE89-BUSY\\\"\" \"$0\" && exit 0; sleep 0.2; done; exit 1", $log]]},
    {match: "ADE89-BUSY", hold: 99, reply: "Done."},
    {match: "ADE89-WORK", hold: 1, reply: "Done."},
    {match: "ADE89-GO", calls: [{action: "sessions.start", arguments:
      {prompt: "ADE89-WORK Build it.", harness: "fx", model: "fx:zai/glm-5.2-fast"}}], reply: "Started."}]}' >"$PUPPET/script.json"
}

chat_of() { q "SELECT json_extract(data_json, '$.sessionId') FROM monos WHERE id = '$1'"; }

# Review r1 M1: the store refuses a delivered mark unless the Mono's saved
# chat, its queue or its transcript, already holds that notice.
mark_after_the_notice() {
  sqlite3 "$DB" "CREATE TRIGGER ade89_mark_after_the_notice BEFORE INSERT ON mono_completion_deliveries
    WHEN NOT EXISTS (SELECT 1 FROM sessions WHERE id = '$1' AND instr(queued_messages_json, NEW.group_id) > 0)
      AND NOT EXISTS (SELECT 1 FROM mono_blocks WHERE session_id = '$1' AND instr(block_json, NEW.group_id) > 0)
    BEGIN SELECT RAISE(ABORT, 'ADE-89: marked before the Mono chat saved the notice'); END;"
}

# The app exits with no quit save: kill -9, or SIGTERM (instance.sh stop).
exit_unsaved() {
  case "$1" in
    kill|two) kill9 ;;
    term) "$INSTANCE" stop ;;
  esac
}

rows() { q "SELECT count(*) FROM mono_completions"; }
queued_notices() { q "SELECT (length(queued_messages_json) - length(replace(queued_messages_json, '$NOTICE', ''))) / length('$NOTICE') FROM sessions WHERE id = '$1'"; }
# The notices the Mono's agent got plus those in its saved queue: a kill
# while the Mono's turn ran leaves its queue paused until the user resumes.
notices() { echo $(($(prompts "$NOTICE") + $(queued_notices "$1"))); }
# reports <session id>: the notices that reached the Mono's agent or wait in
# its saved queue, counted per session they report on.
reports() {
  local agent queued
  agent="$(jq -s --arg notice "$NOTICE" --arg id "\"sessionId\":\"$2\"" \
    '[.[] | select(.rule == $notice) | .prompt | [scan($id)] | length] | add // 0' "$PUPPET/log.jsonl")"
  queued="$(q "SELECT (length(queued_messages_json) - length(replace(queued_messages_json, '\\\"sessionId\\\":\\\"$2\\\"', ''))) / length('\\\"sessionId\\\":\\\"$2\\\"') FROM sessions WHERE id = '$1'")"
  echo $((agent + queued))
}

# ADE-89: a waiting completion result is a mono_completions row from the
# moment the Mono starts the session. It survives two exits with no quit save,
# its notice is in the Mono's saved queue before the delivered mark, and a
# kill right after the mark still leaves the Mono exactly one notice. Mode
# `two` runs the build-two instance: main and window-0 boot together at every
# launch and both claim.
run_ade89() {
  local mode="$1" run="$2" giga_chat mono_chat work early _
  FAILED=0
  log "ADE-89 run $run, exit by $mode"
  [[ "$mode" != two || "$(cat "$EVAL_OUT/instance.app")" == *scenario-two* ]] || die "mode two needs app.sh build-two"
  [[ "$mode" == two || "$(cat "$EVAL_OUT/instance.app")" != *scenario-two* ]] || die "mode $mode needs app.sh build"
  fresh_pair ade89
  ade89_script
  request_habit ade89-giga "Start the work" "ADE89-HABIT: ask ADE89 Mono to start the work."
  giga_chat="$(chat_of ade89-giga)"; mono_chat="$(chat_of ade89-mono)"
  mark_after_the_notice "$mono_chat"
  "$INSTANCE" start
  wait_until 90 "the Mono's session runs, alone in flight" \
    holds "SELECT count(*) = 1 AND min(session_id) NOT IN ('$giga_chat', '$mono_chat') FROM in_flight_sessions"
  work="$(q "SELECT session_id FROM in_flight_sessions")"
  wait_until 30 "the Mono's turn ends" holds "SELECT count(*) = 0 FROM in_flight_sessions WHERE session_id = '$mono_chat'"
  wait_until 30 "the waiting result is a row" holds "SELECT count(*) = 1 FROM mono_completions WHERE request_id = '$work'"
  echo "ade89 ($mode) run $run readings:"
  expect "sessions.start calls that failed" "$(jq -s '[.[] | select(.action and .result.ok != true)] | length' "$PUPPET/log.jsonl")" 0
  expect "workspace rows that carry results" "$(q "SELECT (SELECT count(*) FROM workspace_snapshot WHERE json_type(snapshot_json, '$.monoCompletions') IS NOT NULL)
    + (SELECT count(*) FROM workspace_window_snapshots WHERE json_type(snapshot_json, '$.monoCompletions') IS NOT NULL)")" 0
  kill9

  "$INSTANCE" start
  wait_until 60 "the relaunch continues the session" saw "Continue from where you left off."
  if [[ "$mode" == two ]]; then
    # Both windows restore the session, and the second one's harness bind can
    # end the first one's Continue turn (ADE-93), so the result may be out.
    sleep 5
    expect "windows that hold a row, at most one" "$(q "SELECT count(DISTINCT window_label) <= 1 FROM mono_completions")" 1
    expect "rows and delivered groups after the first relaunch" "$(q "SELECT (SELECT count(*) FROM mono_completions) + (SELECT count(*) FROM mono_completion_deliveries)")" 1
  else
    expect "rows after the first relaunch" "$(rows)" 1
    expect "windows that hold them" "$(q "SELECT count(DISTINCT window_label) FROM mono_completions")" 1
    expect "notices before the second exit" "$(prompts "$NOTICE")" 0
  fi
  exit_unsaved "$mode"

  early="$(q "SELECT count(*) FROM mono_completion_deliveries")"
  request_habit ade89-giga "Keep the Mono busy" "ADE89-BUSY-HABIT: ask ADE89 Mono to wait for the work."
  "$INSTANCE" start
  wait_until 60 "the notice is marked delivered" holds "SELECT count(*) = 1 FROM mono_completion_deliveries"
  if [[ "$early" == 0 ]]; then
    expect "rows once it is marked" "$(rows)" 0
    expect "notices in the Mono's saved queue at the mark" "$(queued_notices "$mono_chat")" 1
    expect "notices the Mono's agent got before the kill" "$(prompts "$NOTICE")" 0
  fi
  [[ "$mode" == two ]] && expect "windows that hold a row, at most one" "$(q "SELECT count(DISTINCT window_label) <= 1 FROM mono_completions")" 1
  # Review r1's probe: the app dies right after the delivered mark.
  kill9

  "$INSTANCE" start
  for _ in $(seq 1 60); do (($(notices "$mono_chat") > 0)) && break; sleep 1; done
  # A copy the boot sends again would follow within the boot's first seconds.
  sleep 10
  expect "notices to the Mono after a kill after the mark" "$(notices "$mono_chat")" 1
  expect "rows once it is delivered" "$(rows)" 0
  expect "delivered groups" "$(q "SELECT count(*) FROM mono_completion_deliveries")" 1
  echo "  Continue prompts: $(prompts "Continue from where you left off.")"
  if [[ "$mode" == kill ]]; then
    crash_before_the_mark "$mono_chat" "$work"
    part_release_then_kill "$mono_chat" a b c
    part_release_then_kill "$mono_chat" b c a
  fi
  "$INSTANCE" stop
  return "$FAILED"
}

# saved <mono chat> <request> <session> [result notice]: a mono_completions
# row's JSON for the Mono's turn 1; with a result, the session completed.
saved() {
  local project="$EVAL_HOME/$SCRATCH_NAME"
  echo "json_object('origin', json_object('monoId', '$1', 'turn', 1),
    'request', json_object('requestId', '$2', 'sessionId', '$3', 'project', '$project', 'prompt', 'ADE89-WORK Build it.')
    ${4:+, 'result', json_object('sessionId', '$3', 'project', '$project', 'title', 'ADE89-WORK',
      'status', 'completed', 'originalPrompt', 'ADE89-WORK Build it.', 'result', '$4', 'truncated', json('false')),
      'notice', '$5'})"
}

# A row in a notice the Mono chat already saved, with no mark: what a hold
# that reaches Rust after that save leaves. The boot delivers it with no
# second notice.
crash_before_the_mark() {
  local work="$2"
  kill9
  sqlite3 "$DB" "DELETE FROM mono_completion_deliveries;
    INSERT INTO mono_completions (request_id, saved_json, window_label, window_run)
      VALUES ('$work', $(saved "$1" "$work" "$work" Done. "mono-completion-$work"), 'main', 'killed-run');"
  "$INSTANCE" start
  wait_until 30 "the relaunch marks it" \
    holds "SELECT (SELECT count(*) FROM mono_completions) = 0 AND (SELECT count(*) FROM mono_completion_deliveries) = 1"
  # A second copy would follow the first within the same second or two.
  sleep 5
  expect "notices after a row outlived its notice's save" "$(notices "$1")" 1
}

# Review r1 M2: one Mono turn watched three sessions. Two ended and their
# notice is in the Mono's saved queue, the app was killed before the mark,
# and the third never continues. The relaunch reports the third once and
# the two not again, whichever request id sorts first.
part_release_then_kill() {
  local first="ade89-$2" second="ade89-$3" late="ade89-$4" group message
  group="mono-completion-batch-$(printf '%s\n' "$first" "$second" | sort | head -1)"
  message="json_object('id', '$group', 'attachments', json_array(),
    'monoSessionCompletion', json_object('sessionId', 's-$first', 'title', '2 session results', 'status', 'completed', 'sessionCount', 2),
    'text', '$NOTICE: these monitored sessions launched during your request have now stopped.' || char(10) || char(10)
      || json_object('sessions', json_array(json_object('sessionId', 's-$first'), json_object('sessionId', 's-$second'))))"
  kill9
  sqlite3 "$DB" "DELETE FROM mono_completion_deliveries;
    INSERT INTO mono_completions (request_id, saved_json, window_label, window_run) VALUES
      ('$first', $(saved "$1" "$first" "s-$first" "Result $2" "$group"), 'main', 'killed-run'),
      ('$second', $(saved "$1" "$second" "s-$second" "Result $3" "$group"), 'main', 'killed-run'),
      ('$late', $(saved "$1" "$late" "s-$late"), 'main', 'killed-run');
    UPDATE sessions SET queued_messages_json = json_array($message) WHERE id = '$1';"
  "$INSTANCE" start
  wait_until 30 "the relaunch marks both notices" \
    holds "SELECT (SELECT count(*) FROM mono_completions) = 0 AND (SELECT count(*) FROM mono_completion_deliveries) = 2"
  # A second copy would follow the first within the same second or two.
  sleep 5
  for id in "$first" "$second" "$late"; do
    expect "reports of $id after a part release and a kill (late $late)" "$(reports "$1" "s-$id")" 1
  done
}

case "${1:-}" in
  build) "$INSTANCE" build "${2:-$SCENARIOS/../..}" scenario ;;
  # The same app with a second workspace window, window-0, from the tauri
  # config: both windows boot at every launch, as File > New Window opens one.
  build-two)
    GIGA_EVAL_CONFIG="$(jq -c '{app: {windows: [.app.windows[0], .app.windows[0] + {label: "window-0"}]}}' \
      "${2:-$SCENARIOS/../..}/src-tauri/tauri.conf.json")" "$INSTANCE" build "${2:-$SCENARIOS/../..}" scenario-two ;;
  # One run per process: a run called in `||` would run with errexit off.
  s0-run) run_s0 "$2" ;;
  d1) run_d1 ;;
  ade89-run) run_ade89 "$2" "$3" ;;
  ade89)
    [[ "${2:-}" =~ ^(kill|term|two)$ ]] || die "usage: app.sh ade89 kill|term|two [runs]"
    failed=0
    for run in $(seq 1 "${3:-1}"); do "$0" ade89-run "$2" "$run" || failed=$((failed + 1)); done
    log "ADE-89 $2: $(( ${3:-1} - failed ))/${3:-1} passed"
    ((failed == 0)) ;;
  s0)
    failed=0
    for run in $(seq 1 "${2:-1}"); do "$0" s0-run "$run" || failed=$((failed + 1)); done
    log "S0: $(( ${2:-1} - failed ))/${2:-1} passed"
    ((failed == 0)) ;;
  *) sed -n '2,15p' "$0"; exit 2 ;;
esac
