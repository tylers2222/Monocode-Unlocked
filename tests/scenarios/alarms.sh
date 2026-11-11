# shellcheck shell=bash
# ADE-260 alarms, design section 14 E2E, for scripts/scenarios/app.sh, which
# sources this file and provides q, holds, holds_log, wait_until, advance,
# active_ms, expect, fresh_pair, request_habit, prompts, saw and the paths.
#   alarms stall   a worker idle with its start ask open alarms its lead at 15
#                  active min, and 15 min with no act takes it to the Mono
#   alarms dialog  a Claude worker's permission dialog alarms its lead at 3
#                  min; the lead's alarms.answer --allow lets the worker go on
#   alarms hold    Claude: a wait sent to the background with `&` or
#                  run_in_background runs unheld; a foreground sleep waits
#                  for the Mono: deny, allow, no answer until the 2 min cut
#   alarms omp     the same three foreground holds for omp
# Claude Code and omp talk to alarms-model.py, never to a real model.

ALARMS_DIR="$SCENARIOS/../../tests/scenarios"
ALARMS_FX='{"harness": "fx", "model": "fx:zai/glm-5.2-fast", "notifyOnComplete": false}'
ALARMS_L1='level="1" cause="idle_open_work"'
ALARMS_L2='level="2" cause="idle_open_work"'
ALARMS_MODEL="$PUPPET/model.jsonl"

# The Giga's habit tells the Mono `$1` (no ask, so the Mono holds no open work).
alarms_habit_rule() {
  jq -n --arg go "$1" '{match: "AL-HABIT", calls: [{tool: "send_message", arguments:
    {message: $go, send_to: "alarms-mono", wanting_reply: false, intent: "new"}}], reply: "Sent."}'
}

# alarms_pair <part> <rules> [installer]: a fresh Giga and Mono with the
# puppet's rules, the Giga's habit due, and the logs kept for the e2e
# artifact. The habit's daily time is 12 h away: at the default 09:00, a
# run started just before 09:00 ran the habit again once the fake clock
# passed 09:00, and the Mono started every session twice.
alarms_pair() {
  local habits="$APP_DATA/monos/alarms-giga/habits.json"
  fresh_pair alarms "${@:3}"
  jq -n --arg app "$(target_bin instance)" --argjson rules "$2" '{app: $app, rules: $rules}' >"$PUPPET/script.json"
  request_habit alarms-giga "Start the work" "AL-HABIT: tell Alarms Mono to start."
  jq --arg time "$(printf '%02d:00' $(((10#$(date +%H) + 12) % 24)))" '.[0].schedule.time = $time' "$habits" >"$habits.tmp"
  mv "$habits.tmp" "$habits"
  : >"$ALARMS_MODEL"
  ALARMS_PART="$1"
  trap 'cp "$PUPPET/log.jsonl" "$EVAL_OUT/alarms-$ALARMS_PART-puppet.log"; cp "$ALARMS_MODEL" "$EVAL_OUT/alarms-$ALARMS_PART-model.log"
    [[ -z "${ALARMS_MODEL_PID:-}" ]] || kill "$ALARMS_MODEL_PID" 2>/dev/null' EXIT
}

# alarms_model <rules.json>: the local model on its own port, before any
# agent turn.
alarms_model() {
  printf '%s' "$1" >"$PUPPET/model-rules.json"
  python3 -I "$ALARMS_DIR/alarms-model.py" "$PUPPET/model-port" "$PUPPET/model-rules.json" "$ALARMS_MODEL" &
  ALARMS_MODEL_PID=$!
  wait_until 10 "the local model listens" test -s "$PUPPET/model-port"
}

