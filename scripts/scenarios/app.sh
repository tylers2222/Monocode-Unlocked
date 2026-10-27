#!/usr/bin/env bash
# Real-app scenarios on a GigaEval instance with the fake-clock build and the
# puppet agent (scripts/scenarios/puppet/puppet.mjs) as fx. No model runs.
#
#   app.sh build [checkout]   build the instance (under heavy-lock.sh)
#   app.sh s0 [runs]          S0 through the real app, `runs` times (default 1)
#   app.sh d4 [d1]            D4 deny rules (d4.test.sh), off then on, with the
#                             claude CLI on PATH against puppet/fake-model.py;
#                             d1: with D1's srt too. In CI only,
#                             D4_CLAUDE=<real claude> runs one real model turn
#   app.sh d2                 D2 database encryption (d2.test.sh): off, on with
#                             the build's test key, then a wrong key, no key and
#                             a plaintext database on, each stopping the app
#   app.sh d1                 D1: the agent sandbox on, off, and on without srt
#   app.sh r6                 R6: agents ask and ack through MCP in the real app
#   app.sh r9                 R9: reminders reach an idle lead, a lead at its
#                             turn end and a lead across two kills, once each;
#                             R10: each offers one extension, used once
#   app.sh r11                R11: the deadline reminder, the parent's why
#                             notice and the lead's reason on its miss
#   app.sh approvals [runs]   ADE-55 approvals board, macOS only, on a copy
#                             of the real app database, `runs` times (default 1)
#
# GIGA_EVAL_OUT and GIGA_EVAL_HOME default to their own /tmp folders, apart
# from other GigaEval runs. The installed apps are never used; `approvals`
# reads the real app database once, read-only, through `sqlite3 .backup`.
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

# fresh_home: the instance stopped, a fresh temp HOME and the fake clock at
# uptime 0.
fresh_home() {
  "$INSTANCE" stop
  "$INSTANCE" home --fresh
  mkdir -p "$APP_DATA"
  UPTIME=0; WALL="$(date +%s)000"; BOOT_MS=$((WALL - 3600000))
  clock_write "$UPTIME" "$WALL"
}

