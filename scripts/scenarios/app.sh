#!/usr/bin/env bash
# Real-app scenarios on a GigaEval instance with the fake-clock build and the
# puppet agent (scripts/scenarios/puppet/puppet.mjs) as fx. No model runs.
#
#   app.sh build [checkout]   build the instance (under heavy-lock.sh)
#   app.sh s0 [runs]          S0 through the real app, `runs` times (default 1)
#   app.sh ade89 MODE [runs]  ADE-89: a waiting completion result survives
#                             two exits with no quit save (MODE kill or
#                             term) and arrives once
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

seed() {
  local giga_chat="$1" mono_chat="$2" scratch="$EVAL_HOME/$SCRATCH_NAME" now; now="$(date +%s)000"
  sqlite3 "$DB" <<SQL
INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at) VALUES
  ('s0-giga', 'S0 Giga', 'giga', json_object('name', 'S0 Giga', 'sessionId', '$giga_chat',
    'gigaSlug', 's0-giga', 'projects', json_array('$scratch')), 0, $now, $now),
  ('s0-mono', 'S0 Mono', 'mono', json_object('name', 'S0 Mono', 'sessionId', '$mono_chat',
    'projects', json_array('$scratch')), 1, $now, $now);
INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at) VALUES
  ('$giga_chat', '$scratch', 'fx', 'fx:zai/glm-5.2-fast', 'auto', 'S0 Giga', $now, $now),
  ('$mono_chat', '$scratch', 'fx', 'fx:zai/glm-5.2-fast', 'auto', 'S0 Mono', $now, $now);
SQL
  mkdir -p "$APP_DATA/monos/s0-giga"
  jq -n --argjson now "$now" '[{id: "s0-ask", name: "Ask about CI", instructions: "S0-HABIT: ask S0 Mono if CI is green.",
    schedule: {scheduleKind: "daily"}, enabled: true, createdAt: $now, nextRunAt: ($now + 86400000), runRequested: true}]' \
    >"$APP_DATA/monos/s0-giga/habits.json"
}

install_puppet() {
  local rules="$1"
  mkdir -p "$EVAL_HOME/.local/bin" "$PUPPET"
  cp "$SCENARIOS/puppet/puppet.mjs" "$EVAL_HOME/.local/bin/fx"
  chmod +x "$EVAL_HOME/.local/bin/fx"
  : >"$PUPPET/log.jsonl"
  jq -n --arg app "$(target_bin instance)" --argjson rules "$rules" '{app: $app, rules: $rules}' >"$PUPPET/script.json"
}

# The answer rule goes first: a prompt carrying the answer may quote the ask.
S0_RULES='[
  {"match": "S0-HABIT", "calls": [{"tool": "send_message", "arguments":
    {"message": "S0-Q Is CI green?", "send_to": "s0-mono", "wanting_reply": false, "intent": "new"}}], "reply": "Asked."},
  {"match": "S0-A", "reply": "Noted."},
  {"match": "S0-Q", "calls": [{"tool": "send_message", "arguments":
    {"message": "S0-A Yes, CI is green.", "send_to": "s0-giga", "wanting_reply": false, "intent": "new"}}], "reply": "Answered."}]'

prompts() { jq -s --arg rule "$1" '[.[] | select(.rule == $rule)] | length' "$PUPPET/log.jsonl"; }
saw() { (($(prompts "$1") > 0)); }

run_s0() {
  local run="$1"
  FAILED=0
  log "S0 run $run"
  "$INSTANCE" stop
  "$INSTANCE" home --fresh
  mkdir -p "$APP_DATA"
  UPTIME=0; WALL="$(date +%s)000"; BOOT_MS=$((WALL - 3600000))
  clock_write "$UPTIME" "$WALL"
  install_puppet "$S0_RULES"
  # The first launch only creates and migrates the DB.
  "$INSTANCE" start
  wait_until 30 "the first launch writes mono_clock" holds "SELECT count(*) = 1 FROM mono_clock"
  "$INSTANCE" stop
  local giga_chat mono_chat
  giga_chat="$(uuidgen | tr '[:upper:]' '[:lower:]')"; mono_chat="$(uuidgen | tr '[:upper:]' '[:lower:]')"
  seed "$giga_chat" "$mono_chat"
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

# The notice rule goes first: the notice quotes the session's prompt.
NOTICE="MonoCode completion notification"
ADE89_RULES='[
  {"match": "S0-HABIT", "calls": [{"tool": "send_message", "arguments":
    {"message": "ADE89-GO Start the work session.", "send_to": "s0-mono", "wanting_reply": false, "intent": "new"}}], "reply": "Asked."},
  {"match": "'"$NOTICE"'", "reply": "Reviewed."},
  {"match": "Continue from where you left off.", "hold": 1, "reply": "Done."},
  {"match": "ADE89-WORK", "hold": 1, "reply": "Done."},
  {"match": "ADE89-GO", "calls": [{"action": "sessions.start", "arguments":
    {"prompt": "ADE89-WORK Build it.", "harness": "fx", "model": "fx:zai/glm-5.2-fast"}}], "reply": "Started."}]'

# The app exits with no quit save: kill -9, or SIGTERM (instance.sh stop).
exit_unsaved() {
  case "$1" in
    kill) kill9 ;;
    term) "$INSTANCE" stop ;;
  esac
}

rows() { q "SELECT count(*) FROM mono_completions"; }