# The real Claude Code CLI as the temp HOME's claude, on the local model, with
# its config in a folder the app never sees as ~/.claude (d4.test.sh).
ALARMS_CLAUDE=""
alarms_claude_on_path() {
  ALARMS_CLAUDE="$(realpath "$(command -v claude)")" || die "alarms needs the claude CLI on PATH"
  PATH="$EVAL_HOME/.local/bin:$PATH"
  export CLAUDE_CONFIG_DIR="$PUPPET/claude-config"
}
install_alarms_claude() {
  local bin="$EVAL_HOME/.local/bin/claude"
  mkdir -p "$(dirname "$bin")"
  { echo '#!/usr/bin/env bash'
    printf 'REAL=%q\nPORT=%q\n' "$ALARMS_CLAUDE" "$PUPPET/model-port"
    cat <<'SH'
[[ "${1:-}" == --version ]] && exec "$REAL" "$@"
export ANTHROPIC_BASE_URL="http://127.0.0.1:$(cat "$PORT")" ANTHROPIC_API_KEY=sk-fake-local \
  DISABLE_AUTOUPDATER=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1
unset ANTHROPIC_AUTH_TOKEN CLAUDE_CODE_OAUTH_TOKEN
exec "$REAL" "$@"
SH
  } >"$bin"
  chmod +x "$bin"
}

# omp from PATH (e2e.yml installs it) under the temp HOME, with the local
# model as provider `alarms` (models.yml, as the P2 probe ran it) and as
# omp's default model. models.list lists omp's own catalog only once the
# app probed it, which it does for an omp tab, so a Mono gets omp:default:
# omp's default model, pinned here. A script, not a link: the temp HOME
# refuses links that leave /tmp (ADE-112).
install_alarms_omp() {
  local omp
  omp="$(command -v omp)" || die "alarms omp needs omp on PATH"
  mkdir -p "$EVAL_HOME/.local/bin" "$EVAL_HOME/.omp/agent"
  printf '#!/usr/bin/env bash\nexec %q "$@"\n' "$omp" >"$EVAL_HOME/.local/bin/omp"
  chmod +x "$EVAL_HOME/.local/bin/omp"
  cat >"$EVAL_HOME/.omp/agent/models.yml" <<YML
providers:
  alarms:
    baseUrl: http://127.0.0.1:$(cat "$PUPPET/model-port")/v1
    auth: none
    api: openai-completions
    models:
      - id: alarms-model
        name: Alarms
        reasoning: false
        input: [text]
        contextWindow: 128000
        maxTokens: 4096
YML
  printf 'modelRoles:\n  default: alarms/alarms-model\n' >"$EVAL_HOME/.omp/agent/config.yml"
}

# The session that the start_session message matching `$1` started.
started() { q "SELECT to_session_id FROM mono_messages WHERE tool = 'start_session' AND $1 ORDER BY created_at LIMIT 1"; }
# Sets lead and worker once the Mono started the lead and the lead its worker.
alarms_lead_and_worker() {
  wait_until 120 "the Mono starts the lead" \
    holds "SELECT count(*) = 1 FROM mono_messages WHERE tool = 'start_session' AND from_mono_id = 'alarms-mono'"
  lead="$(started "from_mono_id = 'alarms-mono'")"
  wait_until 120 "the lead starts its worker" \
    holds "SELECT count(*) = 1 FROM mono_messages WHERE tool = 'start_session' AND from_session_id = '$lead'"
  worker="$(started "from_session_id = '$lead'")"
}
failed_calls() { jq -s '[.[] | select((.tool or .action) and .result.ok != true)] | length' "$PUPPET/log.jsonl"; }
episode_of() { q "SELECT id FROM alarm_episodes WHERE actor = 'session:$1' ORDER BY opened_active_ms DESC LIMIT 1"; }
sends_at() { q "SELECT count(*) FROM alarm_sends WHERE episode_id = '$1' AND level = $2"; }

# alarm_turns <rule> <episode>: "<prompts> <steers>" of rule <rule> that name
# <episode>; a steer would mean the alarm did not start its own turn.
alarm_turns() {
  jq -rs --arg rule "$1" --arg id "alarm=\"$2\"" \
    '[.[] | select(.rule == $rule and (.prompt | contains($id)))] | "\(length) \(map(select(.steer)) | length)"' "$PUPPET/log.jsonl"
}

# told <rule> <session>: a prompt of rule <rule> names the session's
# episode. The Mono may stall at the same pass and alarm the Giga, so a bare
# `saw <rule>` could be the Giga's prompt.
told() {
  local ep
  ep="$(episode_of "$2")"
  [[ -n "$ep" && "$(alarm_turns "$1" "$ep")" != "0 "* ]]
}