# fresh_pair <scenario> [installer...]: a fresh temp HOME with the puppet as
# fx (and the installer's agent), launched once so the DB is created and
# migrated, then a Giga `<scenario>-giga` and a Mono `<scenario>-mono` on the
# scratch project, each with an fx chat.
fresh_pair() {
  fresh_home
  install_puppet
  "${@:2}"
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

# install_puppet_bin: the puppet as the temp HOME's fx, with an empty log.
install_puppet_bin() {
  mkdir -p "$EVAL_HOME/.local/bin" "$PUPPET"
  cp "$SCENARIOS/puppet/puppet.mjs" "$EVAL_HOME/.local/bin/fx"
  chmod +x "$EVAL_HOME/.local/bin/fx"
  : >"$PUPPET/log.jsonl"
}

install_puppet() {
  install_puppet_bin
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
      ["sqlite3", $db, "SELECT count(*) FROM mono_clock"],
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
  expect "sqlite3 on the DB is denied" "$(denied "sqlite3 $DB")" true
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
  expect "sqlite3 on the DB" "$(probe "sqlite3 $DB" '"\(.code) \(.stdout | rtrimstr("\n"))"')" "0 1"
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

# Attribute `$2` of the <mono_message ...> tag whose body starts with `$1`, in
# any prompt (empty when no tag has it). A turn can take several messages at
# once, in either order, so the tag is found by its body, not its place.
attribute() {
  jq -rs --arg key "$1" '[.[] | .prompt // empty | scan("<mono_message [^>]*>\n" + $key)] | first // ""' "$PUPPET/log.jsonl" \
    | grep -o '<mono_message [^>]*>' | sed -n "s/.* $2=\"\([^\"]*\)\".*/\1/p"
}

# Field `$2` of the answer to the call whose input message is `$1`.
answer() {
  jq -rs --arg message "$1" --arg field "$2" \
    'first(.[] | select(.input.message == $message) | .result[$field]) | tostring' "$PUPPET/log.jsonl"
}

# R6: the Giga asks the Mono, the Mono starts a lead and asks it with a
# 30-minute deadline, and each acks through MCP. The lead acks twice and the
# Mono acks its own ask; both refusals reach the puppet. The lead's worker acks
# once the lead has left the window (R6b review r1 M1). R7: the lead and the
# worker ack their start asks with the ask_id their first prompt carries. A
# session's tag names it by its first prompt, so its start ack carries
# R6-BRIEF or R6-WBRIEF too: the no-call R6-LED and R6-WLED rules come before
# those, and after R6-DONE and R6-WORKED, so a turn that takes a start ack
# with one of those still runs it. R6-DONE reads ack_of from its own tag.
R6_RULES='[
  {"match": "R6-HABIT", "calls": [{"tool": "ask", "arguments":
    {"message": "R6-PROJECT Run the project.", "send_to": "r6-mono"}}], "reply": "Asked."},
  {"match": "R6-REPORT", "reply": "Noted."},
  {"match": "R6-DONE", "calls": [{"tool": "ack", "arguments":
    {"ask_id": "$ack_of@R6-DONE", "message": "R6-MINE Acking my own ask."}}], "reply": "Checked."},
  {"match": "R6-LED", "reply": "Noted."},
  {"match": "R6-PROJECT", "calls": [
    {"action": "sessions.start", "arguments":
      {"prompt": "R6-BRIEF You lead the task.", "harness": "fx", "model": "fx:zai/glm-5.2-fast", "notifyOnComplete": false}},
    {"tool": "ask", "arguments": {"message": "R6-TASK Do the task.", "send_to": "$prev.id", "due_minutes": 30}},
    {"tool": "ack", "arguments": {"ask_id": "$ask_id", "message": "R6-REPORT The project runs."}}], "reply": "Started."},
  {"match": "R6-TASK", "calls": [
    {"tool": "ack", "arguments": {"ask_id": "$ask_id", "message": "R6-DONE The task is done."}},
    {"tool": "ack", "arguments": {"ask_id": "$ask_id", "message": "R6-AGAIN The task is done."}},
    {"action": "sessions.start", "arguments":
      {"prompt": "R6-WBRIEF You work on the task.", "harness": "fx", "model": "fx:zai/glm-5.2-fast", "notifyOnComplete": false}},
    {"tool": "ask", "arguments": {"message": "R6-WORK Do the work.", "send_to": "$prev.id"}}], "reply": "Done."},
  {"match": "R6-WORKED", "reply": "Noted."},
  {"match": "R6-WLED", "reply": "Noted."},
  {"match": "R6-WORK", "exec": [["sleep", "3"]], "calls": [
    {"tool": "ack", "arguments": {"ask_id": "$ask_id", "message": "R6-WORKED The work is done."}}], "reply": "Worked."},
  {"match": "R6-WBRIEF", "calls": [
    {"tool": "ack", "arguments": {"ask_id": "$ask_id", "message": "R6-WLED Ready to work."}}], "reply": "Ready."},
  {"match": "R6-BRIEF", "calls": [
    {"tool": "ack", "arguments": {"ask_id": "$ask_id", "message": "R6-LED Ready to lead."}}], "reply": "Ready."}]'

# The working-agents card off (a user setting, read at start): an idle session
# that no tab shows then leaves the window 250 ms after its turn.
live_agents_off() {
  local db
  db="$(find "$EVAL_HOME/Library/WebKit" -name localstorage.sqlite3 | head -1)"
  [[ -n "$db" ]] || die "no localStorage database under the temp HOME"
  # WebKit stores each value as UTF-16LE: X'3000' is "0".
  sqlite3 "$db" "INSERT OR REPLACE INTO ItemTable (key, value) VALUES ('monocode.liveAgentsEnabled', X'3000')"
}

run_r6() {
  FAILED=0
  log "R6 run"
  fresh_pair r6
  # The e2e job uploads $GIGA_EVAL_OUT/*.log: every prompt and call, pass or fail.
  trap 'cp "$PUPPET/log.jsonl" "$EVAL_OUT/r6-puppet.log"' EXIT
  # Active time before the asks, so a due_in that forgets to subtract it shows.
  "$INSTANCE" start
  advance 60000
  "$INSTANCE" stop
  live_agents_off
  jq -n --arg app "$(target_bin instance)" --argjson rules "$R6_RULES" '{app: $app, rules: $rules}' >"$PUPPET/script.json"
  request_habit r6-giga "Ask for the project" "R6-HABIT: ask R6 Mono to run the project."
  "$INSTANCE" start
  # R7: each start is an ask too; these are the asks the puppets sent.
  local sent="id NOT IN (SELECT id FROM mono_messages WHERE tool = 'start_session')"
  wait_until 120 "the Giga's and the Mono's asks are acked" \
    holds "SELECT count(*) = 2 FROM mono_asks WHERE state = 'acked' AND parent_actor IN ('mono:r6-giga', 'mono:r6-mono') AND $sent"
  wait_until 60 "the Giga gets the Mono's ack" saw R6-REPORT
  wait_until 60 "the Mono tries to ack its own ask" \
    holds_log '[.[] | select(.input.message == "R6-MINE Acking my own ask.")] | length == 1'
  wait_until 60 "the worker acks its idle lead" \
    holds_log '[.[] | select(.input.message == "R6-WORKED The work is done.")] | length == 1'
  wait_until 60 "the lead and the worker ack their start asks" \
    holds "SELECT count(*) = 2 FROM mono_asks WHERE state = 'acked' AND id IN (SELECT id FROM mono_messages WHERE tool = 'start_session')"
  local giga_ask mono_ask lead_ask lead worker lead_start worker_start
  giga_ask="$(q "SELECT id FROM mono_asks WHERE parent_actor = 'mono:r6-giga'")"
  mono_ask="$(q "SELECT id FROM mono_asks WHERE parent_actor = 'mono:r6-mono' AND $sent")"
  lead="$(q "SELECT to_session_id FROM mono_messages WHERE tool = 'start_session' AND from_mono_id = 'r6-mono'")"
  lead_ask="$(q "SELECT id FROM mono_asks WHERE parent_actor = 'session:$lead' AND $sent")"
  worker="$(q "SELECT to_session_id FROM mono_messages WHERE tool = 'start_session' AND from_session_id = '$lead'")"
  lead_start="$(q "SELECT id FROM mono_messages WHERE tool = 'start_session' AND to_session_id = '$lead'")"
  worker_start="$(q "SELECT id FROM mono_messages WHERE tool = 'start_session' AND to_session_id = '$worker'")"
  echo "R6 readings (Giga ask $giga_ask, Mono ask $mono_ask, lead $lead, lead ask $lead_ask, worker $worker, start asks $lead_start $worker_start):"
  expect "worker acks its idle lead: ok, code" \
    "$(answer "R6-WORKED The work is done." ok) $(answer "R6-WORKED The work is done." code)" "true null"
  wait_until 60 "the lead gets the worker's ack" saw R6-WORKED
  expect "the lead left the window before the ack: agent processes of R6-TASK and R6-WORKED" \
    "$(jq -s '[.[] | select(.rule == "R6-TASK" or .rule == "R6-WORKED") | .pid] | unique | length' "$PUPPET/log.jsonl")" 2
  expect "Giga ask: child, deadline, state" \
    "$(q "SELECT child_actor || ' ' || ifnull(due_active_ms, 'none') || ' ' || state FROM mono_asks WHERE id = '$giga_ask'")" \
    "mono:r6-mono none acked"
  expect "Mono ask: child, active ms left, state" \
    "$(q "SELECT child_actor || ' ' || (due_active_ms - (SELECT active_ms FROM mono_clock)) || ' ' || state FROM mono_asks WHERE id = '$mono_ask'")" \
    "session:$lead 1800000 acked"
  expect "lead ask: child, state" \
    "$(q "SELECT child_actor || ' ' || state FROM mono_asks WHERE id = '$lead_ask'")" "session:$worker acked"
  expect "start asks: child, active ms left, state" \
    "$(q "SELECT group_concat(child_actor || ' ' || (due_active_ms - (SELECT active_ms FROM mono_clock)) || ' ' || state, ', ') FROM (SELECT * FROM mono_asks WHERE NOT ($sent) ORDER BY parent_level)")" \
    "session:$lead 3600000 acked, session:$worker 3600000 acked"
  expect "lead's first prompt carries its start ask" "$(attribute R6-BRIEF ask_id) $(attribute R6-BRIEF due_in)" "$lead_start 60m"
  expect "worker's first prompt carries its start ask" "$(attribute R6-WBRIEF ask_id) $(attribute R6-WBRIEF due_in)" "$worker_start 60m"
  expect "Mono gets the lead's start ack with ack_of" "$(attribute R6-LED ack_of)" "$lead_start"
  expect "lead gets the worker's start ack with ack_of" "$(attribute R6-WLED ack_of)" "$worker_start"
  expect "ack events" "$(q "SELECT count(*) FROM mono_ask_events WHERE kind = 'ack'")" 5
  expect "Mono's ask arrives with ask_id" "$(attribute R6-PROJECT ask_id)" "$giga_ask"
  expect "Mono's ask from a Giga has no due_in" "$(attribute R6-PROJECT due_in)" ""
  expect "lead's ask arrives with ask_id" "$(attribute R6-TASK ask_id)" "$mono_ask"
  expect "lead's ask arrives with due_in" "$(attribute R6-TASK due_in)" "30m"
  expect "Mono gets the lead's ack with ack_of" "$(attribute R6-DONE ack_of)" "$mono_ask"
  expect "Giga gets the Mono's ack with ack_of" "$(attribute R6-REPORT ack_of)" "$giga_ask"
  expect "lead gets the worker's ack with ack_of" "$(attribute R6-WORKED ack_of)" "$lead_ask"
  expect "lead's ack" "$(answer "R6-DONE The task is done." ok)" true
  expect "lead's second ack: code" "$(answer "R6-AGAIN The task is done." code)" ack_refused
  expect "lead's second ack: hint" "$(answer "R6-AGAIN The task is done." hint)" \
    "This ask was already acked. A new ask from your parent has a new id."
  expect "Mono acks its own ask: ask_id" \
    "$(jq -rs 'first(.[] | select(.input.message == "R6-MINE Acking my own ask.") | .input.ask_id)' "$PUPPET/log.jsonl")" "$mono_ask"
  expect "Mono acks its own ask: code" "$(answer "R6-MINE Acking my own ask." code)" ack_refused
  expect "Mono acks its own ask: hint" "$(answer "R6-MINE Acking my own ask." hint)" \
    "This ask is not yours: only the Mono or session it was sent to can ack it."
  expect "lead prompts with the ask" "$(prompts R6-TASK)" 1
  expect "worker prompts with the ask" "$(prompts R6-WORK)" 1
  expect "calls that failed" "$(jq -s '[.[] | select((.tool or .action) and .result.ok != true)] | length' "$PUPPET/log.jsonl")" 2
  expect "steers" "$(jq -s '[.[] | select(.steer == true)] | length' "$PUPPET/log.jsonl")" 0
  q "SELECT '  ask ' || id || ' ' || parent_actor || ' -> ' || child_actor || ' ' || state FROM mono_asks ORDER BY created_wall_ms"
  "$INSTANCE" stop
  return "$FAILED"
}

# R9: the Giga asks the Mono, which starts three leads with a 10-minute start
# ask each and never acks. The idle lead's turn ends at once; the busy and
# late leads' first turns wait while $PUPPET/hold and $PUPPET/hold-late
# exist. The reminder rule comes first: each lead extends its one open ask
# twice (R10). R9_REMINDER is the real tag's prefix: the Mono rules text names
# a bare `<mono_reminder>`, so a looser match would answer every Mono prompt.
R9_REMINDER='<mono_reminder ask_id="'
r9_rules() {
  jq -n --arg hold "$PUPPET/hold" --arg late "$PUPPET/hold-late" --arg reminder "$R9_REMINDER" '[
    {match: $reminder, calls: [
      {tool: "extend", arguments: {}}, {tool: "extend", arguments: {}}],
      reply: "Noted."},
    {match: "R9-HABIT", calls: [{tool: "ask", arguments:
      {message: "R9-PROJECT Start the leads.", send_to: "r9-mono"}}], reply: "Asked."},
    {match: "R9-PROJECT", calls: [
      {action: "sessions.start", arguments: {prompt: "R9-IDLE You lead the first task.",
        harness: "fx", model: "fx:zai/glm-5.2-fast", notifyOnComplete: false, due_minutes: 10}},
      {action: "sessions.start", arguments: {prompt: "R9-BUSY You lead the second task.",
        harness: "fx", model: "fx:zai/glm-5.2-fast", notifyOnComplete: false, due_minutes: 10}},
      {action: "sessions.start", arguments: {prompt: "R9-LATE You lead the third task.",
        harness: "fx", model: "fx:zai/glm-5.2-fast", notifyOnComplete: false, due_minutes: 10}}],
      reply: "Started."},
    {match: "R9-IDLE", reply: "Ready."},
    {match: "R9-BUSY", exec: [["sh", "-c", "while [ -e \"$1\" ]; do sleep 0.2; done", "sh", $hold]], reply: "Ready."},
    {match: "R9-LATE", exec: [["sh", "-c", "while [ -e \"$1\" ]; do sleep 0.2; done", "sh", $late]], reply: "Ready."}]'
}