# ADE-89: a waiting completion result is a mono_completions row from the
# moment the Mono starts the session. It survives two exits with no quit save
# and reaches the Mono once.
run_ade89() {
  local mode="$1" run="$2" giga_chat mono_chat work _
  FAILED=0
  log "ADE-89 run $run, exit by $mode"
  "$INSTANCE" stop
  "$INSTANCE" home --fresh
  mkdir -p "$APP_DATA"
  UPTIME=0; WALL="$(date +%s)000"; BOOT_MS=$((WALL - 3600000))
  clock_write "$UPTIME" "$WALL"
  install_puppet "$ADE89_RULES"
  "$INSTANCE" start
  wait_until 30 "the first launch writes mono_clock" holds "SELECT count(*) = 1 FROM mono_clock"
  "$INSTANCE" stop
  giga_chat="$(uuidgen | tr '[:upper:]' '[:lower:]')"; mono_chat="$(uuidgen | tr '[:upper:]' '[:lower:]')"
  seed "$giga_chat" "$mono_chat"
  "$INSTANCE" start
  wait_until 90 "the Mono's session runs, alone in flight" \
    holds "SELECT count(*) = 1 AND min(session_id) NOT IN ('$giga_chat', '$mono_chat') FROM in_flight_sessions"
  work="$(q "SELECT session_id FROM in_flight_sessions")"
  wait_until 30 "the Mono's turn ends" holds "SELECT count(*) = 0 FROM in_flight_sessions WHERE session_id = '$mono_chat'"
  wait_until 30 "the waiting result is a row" holds "SELECT count(*) = 1 FROM mono_completions WHERE request_id = '$work'"
  echo "ade89 ($mode) run $run readings:"
  expect "sessions.start calls that failed" "$(jq -s '[.[] | select(.action and .result.ok != true)] | length' "$PUPPET/log.jsonl")" 0
  expect "workspace rows that carry results" "$(q "SELECT count(*) FROM workspace_snapshot WHERE json_type(snapshot_json, '$.monoCompletions') IS NOT NULL")" 0
  kill9

  "$INSTANCE" start
  wait_until 60 "the relaunch continues the session" saw "Continue from where you left off."
  expect "rows after the first relaunch" "$(rows)" 1
  expect "windows that hold them" "$(q "SELECT count(DISTINCT window_label) FROM mono_completions")" 1
  expect "notices before the second exit" "$(prompts "$NOTICE")" 0
  exit_unsaved "$mode"

  "$INSTANCE" start
  for _ in $(seq 1 60); do saw "$NOTICE" && break; sleep 1; done
  # A second copy would follow the first within the same second or two.
  sleep 5
  expect "notices to the Mono" "$(prompts "$NOTICE")" 1
  expect "rows once it is delivered" "$(rows)" 0
  expect "delivered groups" "$(q "SELECT count(*) FROM mono_completion_deliveries")" 1
  echo "  Continue prompts: $(prompts "Continue from where you left off.")"
  [[ "$mode" == kill ]] && crash_before_the_mark "$mono_chat" "$work"
  "$INSTANCE" stop
  return "$FAILED"
}

# A kill after the notice reached the Mono's queue and before the delivered
# commit leaves the result's row and no mark. That state is rebuilt here from
# the delivered run, since the gap between the two writes is too short to
# hit with a kill. The relaunch sends the notice again under the same id,
# and the Mono's queue, which already holds it, takes it once.
crash_before_the_mark() {
  local mono_chat="$1" work="$2" project="$EVAL_HOME/$SCRATCH_NAME"
  kill9
  sqlite3 "$DB" "DELETE FROM mono_completion_deliveries;
    INSERT INTO mono_completions (request_id, saved_json, window_label, window_run) VALUES ('$work',
      json_object('origin', json_object('monoId', '$mono_chat', 'turn', 1),
        'request', json_object('requestId', '$work', 'sessionId', '$work', 'project', '$project',
          'prompt', 'ADE89-WORK Build it.'),
        'result', json_object('sessionId', '$work', 'project', '$project', 'title', 'ADE89-WORK',
          'status', 'completed', 'originalPrompt', 'ADE89-WORK Build it.', 'result', 'Done.',
          'truncated', json('false'))),
      'main', 'killed-run');"
  "$INSTANCE" start
  wait_until 30 "the relaunch sends it again and marks it" \
    holds "SELECT (SELECT count(*) FROM mono_completions) = 0 AND (SELECT count(*) FROM mono_completion_deliveries) = 1"
  # A second copy would follow the first within the same second or two.
  sleep 5
  expect "notices after a kill between the queue and the mark" "$(prompts "$NOTICE")" 1
}

case "${1:-}" in
  build) "$INSTANCE" build "${2:-$SCENARIOS/../..}" scenario ;;
  # One run per process: a run called in `||` would run with errexit off.
  s0-run) run_s0 "$2" ;;
  ade89-run) run_ade89 "$2" "$3" ;;
  ade89)
    [[ "${2:-}" =~ ^(kill|term)$ ]] || die "usage: app.sh ade89 kill|term [runs]"
    failed=0
    for run in $(seq 1 "${3:-1}"); do "$0" ade89-run "$2" "$run" || failed=$((failed + 1)); done
    log "ADE-89 $2: $(( ${3:-1} - failed ))/${3:-1} passed"
    ((failed == 0)) ;;
  s0)
    failed=0
    for run in $(seq 1 "${2:-1}"); do "$0" s0-run "$run" || failed=$((failed + 1)); done
    log "S0: $(( ${2:-1} - failed ))/${2:-1} passed"
    ((failed == 0)) ;;
  *) sed -n '2,9p' "$0"; exit 2 ;;
esac