# What the agent sent the model back for tool call `$1`, once it did
# ("error: ..." when the harness marked it an error).
returned() {
  jq -rs --arg id "$1" 'map(.results[$id] // empty) | first // empty' "$ALARMS_MODEL"
}
has_returned() { jq -se --arg id "$1" 'map(.results | has($id)) | any' "$ALARMS_MODEL" >/dev/null; }
errored() { [[ "$(returned "$1")" == error:* ]] && echo yes || echo no; }

# advance_to <active ms>: the fake clock on to at least that active time, in
# whole 5 s steps (a shorter step is not stored, see advance).
advance_to() {
  local left=$(($1 - $(active_ms)))
  ((left > 0)) || return 0
  advance $(((left + 4999) / 5000 * 5000))
}

# advance_until <ms> <what> <command...>: the fake clock on in 5 s steps, one
# detector pass each, until the command holds; at most <ms>. A turn's end is
# stamped up to a tick past the clock's last step, so a stall can open one
# pass later than the whole minutes.
advance_until() {
  local left="$1" what="$2"
  shift 2
  until "$@"; do
    ((left > 0)) || die "not within the active time given: $what"
    advance 5000
    left=$((left - 5000))
  done
}

alarms_readings() {
  q "SELECT '  episode ' || id || ' ' || actor || ' ' || cause || ' L' || level || ' ' || ifnull(close_reason, 'open')
       || ' acted ' || ifnull(acted_how, '-') FROM alarm_episodes ORDER BY opened_active_ms"
  q "SELECT '  send ' || episode_id || ' L' || level || ' -> ' || recipient || ' sent ' || (sent_wall_ms IS NOT NULL)
       FROM alarm_sends ORDER BY created_active_ms"
  q "SELECT '  hold ' || id || ' ' || harness || ' ' || tool_use_id || ' ' || command || ' ' || ifnull(decision, 'open')
       || ' ' || ifnull(decided_by, '-') FROM sleep_holds ORDER BY requested_active_ms"
}

# lead_rules <alarm rules> <worker start>: the alarm rules first (an alarm
# names the worker and the lead by their first prompts), then the Mono
# starting a lead, which acks its start ask and starts the worker. The worker
# never acks its own start ask.
lead_rules() {
  jq -n --argjson alarm "$1" --argjson worker "$2" --argjson fx "$ALARMS_FX" \
    --argjson habit "$(alarms_habit_rule "AL-GO Start the lead.")" '$alarm + [
    {match: "AL-LED", reply: "Noted."},
    $habit,
    {match: "AL-GO", calls: [{action: "sessions.start", arguments: ($fx + {prompt: "AL-LEAD You lead the task."})}],
      reply: "Started."},
    {match: "AL-WORK", reply: "Working."},
    {match: "AL-LEAD", calls: [
      {tool: "ack", arguments: {ask_id: "$ask_id", message: "AL-LED Ready to lead."}},
      {action: "sessions.start", arguments: $worker}], reply: "Ready."}]'
}

