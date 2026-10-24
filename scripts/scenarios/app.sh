#!/usr/bin/env bash
# Real-app scenarios on a GigaEval instance with the fake-clock build and the
# puppet agent (scripts/scenarios/puppet/puppet.mjs) as fx. No model runs.
#
#   app.sh build [checkout]   build the instance (under heavy-lock.sh)
#   app.sh s0 [runs]          S0 through the real app, `runs` times (default 1)
#   app.sh d1                 D1: the agent sandbox on, off, and on without srt
#   app.sh r6                 R6: agents ask and ack through MCP in the real app
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

# A jq filter over the puppet log that prints true.
holds_log() { jq -se "$1" "$PUPPET/log.jsonl" >/dev/null; }

# The <mono_message ...> tag of the first prompt that rule `$1` took, and
# attribute `$2` of it (empty when the tag has none).
attribute() {
  jq -rs --arg rule "$1" 'first(.[] | select(.rule == $rule) | .prompt)' "$PUPPET/log.jsonl" \
    | grep -o '<mono_message [^>]*>' | head -1 | sed -n "s/.* $2=\"\([^\"]*\)\".*/\1/p"
}

# Field `$2` of the answer to the call whose input message is `$1`.
answer() {
  jq -rs --arg message "$1" --arg field "$2" \
    'first(.[] | select(.input.message == $message) | .result[$field]) | tostring' "$PUPPET/log.jsonl"
}

# R6: the Giga asks the Mono, the Mono starts a lead and asks it with a
# 30-minute deadline, and each acks through MCP. The lead acks twice and the
# Mono acks its own ask; both refusals reach the puppet.
R6_RULES='[
  {"match": "R6-HABIT", "calls": [{"tool": "ask", "arguments":
    {"message": "R6-PROJECT Run the project.", "send_to": "r6-mono"}}], "reply": "Asked."},
  {"match": "R6-REPORT", "reply": "Noted."},
  {"match": "R6-DONE", "calls": [{"tool": "ack", "arguments":
    {"ask_id": "$ack_of", "message": "R6-MINE Acking my own ask."}}], "reply": "Checked."},
  {"match": "R6-PROJECT", "calls": [
    {"action": "sessions.start", "arguments":
      {"prompt": "R6-BRIEF You lead the task.", "harness": "fx", "model": "fx:zai/glm-5.2-fast", "notifyOnComplete": false}},
    {"tool": "ask", "arguments": {"message": "R6-TASK Do the task.", "send_to": "$prev.id", "due_minutes": 30}},
    {"tool": "ack", "arguments": {"ask_id": "$ask_id", "message": "R6-REPORT The project runs."}}], "reply": "Started."},
  {"match": "R6-TASK", "calls": [
    {"tool": "ack", "arguments": {"ask_id": "$ask_id", "message": "R6-DONE The task is done."}},
    {"tool": "ack", "arguments": {"ask_id": "$ask_id", "message": "R6-AGAIN The task is done."}}], "reply": "Done."},
  {"match": "R6-BRIEF", "reply": "Ready."}]'

