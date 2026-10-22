#!/usr/bin/env bash
# Real-app scenarios on a GigaEval instance with the fake-clock build and the
# puppet agent (scripts/scenarios/puppet/puppet.mjs) as fx. No model runs.
#
#   app.sh build [checkout]   build the instance (under heavy-lock.sh)
#   app.sh s0 [runs]          S0 through the real app, `runs` times (default 1)
#   app.sh d4                 D4 deny rules, off then on, with a recorder as
#                             claude; D4_CLAUDE=<real claude> also runs a model
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
  "$INSTANCE" stop
  "$INSTANCE" home --fresh
  mkdir -p "$APP_DATA"
  UPTIME=0; WALL="$(date +%s)000"; BOOT_MS=$((WALL - 3600000))
  clock_write "$UPTIME" "$WALL"
  install_puppet
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

D4_INTERACTIVE="--setting-sources=user,project,local"

install_claude_recorder() {
  local bin="$EVAL_HOME/.local/bin/claude"
  mkdir -p "$(dirname "$bin")" "$PUPPET"
  # The app starts it without $PUPPET. `--version` is the launch update check
  # through harness_exec, not a start, so it is not recorded.
  { echo '#!/usr/bin/env bash'
    printf 'PUPPET=%q\n' "$PUPPET"
    cat <<'SH'
REAL="$(cat "$PUPPET/real-claude" 2>/dev/null || true)"
if [[ "${1:-}" == --version ]]; then
  [[ -z "$REAL" ]] || exec "$REAL" "$@"
  echo "2.1.305 (Claude Code)"; exit 0
fi
printf '%s\0' "$@" >"$PUPPET/claude-argv/$$"
[[ -n "$REAL" ]] || exit 1
exec "$REAL" "$@" > >(tee -a "$PUPPET/claude-out.jsonl")
SH
  } >"$bin"
  chmod +x "$bin"
}

seed_d4() {
  local giga_chat="$1" scratch="$EVAL_HOME/$SCRATCH_NAME" now; now="$(date +%s)000"
  sqlite3 "$DB" <<SQL
INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at) VALUES
  ('d4-giga', 'D4 Giga', 'giga', json_object('name', 'D4 Giga', 'sessionId', '$giga_chat',
    'gigaSlug', 'd4-giga', 'projects', json_array('$scratch')), 0, $now, $now);
INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at) VALUES
  ('$giga_chat', '$scratch', 'claude', 'claude:haiku-4.5', 'full-access', 'D4 Giga', $now, $now);
SQL
  request_d4_habit
}

request_d4_habit() {
  local now; now="$(date +%s)000"
  mkdir -p "$APP_DATA/monos/d4-giga"
  jq -n --argjson now "$now" --arg db "$DB" '[{id: "d4-read", name: "Read the database",
    instructions: "D4-HABIT: Use the Read tool to read the file \($db). Make only that one tool call. Then reply with the exact text of the tool result.",
    schedule: {scheduleKind: "daily"}, enabled: true, createdAt: $now, nextRunAt: ($now + 86400000), runRequested: true}]' \
    >"$APP_DATA/monos/d4-giga/habits.json"
}

d4_starts() { find "$1" -type f -exec jq -Rs 'split("\u0000")[:-1]' {} \; | jq -s -c .; }

d4_jq() {
  jq -S -c --arg interactive "$D4_INTERACTIVE" '
    def interactive: index($interactive) != null;
    def settings: index("--settings") as $i | if $i then .[$i + 1] | fromjson else null end;
    def redacted: index("--mcp-config") as $i | if $i then .[$i + 1] = "<mcp-config>" else . end;
    '"$1" <<<"$2"
}