# Reminder prompts in the puppet log that name ask `$1`.
reminded() {
  jq -s --arg ask "ask_id=\"$1\"" --arg reminder "$R9_REMINDER" '[.[] | select(.rule == $reminder and (.prompt | contains($ask)))] | length' "$PUPPET/log.jsonl"
}

# 1 when the reminder prompt for ask `$1` is logged after the hold loop on
# file `$2` ended, that is after the lead's first turn replied.
reminded_after() {
  jq -s --arg ask "ask_id=\"$1\"" --arg hold "$2" --arg reminder "$R9_REMINDER" '
    (to_entries | map(select(.value.exec and (.value.exec | any(. == $hold))))[0].key) as $replied
    | (to_entries | map(select(.value.rule == $reminder and (.value.prompt | contains($ask))))[0].key) as $reminder_at
    | if $replied != null and $reminder_at != null and $reminder_at > $replied then 1 else 0 end' "$PUPPET/log.jsonl"
}

# S5.5: an idle lead wakes with its reminder as a new turn. A busy lead's
# reminder waits unclaimed (fx cannot take a steer): the late lead's turn
# ends while the app runs and its turn end delivers it. A kill on the busy
# lead, after the enqueue and before any claim, and a kill after the
# restart's claim and before the provider read the prompt, still give one
# delivery and one sent_at.
run_r9() {
  FAILED=0
  log "R9 run"
  fresh_pair r9
  trap 'cp "$PUPPET/log.jsonl" "$EVAL_OUT/r9-puppet.log"' EXIT
  jq -n --arg app "$(target_bin instance)" --argjson rules "$(r9_rules)" '{app: $app, rules: $rules}' >"$PUPPET/script.json"
  request_habit r9-giga "Ask for the leads" "R9-HABIT: ask R9 Mono to start the leads."
  touch "$PUPPET/hold" "$PUPPET/hold-late"
  "$INSTANCE" start
  wait_until 120 "the Mono starts three leads" \
    holds "SELECT count(*) = 3 FROM mono_asks WHERE parent_actor = 'mono:r9-mono' AND due_active_ms IS NOT NULL"
  wait_until 60 "the idle lead's turn" saw R9-IDLE
  wait_until 60 "the busy lead's turn" saw R9-BUSY
  wait_until 60 "the late lead's turn" saw R9-LATE
  local idle busy late idle_key busy_key late_key
  idle="$(q "SELECT id FROM mono_messages WHERE tool = 'start_session' AND body LIKE 'R9-IDLE%'")"
  busy="$(q "SELECT id FROM mono_messages WHERE tool = 'start_session' AND body LIKE 'R9-BUSY%'")"
  late="$(q "SELECT id FROM mono_messages WHERE tool = 'start_session' AND body LIKE 'R9-LATE%'")"
  idle_key="reminder:$idle:t5"; busy_key="reminder:$busy:t5"; late_key="reminder:$late:t5"
  echo "R9 readings (idle lead's ask $idle, busy lead's ask $busy, late lead's ask $late):"
  local dues
  dues="$(q "SELECT group_concat(due_active_ms, ' ') FROM (SELECT due_active_ms FROM mono_asks WHERE id IN ('$idle', '$busy', '$late') ORDER BY id)")"
  advance $((5 * 60000))
  wait_until 60 "the three reminders are enqueued" \
    holds "SELECT count(*) = 3 FROM mono_outbox WHERE key IN ('$idle_key', '$busy_key', '$late_key')"
  wait_until 60 "the idle lead's reminder is sent" \
    holds "SELECT sent_at IS NOT NULL FROM mono_outbox WHERE key = '$idle_key'"
  expect "idle lead: reminder prompts, as a new turn" \
    "$(reminded "$idle") $(jq -s --arg reminder "$R9_REMINDER" '[.[] | select(.rule == $reminder)] | first | .steer' "$PUPPET/log.jsonl")" "1 false"
  expect "busy lead: reminder waits for the turn end, unclaimed" \
    "$(q "SELECT ifnull(claimed_by, 'none') || ' ' || ifnull(sent_at, 'unsent') FROM mono_outbox WHERE key = '$busy_key'")" "none unsent"
  wait_until 60 "the idle lead extends twice" holds_log '[.[] | select(.tool == "extend")] | length == 2'
  expect "late lead: reminder waits for the turn end, unclaimed" \
    "$(q "SELECT ifnull(claimed_by, 'none') || ' ' || ifnull(sent_at, 'unsent') FROM mono_outbox WHERE key = '$late_key'")" "none unsent"
  # The late lead's turn ends while the app runs: its turn end delivers it.
  rm -f "$PUPPET/hold-late"
  wait_until 60 "the late lead's reminder is sent at its turn end" \
    holds "SELECT sent_at IS NOT NULL FROM mono_outbox WHERE key = '$late_key'"
  expect "late lead: reminder prompts" "$(reminded "$late")" 1
  expect "late lead: reminder after its first turn's reply" "$(reminded_after "$late" "$PUPPET/hold-late")" 1
  wait_until 60 "the late lead extends twice" holds_log '[.[] | select(.tool == "extend")] | length == 4'
  # Killed after the enqueue and before any claim. The next agent reads no
  # request while hold-start exists.
  touch "$PUPPET/hold-start"
  kill9
  rm -f "$PUPPET/hold"
  "$INSTANCE" start
  wait_until 60 "after the restart a window claims the busy lead's reminder" \
    holds "SELECT claimed_by IS NOT NULL AND sent_at IS NULL FROM mono_outbox WHERE key = '$busy_key'"
  expect "busy lead: no reminder read before the second kill" "$(reminded "$busy")" 0
  # Killed after the claim and before the provider read the prompt.
  kill9
  rm -f "$PUPPET/hold-start"
  "$INSTANCE" start
  wait_until 90 "the busy lead's reminder is sent" \
    holds "SELECT sent_at IS NOT NULL FROM mono_outbox WHERE key = '$busy_key'"
  expect "busy lead: reminder prompts" "$(reminded "$busy")" 1
  expect "idle lead: reminder prompts after both restarts" "$(reminded "$idle")" 1
  expect "late lead: reminder prompts after both restarts" "$(reminded "$late")" 1
  expect "reminders sent" "$(q "SELECT count(*) FROM mono_outbox WHERE sent_at IS NOT NULL")" 3
  # R10: each reminder offers the extension; each lead's first extend moves
  # its deadline 15 active minutes, its second is refused.
  wait_until 60 "the busy lead extends twice" holds_log '[.[] | select(.tool == "extend")] | length == 6'
  expect "reminders that offer the extension" \
    "$(jq -s --arg reminder "$R9_REMINDER" '[.[] | select(.rule == $reminder and (.prompt | contains("app messages.extend")))] | length' "$PUPPET/log.jsonl")" 3
  expect "extends: answers" \
    "$(jq -rs '[.[] | select(.tool == "extend") | .result.code // "ok"] | sort | join(" ")' "$PUPPET/log.jsonl")" \
    "extend_refused extend_refused extend_refused ok ok ok"
  expect "deadlines moved once by 15 active minutes" \
    "$(q "SELECT group_concat(due_active_ms, ' ') FROM (SELECT due_active_ms FROM mono_asks WHERE id IN ('$idle', '$busy', '$late') ORDER BY id)")" \
    "$(for due in $dues; do printf '%s ' $((due + 15 * 60000)); done | sed 's/ $//')"
  expect "extension events" "$(q "SELECT count(*) FROM mono_ask_events WHERE kind = 'extension'")" 3
  expect "steers" "$(jq -s '[.[] | select(.steer == true)] | length' "$PUPPET/log.jsonl")" 0
  q "SELECT '  outbox ' || key || ' ' || ifnull(claimed_by, '-') || ' ' || ifnull(sent_at, '-') FROM mono_outbox ORDER BY key"
  "$INSTANCE" stop
  return "$FAILED"
}