# Design 14 E2E 1 and 2: the fx worker idles with its start ask open.
run_alarms_stall() {
  local lead worker ep
  alarms_pair stall "$(lead_rules "$(jq -n --arg l1 "$ALARMS_L1" --arg l2 "$ALARMS_L2" \
    '[{match: $l2, reply: "Seen."}, {match: $l1, reply: "Seen."}]')" \
    "$(jq -n --argjson fx "$ALARMS_FX" '$fx + {prompt: "AL-WORK You do the task."}')")"
  "$INSTANCE" start
  alarms_lead_and_worker
  wait_until 60 "the Mono gets the lead's start ack" saw AL-LED
  wait_until 60 "the worker's first turn" saw AL-WORK
  wait_until 60 "every turn ends" holds "SELECT count(*) = 0 FROM in_flight_sessions"
  echo "alarms stall readings (lead $lead, worker $worker):"
  advance $((14 * 60000))
  expect "worker alarms after 14 idle min" "$(q "SELECT count(*) FROM alarm_episodes WHERE actor = 'session:$worker'")" 0
  advance_until 70000 "the worker's alarm opens at 15 idle min" \
    holds "SELECT count(*) = 1 FROM alarm_episodes WHERE actor = 'session:$worker'"
  wait_until 60 "the lead's chat gets the worker's alarm" told "$ALARMS_L1" "$worker"
  ep="$(episode_of "$worker")"
  expect "worker's alarm: cause, level-1 recipient" \
    "$(q "SELECT e.cause || ' ' || s.recipient FROM alarm_episodes e JOIN alarm_sends s ON s.episode_id = e.id AND s.level = 1 WHERE e.id = '$ep'")" \
    "idle_open_work session:$lead"
  expect "worker's alarm opened 15 min after its turn ended" \
    "$(q "SELECT opened_active_ms - stalled_since_active_ms BETWEEN 900000 AND 905000 FROM alarm_episodes WHERE id = '$ep'")" 1
  expect "lead's alarm prompts: turns, steers" "$(alarm_turns "$ALARMS_L1" "$ep")" "1 0"
  wait_until 60 "the lead's alarm turn ends" holds "SELECT count(*) = 0 FROM in_flight_sessions WHERE session_id = '$lead'"
  advance $((14 * 60000))
  expect "level-2 sends after the lead's 14 min with no act" "$(sends_at "$ep" 2)" 0
  advance_until 90000 "the alarm goes up a level at 15 min with no act" \
    holds "SELECT count(*) = 1 FROM alarm_sends WHERE episode_id = '$ep' AND level = 2"
  wait_until 60 "the Mono's chat gets the level-2 alarm" told "$ALARMS_L2" "$worker"
  expect "level-2 recipient" "$(q "SELECT recipient FROM alarm_sends WHERE episode_id = '$ep' AND level = 2")" mono:alarms-mono
  expect "Mono's level-2 prompts: turns, steers" "$(alarm_turns "$ALARMS_L2" "$ep")" "1 0"
  expect "worker's alarm: open, acted by, level" \
    "$(q "SELECT (closed_active_ms IS NULL) || ' ' || ifnull(acted_by, 'none') || ' ' || level FROM alarm_episodes WHERE id = '$ep'")" \
    "1 none 2"
  expect "lead's level-1 prompts after 30 min" "$(alarm_turns "$ALARMS_L1" "$ep")" "1 0"
  advance 5000
  expect "Mono's level-2 prompts one pass later" "$(alarm_turns "$ALARMS_L2" "$ep")" "1 0"
  expect "puppet calls that failed" "$(failed_calls)" 0
  alarms_readings
  "$INSTANCE" stop
}

# Design 14 E2E 3: a supervised Claude worker, whose one Bash call needs the
# user's permission.
run_alarms_dialog() {
  local lead worker ep since file="$EVAL_HOME/$SCRATCH_NAME/alarms-dialog.txt" call=toolu_al_dialog_0
  alarms_claude_on_path
  alarms_pair dialog "$(lead_rules '[{"match": "cause=\"dialog\"", "calls": [{"action": "alarms.answer",
      "arguments": {"alarm": "$alarm", "allow": true}}], "reply": "Answered."}]' \
    '{"prompt": "AL-DIALOG Touch the file.", "harness": "claude", "model": "$model:claude:haiku",
      "runtimeMode": "supervised", "notifyOnComplete": false}')" install_alarms_claude
  alarms_model "$(jq -n --arg file "$file" '[{match: "AL-DIALOG", tools: [{name: "Bash",
    input: {command: "touch \($file | @sh)", description: "Touch the file"}}]}]')"
  "$INSTANCE" start
  alarms_lead_and_worker
  echo "alarms dialog readings (lead $lead, worker $worker):"
  wait_until 120 "the worker's model call" grep -qF "\"$call\"" "$ALARMS_MODEL"
  # The tap's dialog row is written by the detector's pass, once per 5 s.
  advance_until 60000 "the worker's permission dialog is recorded" \
    holds "SELECT dialog_kind = 'permission' FROM agent_activity WHERE session_id = '$worker'"
  since="$(q "SELECT dialog_since_active_ms FROM agent_activity WHERE session_id = '$worker'")"
  advance_to $((since + 170000))
  expect "worker alarms 2:50 into its dialog" "$(q "SELECT count(*) FROM alarm_episodes WHERE actor = 'session:$worker'")" 0
  advance_until 20000 "the dialog alarm opens at 3 min" \
    holds "SELECT count(*) = 1 FROM alarm_episodes WHERE actor = 'session:$worker'"
  wait_until 60 "the lead's chat gets the dialog alarm" told 'cause="dialog"' "$worker"
  ep="$(episode_of "$worker")"
  expect "dialog alarm: cause, recipient" \
    "$(q "SELECT e.cause || ' ' || s.recipient FROM alarm_episodes e JOIN alarm_sends s ON s.episode_id = e.id AND s.level = 1 WHERE e.id = '$ep'")" \
    "dialog session:$lead"
  expect "dialog alarm names the permission and the command" \
    "$(jq -rs --arg id "alarm=\"$ep\"" '[.[] | select(.rule == "cause=\"dialog\"" and (.prompt | contains($id)))]
      | first | .prompt | (contains("Dialog (permission") and contains("touch"))' "$PUPPET/log.jsonl")" true
  wait_until 60 "the lead answers the dialog" holds_log '[.[] | select(.action == "alarms.answer")] | length == 1'
  expect "alarms.answer --allow: ok" \
    "$(jq -rs 'first(.[] | select(.action == "alarms.answer") | .result.ok) | tostring' "$PUPPET/log.jsonl")" true
  wait_until 90 "the worker runs its command" test -e "$file"
  wait_until 90 "the worker sends the model its result" has_returned "$call"
  expect "worker's Bash result: an error" "$(errored "$call")" no
  advance_until 15000 "the dialog alarm closes" holds "SELECT close_reason IS NOT NULL FROM alarm_episodes WHERE id = '$ep'"
  expect "dialog alarm: closed as, acts" \
    "$(q "SELECT ifnull(close_reason, 'open') || ' ' || (SELECT group_concat(how || ' by ' || actor) FROM alarm_acts WHERE episode_id = '$ep') FROM alarm_episodes WHERE id = '$ep'")" \
    "progress answer by session:$lead"
  expect "puppet calls that failed" "$(failed_calls)" 0
  alarms_readings
  "$INSTANCE" stop
}

