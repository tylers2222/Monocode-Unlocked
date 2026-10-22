#!/usr/bin/env bash
# Real-app scenarios on a GigaEval instance with the fake-clock build and the
# puppet agent (scripts/scenarios/puppet/puppet.mjs) as fx. No model runs.
#
#   app.sh build [checkout]   build the instance (under heavy-lock.sh)
#   app.sh s0 [runs]          S0 through the real app, `runs` times (default 1)
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

# A Giga `<tag>-giga` and a Mono `<tag>-mono` with new chats in the
# scratch project, and one Giga habit with `habit` as its instructions that
# runs at the next launch.
seed() {
  local tag="$1" habit="$2" scratch="$EVAL_HOME/$SCRATCH_NAME" now; now="$(date +%s)000"
  local giga_chat mono_chat
  giga_chat="$(uuidgen | tr '[:upper:]' '[:lower:]')"; mono_chat="$(uuidgen | tr '[:upper:]' '[:lower:]')"
  sqlite3 "$DB" <<SQL
INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at) VALUES
  ('$tag-giga', '$tag Giga', 'giga', json_object('name', '$tag Giga', 'sessionId', '$giga_chat',
    'gigaSlug', '$tag-giga', 'projects', json_array('$scratch')), 0, $now, $now),
  ('$tag-mono', '$tag Mono', 'mono', json_object('name', '$tag Mono', 'sessionId', '$mono_chat',
    'projects', json_array('$scratch')), 1, $now, $now);
INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at) VALUES
  ('$giga_chat', '$scratch', 'fx', '$MODEL', 'auto', '$tag Giga', $now, $now),
  ('$mono_chat', '$scratch', 'fx', '$MODEL', 'auto', '$tag Mono', $now, $now);
SQL
  mkdir -p "$APP_DATA/monos/$tag-giga"
  jq -n --argjson now "$now" --arg id "$tag-habit" --arg habit "$habit" '[{id: $id, name: "Scenario habit", instructions: $habit,
    schedule: {scheduleKind: "daily"}, enabled: true, createdAt: $now, nextRunAt: ($now + 86400000), runRequested: true}]' \
    >"$APP_DATA/monos/$tag-giga/habits.json"
}

# The puppet as fx, with `rules` (a JSON array) as its script.
install_puppet() {
  mkdir -p "$EVAL_HOME/.local/bin" "$PUPPET"
  cp "$SCENARIOS/puppet/puppet.mjs" "$EVAL_HOME/.local/bin/fx"
  chmod +x "$EVAL_HOME/.local/bin/fx"
  : >"$PUPPET/log.jsonl"
  jq -n --arg app "$(target_bin instance)" --argjson rules "$1" '{app: $app, rules: $rules}' >"$PUPPET/script.json"
}

# A jq filter over the puppet log that prints true.
holds_log() { jq -se "$1" "$PUPPET/log.jsonl" >/dev/null; }

prompts() { jq -s --arg rule "$1" '[.[] | select(.rule == $rule)] | length' "$PUPPET/log.jsonl"; }
saw() { (($(prompts "$1") > 0)); }

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

# A fresh instance with the clock at 0, the puppet's `rules`, and `seed tag
# habit`, started. The first launch runs `active` ms of awake time (default 0).
launch_seeded() {
  local tag="$1" habit="$2" rules="$3" active="${4:-0}"
  "$INSTANCE" stop
  "$INSTANCE" home --fresh
  mkdir -p "$APP_DATA"
  UPTIME=0; WALL="$(date +%s)000"; BOOT_MS=$((WALL - 3600000))
  clock_write "$UPTIME" "$WALL"
  install_puppet "$rules"
  # The first launch creates and migrates the DB.
  "$INSTANCE" start
  wait_until 30 "the first launch writes mono_clock" holds "SELECT count(*) = 1 FROM mono_clock"
  advance "$active"
  "$INSTANCE" stop
  seed "$tag" "$habit"
  "$INSTANCE" start
}

MODEL="fx:zai/glm-5.2-fast"
# The answer rule goes first: a prompt carrying the answer may quote the ask.
S0_RULES='[
  {"match": "S0-HABIT", "calls": [{"tool": "send_message", "arguments":
    {"message": "S0-Q Is CI green?", "send_to": "s0-mono", "wanting_reply": false, "intent": "new"}}], "reply": "Asked."},
  {"match": "S0-A", "reply": "Noted."},
  {"match": "S0-Q", "calls": [{"tool": "send_message", "arguments":
    {"message": "S0-A Yes, CI is green.", "send_to": "s0-giga", "wanting_reply": false, "intent": "new"}}], "reply": "Answered."}]'

run_s0() {
  local run="$1"
  FAILED=0
  log "S0 run $run"
  launch_seeded s0 "S0-HABIT: ask S0 Mono if CI is green." "$S0_RULES"
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
      {"prompt": "R6-BRIEF You lead the task.", "harness": "fx", "model": "'"$MODEL"'", "notifyOnComplete": false}},
    {"tool": "ask", "arguments": {"message": "R6-TASK Do the task.", "send_to": "$prev.id", "due_minutes": 30}},
    {"tool": "ack", "arguments": {"ask_id": "$ask_id", "message": "R6-REPORT The project runs."}}], "reply": "Started."},
  {"match": "R6-TASK", "calls": [
    {"tool": "ack", "arguments": {"ask_id": "$ask_id", "message": "R6-DONE The task is done."}},
    {"tool": "ack", "arguments": {"ask_id": "$ask_id", "message": "R6-AGAIN The task is done."}}], "reply": "Done."},
  {"match": "R6-BRIEF", "reply": "Ready."}]'

run_r6() {
  FAILED=0
  log "R6 run"
  # Active time before the asks, so a due_in that forgets to subtract it shows.
  launch_seeded r6 "R6-HABIT: ask R6 Mono to run the project." "$R6_RULES" 60000
  wait_until 120 "both asks are acked" holds "SELECT count(*) = 2 FROM mono_asks WHERE state = 'acked'"
  wait_until 60 "the Giga gets the Mono's ack and the Mono its lead's" saw R6-REPORT
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
  "$INSTANCE" stop
  return "$FAILED"
}

case "${1:-}" in
  build) "$INSTANCE" build "${2:-$SCENARIOS/../..}" scenario ;;
  # One run per process: a run called in `||` would run with errexit off.
  s0-run) run_s0 "$2" ;;
  s0)
    failed=0
    for run in $(seq 1 "${2:-1}"); do "$0" s0-run "$run" || failed=$((failed + 1)); done
    log "S0: $(( ${2:-1} - failed ))/${2:-1} passed"
    ((failed == 0)) ;;
  r6) run_r6 ;;
  *) sed -n '2,10p' "$0"; exit 2 ;;
esac