# R11 (S3 second half): the Mono starts one lead with a 10-minute start ask.
# The lead extends at its t5 reminder; at the new deadline its reason is
# refused (no miss yet). 5 active minutes later the Mono gets the why notice
# and asks the lead (send_to from the notice's child attribute); the lead
# stores its reason and acks with the ask_id the reason answered. The
# stage rules come first; R11-DONE before R11-LEAD, whose text names the
# lead in the tag of its ack.
R11_RULES='[
  {"match": "stage=\"why\"", "calls": [{"tool": "send_message", "arguments":
    {"message": "R11-WHY Why is the task late?", "send_to": "$child", "wanting_reply": false, "intent": "new"}}],
    "reply": "Asked why."},
  {"match": "stage=\"deadline\"", "calls": [{"tool": "reason", "arguments": {"message": "R11-EARLY Not missed yet."}}],
    "reply": "Noted."},
  {"match": "stage=\"t5\"", "calls": [{"tool": "extend", "arguments": {}}], "reply": "Noted."},
  {"match": "R11-DONE", "reply": "Noted."},
  {"match": "R11-WHY", "calls": [
    {"tool": "reason", "arguments": {"message": "R11-REASON The fixture build was slow."}},
    {"tool": "ack", "arguments": {"ask_id": "$prev.ask_id", "message": "R11-DONE The task is done."}}],
    "reply": "Answered."},
  {"match": "R11-HABIT", "calls": [{"tool": "ask", "arguments":
    {"message": "R11-PROJECT Start the lead.", "send_to": "r11-mono"}}], "reply": "Asked."},
  {"match": "R11-PROJECT", "calls": [{"action": "sessions.start", "arguments": {"prompt": "R11-LEAD You lead the task.",
    "harness": "fx", "model": "fx:zai/glm-5.2-fast", "notifyOnComplete": false, "due_minutes": 10}}],
    "reply": "Started."},
  {"match": "R11-LEAD", "reply": "Ready."}]'