# held <seconds>: a foreground wait the hold catches. Claude Code itself
# refuses a command that starts with `sleep N`, before its PreToolUse hook
# runs, so the sleep comes second, for both harnesses.
held() { printf 'echo hold && sleep %s' "$1"; }

# hold_rules <start>: the Mono starts the first session, denies its hold
# (held 31) and starts the second, allows that one's (held 32) and starts
# the third, and leaves the third's (held 33) unanswered. omp gives no
# reason for a call, so each hold is told apart by its command.
hold_rules() {
  jq -n --argjson start "$1" --argjson habit "$(alarms_habit_rule "HOLD-GO Start the first session.")" \
    --arg deny "Command: $(held 31)" --arg allow "Command: $(held 32)" --arg timeout "Command: $(held 33)" '[
    {match: $deny, calls: [
      {action: "holds.deny", arguments: {hold: "$hold", reason: "HOLD-DENIED run it in the background"}},
      {action: "sessions.start", arguments: ($start + {prompt: "HOLD-TWO Wait for the build."})}], reply: "Denied."},
    {match: $allow, calls: [
      {action: "holds.allow", arguments: {hold: "$hold"}},
      {action: "sessions.start", arguments: ($start + {prompt: "HOLD-THREE Wait for the deploy."})}], reply: "Allowed."},
    {match: $timeout, reply: "Not now."},
    $habit,
    {match: "HOLD-GO", calls: [{action: "sessions.start", arguments: ($start + {prompt: "HOLD-ONE Wait for the tests."})}],
      reply: "Started."}]'
}

# hold_decision <tool call>: the hold row of that call, "<command> <decision> <by>".
hold_decision() {
  q "SELECT command || ' ' || ifnull(decision, 'open') || ' ' || ifnull(decided_by, '-') FROM sleep_holds WHERE tool_use_id = '$1'"
}