run_d4() {
  FAILED=0
  log "D4"
  "$INSTANCE" stop
  "$INSTANCE" home --fresh
  mkdir -p "$APP_DATA"
  UPTIME=0; WALL="$(date +%s)000"; BOOT_MS=$((WALL - 3600000))
  clock_write "$UPTIME" "$WALL"
  install_claude_recorder
  mkdir -p "$PUPPET/claude-argv"
  # The PATH prefix makes the app resolve the recorder as claude. The first
  # launch only creates and migrates the DB.
  PATH="$EVAL_HOME/.local/bin:$PATH" "$INSTANCE" start
  wait_until 30 "the first launch writes mono_clock" holds "SELECT count(*) = 1 FROM mono_clock"
  "$INSTANCE" stop
  local giga_chat; giga_chat="$(uuidgen | tr '[:upper:]' '[:lower:]')"
  seed_d4 "$giga_chat"
  PATH="$EVAL_HOME/.local/bin:$PATH" "$INSTANCE" start
  wait_until 90 "the app starts Claude Code for the Giga" grep -qrF -e "$D4_INTERACTIVE" "$PUPPET/claude-argv"
  "$INSTANCE" stop
  mv "$PUPPET/claude-argv" "$PUPPET/off"
  mkdir "$PUPPET/claude-argv"
  touch "$APP_DATA/claude-deny-rules-on"
  request_d4_habit
  if [[ -n "${D4_CLAUDE:-}" ]]; then echo "$D4_CLAUDE" >"$PUPPET/real-claude"; fi
  PATH="$EVAL_HOME/.local/bin:$PATH" "$INSTANCE" start
  wait_until 90 "the app starts Claude Code for the Giga" grep -qrF -e "$D4_INTERACTIVE" "$PUPPET/claude-argv"
  if [[ -n "${D4_CLAUDE:-}" ]]; then
    wait_until 180 "Claude Code ends a turn" grep -qsF '"type":"result"' "$PUPPET/claude-out.jsonl"
  fi
  "$INSTANCE" stop
  mv "$PUPPET/claude-argv" "$PUPPET/on"
  local off on interactive
  off="$(d4_starts "$PUPPET/off")"; on="$(d4_starts "$PUPPET/on")"
  interactive="$(d4_jq '[.[] | select(interactive)] | length' "$off")"
  echo "D4 readings:"
  expect "OFF starts with deny rules" \
    "$(d4_jq '[.[] | settings | select(.permissions != null or .sandbox != null)] | length' "$off")" 0
  expect "OFF interactive starts ($interactive), at least 1" "$(jq -n "$interactive >= 1")" true
  expect "ON interactive permissions" "$(d4_jq 'first(.[] | select(interactive)) | settings.permissions' "$on")" \
    "$(jq -S -c -n --arg d "$APP_DATA" '{deny: ["Read(/\($d)/**)", "Edit(/\($d)/**)"]}')"
  expect "ON interactive sandbox" "$(d4_jq 'first(.[] | select(interactive)) | settings.sandbox' "$on")" \
    "$(jq -S -c -n --arg d "$APP_DATA" '{enabled: true, allowUnsandboxedCommands: false,
      filesystem: {denyRead: [$d], denyWrite: [$d]}}')"
  expect "ON starts without deny rules, of $(d4_jq length "$on")" \
    "$(d4_jq '[.[] | settings | select(.permissions.deny == null)] | length' "$on")" 0
  expect "ON interactive settings apart from the rules" \
    "$(d4_jq 'first(.[] | select(interactive)) | settings | del(.permissions, .sandbox)' "$on")" \
    "$(d4_jq 'first(.[] | select(interactive)) | settings' "$off")"
  echo "  OFF interactive argv: $(d4_jq 'first(.[] | select(interactive)) | redacted' "$off")"
  echo "  ON interactive argv: $(d4_jq 'first(.[] | select(interactive)) | redacted' "$on")"
  if [[ -n "${D4_CLAUDE:-}" ]]; then
    jq -rR 'fromjson? | select(.type == "user") | .message.content[]? | select(.type? == "tool_result")
      | "  tool_result: \(.content | if type == "array" then map(.text? // empty) | join("\n") else . end)"' \
      "$PUPPET/claude-out.jsonl"
    jq -rR 'fromjson? | select(.type == "result") | "  total_cost_usd: \(.total_cost_usd)"' "$PUPPET/claude-out.jsonl"
  fi
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
  d4-run) run_d4 ;;
  d4) "$0" d4-run ;;
  *) sed -n '2,11p' "$0"; exit 2 ;;
esac