run_r11() {
  FAILED=0
  log "R11 run"
  fresh_pair r11
  trap 'cp "$PUPPET/log.jsonl" "$EVAL_OUT/r11-puppet.log"' EXIT
  jq -n --arg app "$(target_bin instance)" --argjson rules "$R11_RULES" '{app: $app, rules: $rules}' >"$PUPPET/script.json"
  request_habit r11-giga "Ask for the lead" "R11-HABIT: ask R11 Mono to start the lead."
  "$INSTANCE" start
  wait_until 120 "the Mono starts the lead" \
    holds "SELECT count(*) = 1 FROM mono_asks WHERE parent_actor = 'mono:r11-mono' AND due_active_ms IS NOT NULL"
  wait_until 60 "the lead's turn" saw R11-LEAD
  local ask lead left
  ask="$(q "SELECT id FROM mono_asks WHERE parent_actor = 'mono:r11-mono'")"
  lead="$(q "SELECT to_session_id FROM mono_messages WHERE id = '$ask'")"
  echo "R11 readings (lead $lead, ask $ask):"
  advance $((5 * 60000))
  wait_until 60 "the lead extends at its t5 reminder" \
    holds "SELECT count(*) = 1 FROM mono_ask_events WHERE ask_id = '$ask' AND kind = 'extension'"
  left="$(q "SELECT due_active_ms - (SELECT active_ms FROM mono_clock WHERE id = 1) FROM mono_asks WHERE id = '$ask'")"
  expect "active ms to the extended deadline" "$left" 1200000
  # S3.3: at the new deadline, a second reminder to the lead.
  advance "$left"
  wait_until 60 "the lead's deadline reminder is sent" \
    holds "SELECT recipient = 'session:$lead' AND sent_at IS NOT NULL FROM mono_outbox WHERE key = 'reminder:$ask:deadline'"
  wait_until 60 "the lead tries a reason at the deadline" \
    holds_log '[.[] | select(.input.message == "R11-EARLY Not missed yet.")] | length == 1'
  expect "a reason before the miss: code" "$(answer "R11-EARLY Not missed yet." code)" reason_refused
  expect "misses before the why stage" "$(q "SELECT count(*) FROM mono_ask_events WHERE kind = 'miss'")" 0
  # S3.4: 5 active minutes later the Mono is told and asks why.
  advance $((5 * 60000))
  wait_until 60 "the Mono gets the why notice" saw 'stage="why"'
  # S3.5: still open after the miss; the lead's ack closes it.
  wait_until 60 "the lead acks after its reason" holds "SELECT state = 'acked' FROM mono_asks WHERE id = '$ask'"
  expect "why notice: recipient, sent" \
    "$(q "SELECT recipient || ' ' || (sent_at IS NOT NULL) FROM mono_outbox WHERE key = 'reminder:$ask:why'")" "mono:r11-mono 1"
  expect "why notice: names the lead, as a new turn" \
    "$(jq -rs --arg child "child=\"$lead\"" '[.[] | select(.rule == "stage=\"why\"")] | [length, (first | .prompt | contains($child)), (first | .steer)] | map(tostring) | join(" ")' "$PUPPET/log.jsonl")" \
    "1 true false"
  expect "the Mono's why message reaches the lead" "$(prompts R11-WHY)" 1
  expect "the lead's reason: ok" "$(answer "R11-REASON The fixture build was slow." ok)" true
  expect "miss: rows, reason" \
    "$(q "SELECT count(*) || ' ' || group_concat(reason) FROM mono_ask_events WHERE ask_id = '$ask' AND kind = 'miss'")" \
    "1 R11-REASON The fixture build was slow."
  expect "the lead's ack: ok" "$(answer "R11-DONE The task is done." ok)" true
  expect "reminders, each sent once" \
    "$(q "SELECT group_concat(stage || ':' || (sent_at IS NOT NULL), ' ') FROM (SELECT * FROM mono_outbox WHERE ask_id = '$ask' ORDER BY at_active_ms)")" \
    "t5:1 deadline:1 why:1"
  q "SELECT '  outbox ' || key || ' ' || recipient || ' ' || ifnull(sent_at, '-') FROM mono_outbox ORDER BY at_active_ms"
  "$INSTANCE" stop
  return "$FAILED"
}
source "$SCENARIOS/approvals.sh"