# advance_with_hold <tool call> <active ms>: the fake clock on toward that
# active time while the call's hold is open, never past the end of the
# hold client's current wait. The client's waits run in real time, so the
# clock goes on only as the client sends each next wait; a clock ahead of
# the client would close the hold as a stopped client (3 s idle), not at
# the cut. It stops within 5 s of the time, as a shorter step is not stored.
advance_with_hold() {
  local open="SELECT decision IS NULL FROM sleep_holds WHERE tool_use_id = '$1'" until to
  while (($2 - $(active_ms) >= 5000)) && holds "$open"; do
    until="$(q "SELECT ifnull(waiting_until_active_ms, 0) FROM sleep_holds WHERE tool_use_id = '$1'")"
    to=$((until < $2 ? until : $2))
    if ((to - $(active_ms) >= 5000)); then
      advance $((to - $(active_ms)))
    else
      wait_until 40 "the hold client sends its next wait" \
        holds "SELECT waiting_until_active_ms > $until OR decision IS NOT NULL FROM sleep_holds WHERE tool_use_id = '$1'"
    fi
  done
}

# check_holds <harness> <deny call>: the three holds of hold_rules, each
# through the harness's real hook. The third hold is held while the allowed
# wait still sleeps: the clock must follow its client from the first wait
# on, so the allowed wait's return is checked last.
check_holds() {
  local harness="$1" deny="$2" allow=toolu_hold_two_0 timeout=toolu_hold_three_0 cut
  wait_until 180 "the denied wait reaches the agent" has_returned "$deny"
  expect "$harness deny: hold" "$(hold_decision "$deny")" "$(held 31) deny mono:alarms-mono"
  expect "$harness deny: the agent sees the refusal" \
    "$(returned "$deny" | grep -oF "Refused: foreground wait \"$(held 31)\" was not allowed (denied: HOLD-DENIED run it in the background)")" \
    "Refused: foreground wait \"$(held 31)\" was not allowed (denied: HOLD-DENIED run it in the background)"
  wait_until 120 "the second wait is answered" \
    holds "SELECT decision IS NOT NULL FROM sleep_holds WHERE tool_use_id = '$allow'"
  expect "$harness allow: hold" "$(hold_decision "$allow")" "$(held 32) allow mono:alarms-mono"
  wait_until 120 "the third wait is held" holds "SELECT count(*) = 1 FROM sleep_holds WHERE tool_use_id = '$timeout'"
  wait_until 60 "the Mono gets the third hold and does not answer" saw "Command: $(held 33)"
  cut="$(q "SELECT deadline_active_ms FROM sleep_holds WHERE tool_use_id = '$timeout'")"
  expect "$harness timeout: answer window" "$(q "SELECT $cut - requested_active_ms FROM sleep_holds WHERE tool_use_id = '$timeout'")" 115000
  advance_with_hold "$timeout" $((cut - 15000))
  expect "$harness timeout: hold 15 s before the cut" "$(hold_decision "$timeout")" "$(held 33) open -"
  expect "$harness timeout: the agent still waits 15 s before the cut" "$(has_returned "$timeout" && echo returned || echo waiting)" waiting
  advance $((cut - $(active_ms)))
  wait_until 60 "the unanswered wait is refused at the cut" has_returned "$timeout"
  expect "$harness timeout: hold" "$(hold_decision "$timeout")" "$(held 33) timeout -"
  expect "$harness timeout: closed for, at the cut" \
    "$(q "SELECT decision_reason || ' ' || (decided_active_ms >= deadline_active_ms) FROM sleep_holds WHERE tool_use_id = '$timeout'")" \
    "no answer in 2 min 1"
  expect "$harness timeout: the agent sees the refusal" \
    "$(returned "$timeout" | grep -oF "Refused: foreground wait \"$(held 33)\" was not allowed (no answer in 2 min)")" \
    "Refused: foreground wait \"$(held 33)\" was not allowed (no answer in 2 min)"
  wait_until 180 "the allowed wait runs and returns" has_returned "$allow"
  expect "$harness allow: the wait ran: error, refusals, output" \
    "$(errored "$allow") $(returned "$allow" | grep -c 'Refused:' || true) $(returned "$allow" | grep -qw hold && echo hold)" "no 0 hold"
}