run_r6() {
  FAILED=0
  log "R6 run"
  fresh_pair r6
  # Active time before the asks, so a due_in that forgets to subtract it shows.
  "$INSTANCE" start
  advance 60000
  "$INSTANCE" stop
  jq -n --arg app "$(target_bin instance)" --argjson rules "$R6_RULES" '{app: $app, rules: $rules}' >"$PUPPET/script.json"
  request_habit r6-giga "Ask for the project" "R6-HABIT: ask R6 Mono to run the project."
  "$INSTANCE" start
  wait_until 120 "both asks are acked" holds "SELECT count(*) = 2 FROM mono_asks WHERE state = 'acked'"
  wait_until 60 "the Giga gets the Mono's ack" saw R6-REPORT
  wait_until 60 "the Mono tries to ack its own ask" \
    holds_log '[.[] | select(.input.message == "R6-MINE Acking my own ask.")] | length == 1'
  local giga_ask mono_ask lead
  giga_ask="$(q "SELECT id FROM mono_asks WHERE parent_actor = 'mono:r6-giga'")"
  mono_ask="$(q "SELECT id FROM mono_asks WHERE parent_actor = 'mono:r6-mono'")"
  lead="$(q "SELECT to_session_id FROM mono_messages WHERE tool = 'start_session' AND from_mono_id = 'r6-mono'")"
  echo "R6 readings (Giga ask $giga_ask, Mono ask $mono_ask, lead $lead):"
  expect "Giga ask: child, deadline, state" \
    "$(q "SELECT child_actor || ' ' || ifnull(due_active_ms, 'none') || ' ' || state FROM mono_asks WHERE id = '$giga_ask'")" \
    "mono:r6-mono none acked"
  expect "Mono ask: child, active ms left, state" \
    "$(q "SELECT child_actor || ' ' || (due_active_ms - (SELECT active_ms FROM mono_clock)) || ' ' || state FROM mono_asks WHERE id = '$mono_ask'")" \
    "session:$lead 1800000 acked"
  expect "ack events" "$(q "SELECT count(*) FROM mono_ask_events WHERE kind = 'ack' AND ask_id IN ('$giga_ask', '$mono_ask')")" 2
  expect "Mono's ask arrives with ask_id" "$(attribute R6-PROJECT ask_id)" "$giga_ask"
  expect "Mono's ask from a Giga has no due_in" "$(attribute R6-PROJECT due_in)" ""
  expect "lead's ask arrives with ask_id" "$(attribute R6-TASK ask_id)" "$mono_ask"
  expect "lead's ask arrives with due_in" "$(attribute R6-TASK due_in)" "30m"
  expect "Mono gets the lead's ack with ack_of" "$(attribute R6-DONE ack_of)" "$mono_ask"
  expect "Giga gets the Mono's ack with ack_of" "$(attribute R6-REPORT ack_of)" "$giga_ask"
  expect "lead's ack" "$(answer "R6-DONE The task is done." ok)" true
  expect "lead's second ack: code" "$(answer "R6-AGAIN The task is done." code)" ack_refused
  expect "lead's second ack: hint" "$(answer "R6-AGAIN The task is done." hint)" \
    "This ask was already acked. A new ask from your parent has a new id."
  expect "Mono acks its own ask: code" "$(answer "R6-MINE Acking my own ask." code)" ack_refused
  expect "Mono acks its own ask: hint" "$(answer "R6-MINE Acking my own ask." hint)" \
    "This ask is not yours: only the Mono or session it was sent to can ack it."
  expect "lead prompts with the ask" "$(prompts R6-TASK)" 1
  expect "calls that failed" "$(jq -s '[.[] | select((.tool or .action) and .result.ok != true)] | length' "$PUPPET/log.jsonl")" 2
  expect "steers" "$(jq -s '[.[] | select(.steer == true)] | length' "$PUPPET/log.jsonl")" 0
  q "SELECT '  ask ' || id || ' ' || parent_actor || ' -> ' || child_actor || ' ' || state FROM mono_asks ORDER BY created_wall_ms"
  cp "$PUPPET/log.jsonl" "$EVAL_OUT/r6.puppet.jsonl"
  "$INSTANCE" stop
  return "$FAILED"
}

case "${1:-}" in
  build) "$INSTANCE" build "${2:-$SCENARIOS/../..}" scenario ;;
  # One run per process: a run called in `||` would run with errexit off.
  s0-run) run_s0 "$2" ;;
  d1) run_d1 ;;
  r6) run_r6 ;;
  s0)
    failed=0
    for run in $(seq 1 "${2:-1}"); do "$0" s0-run "$run" || failed=$((failed + 1)); done
    log "S0: $(( ${2:-1} - failed ))/${2:-1} passed"
    ((failed == 0)) ;;
  *) sed -n '2,11p' "$0"; exit 2 ;;
esac