[[ "${1:-}" != approvals* || "$EVAL_OS" == macos ]] || die "approvals is macOS only (GIGA_EVAL_SANDBOX, sandbox-exec)"

case "${1:-}" in
  build) "$INSTANCE" build "${2:-$SCENARIOS/../..}" scenario ;;
  # One run per process: a run called in `||` would run with errexit off.
  s0-run) run_s0 "$2" ;;
  d1) run_d1 ;;
  r6) run_r6 ;;
  r9) run_r9 ;;
  r11) run_r11 ;;
  s0)
    failed=0
    for run in $(seq 1 "${2:-1}"); do "$0" s0-run "$run" || failed=$((failed + 1)); done
    log "S0: $(( ${2:-1} - failed ))/${2:-1} passed"
    ((failed == 0)) ;;
  d4) source "$SCENARIOS/d4.test.sh"; run_d4 "${2:-}" ;;
  d2) source "$SCENARIOS/d2.test.sh"; run_d2 ;;
  approvals-run) export GIGA_EVAL_SANDBOX=1 GIGA_EVAL_WATCH_NOTIFY=$$; run_approvals "$2" ;;
  approvals)
    APPROVALS_USER_BIN="${APPROVALS_USER_BIN:-$(user_bin)}"
    [[ -x "$APPROVALS_USER_BIN" ]] || die "no test binary for the user tool"
    export APPROVALS_USER_BIN
    failed=0
    for run in $(seq 1 "${2:-1}"); do "$0" approvals-run "$run" || failed=$((failed + 1)); done
    log "approvals: $(( ${2:-1} - failed ))/${2:-1} passed"
    ((failed == 0)) ;;
  *) sed -n '2,26p' "$0"; exit 2 ;;
esac