# Design 14 E2E 4 and 5 on Claude Code. The Mono's chat is full-access, so
# its full-access sessions run Bash with no permission dialog.
run_alarms_hold() {
  local start='{"harness": "claude", "model": "$model:claude:haiku", "runtimeMode": "full-access", "notifyOnComplete": false}'
  alarms_claude_on_path
  alarms_pair hold "$(hold_rules "$start")" install_alarms_claude
  sqlite3 "$DB" "UPDATE sessions SET runtime_mode = 'full-access'
    WHERE id = (SELECT json_extract(data_json, '\$.sessionId') FROM monos WHERE id = 'alarms-mono')"
  alarms_model "$(jq -n --arg deny "$(held 31)" --arg allow "$(held 32)" --arg timeout "$(held 33)" '[
    {match: "HOLD-ONE", tools: [
      {name: "Bash", input: {command: "echo start && sleep 30 &", description: "Wait in the background"}},
      {name: "Bash", input: {command: "sleep 30", description: "Wait as a background task", run_in_background: true}},
      {name: "Bash", input: {command: $deny, description: "Wait for the tests"}}]},
    {match: "HOLD-TWO", tools: [{name: "Bash", input: {command: $allow, description: "Wait for the build"}}]},
    {match: "HOLD-THREE", tools: [{name: "Bash", input: {command: $timeout, description: "Wait for the deploy"}}]}]')"
  "$INSTANCE" start
  echo "alarms hold readings (Claude Code):"
  wait_until 180 "the first session's background waits return" has_returned toolu_hold_one_1
  expect "echo start && sleep 30 &: held, error, refused" \
    "$(q "SELECT count(*) FROM sleep_holds WHERE tool_use_id = 'toolu_hold_one_0'") $(errored toolu_hold_one_0) $(returned toolu_hold_one_0 | grep -c 'Refused:' || true)" "0 no 0"
  expect "run_in_background sleep 30: held, error, refused" \
    "$(q "SELECT count(*) FROM sleep_holds WHERE tool_use_id = 'toolu_hold_one_1'") $(errored toolu_hold_one_1) $(returned toolu_hold_one_1 | grep -c 'Refused:' || true)" "0 no 0"
  check_holds claude toolu_hold_one_2
  expect "holds the three foreground waits made" "$(q "SELECT count(*) FROM sleep_holds")" 3
  expect "puppet calls that failed" "$(failed_calls)" 0
  alarms_readings
  "$INSTANCE" stop
}

# Design 14 E2E 6: the same three holds on omp.
run_alarms_omp() {
  local start='{"harness": "omp", "model": "$model:omp:default", "notifyOnComplete": false}' sessions
  alarms_pair omp "$(hold_rules "$start")"
  sqlite3 "$DB" "UPDATE sessions SET runtime_mode = 'full-access'
    WHERE id = (SELECT json_extract(data_json, '\$.sessionId') FROM monos WHERE id = 'alarms-mono')"
  alarms_model "$(jq -n --arg deny "$(held 31)" --arg allow "$(held 32)" --arg timeout "$(held 33)" '[
    {match: "HOLD-ONE", tools: [{name: "bash", input: {command: $deny}}]},
    {match: "HOLD-TWO", tools: [{name: "bash", input: {command: $allow}}]},
    {match: "HOLD-THREE", tools: [{name: "bash", input: {command: $timeout}}]}]')"
  install_alarms_omp
  "$INSTANCE" start
  echo "alarms hold readings (omp):"
  wait_until 120 "the Mono starts the first omp session" \
    holds "SELECT count(*) = 1 FROM mono_messages WHERE tool = 'start_session' AND from_mono_id = 'alarms-mono'"
  check_holds omp toolu_hold_one_0
  sessions="$(q "SELECT group_concat(DISTINCT model) FROM sessions WHERE id IN
    (SELECT to_session_id FROM mono_messages WHERE tool = 'start_session' AND from_mono_id = 'alarms-mono')")"
  expect "omp sessions: model, the model omp asked for" \
    "$sessions $(jq -rs 'map(.model) | unique | join(",")' "$ALARMS_MODEL")" "omp:default alarms-model"
  expect "omp hold harness" "$(q "SELECT group_concat(DISTINCT harness) FROM sleep_holds")" omp
  expect "puppet calls that failed" "$(failed_calls)" 0
  alarms_readings
  "$INSTANCE" stop
}

run_alarms() {
  FAILED=0
  log "alarms $1"
  case "$1" in
    stall) run_alarms_stall ;;
    dialog) run_alarms_dialog ;;
    hold) run_alarms_hold ;;
    omp) run_alarms_omp ;;
    *) die "usage: app.sh alarms stall|dialog|hold|omp" ;;
  esac
  return "$FAILED"
}
