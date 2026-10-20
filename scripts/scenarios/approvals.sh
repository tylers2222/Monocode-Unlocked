# Sourced by app.sh: ADE-55, the approvals board through the real app on a
# copy of the real app database. Gigas A, B and C and Mono M are puppets on
# fx; M starts a lead, the lead starts a worker. Every agent call goes
# through app-mcp or the `app` CLI with the identity the app gave it. The
# user's decisions and the Giga's removal go through the functions their
# commands call (`s2_user_acts_on_an_instance_database`), with the app
# stopped: the window is the user's only other way in.

GA=ade55-giga-a GB=ade55-giga-b GC=ade55-giga-c GM=ade55-mono
FX_MODEL="fx:zai/glm-5.2-fast"

# The test binary that holds the user tool; built once per `approvals` run.
user_bin() {
  local lock=() root="$SCENARIOS/../.."
  [[ -n "${HEAVY_LOCK:-}" ]] && lock=("$HEAVY_LOCK")
  ${lock[@]+"${lock[@]}"} cargo test --manifest-path "$root/src-tauri/Cargo.toml" --lib --no-run \
    --message-format=json 2>/dev/null \
    | jq -r 'select(.reason == "compiler-artifact" and .profile.test and .target.name == "monocode_lib") | .executable' \
    | tail -1
}

user() {
  MONOCODE_SCENARIO_USER_DB="$DB" MONOCODE_SCENARIO_USER_NOW="$WALL" MONOCODE_SCENARIO_USER_STEPS="$1" \
    "$APPROVALS_USER_BIN" --ignored --exact --nocapture scenario::s2_approvals::s2_user_acts_on_an_instance_database \
    | grep '"step":'
}

# The copy holds turns and sessions for the user's real agents. The instance
# runs under scripts/giga-eval/sandbox.sh (GIGA_EVAL_SANDBOX=1): the copy is
# scrubbed and every start is guarded (the primary protection), the home is
# fresh with no real login, and the app starts from `env -i` with a fixed
# PATH in a profile that lets it start only itself, node, the puppet and
# git, reach only localhost, and write only to the temp HOME and its own
# jail dir.
start_app() { "$INSTANCE" start; }

approvals_seed() {
  local scratch="$EVAL_HOME/$SCRATCH_NAME" now; now="$(date +%s)000"
  local sql="" id name role
  for id in $GA $GB $GC $GM; do
    role=giga; [[ "$id" == "$GM" ]] && role=mono
    name="ADE55 ${id#ade55-}"
    sql+="INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at) VALUES
      ('$id', '$name', '$role', json_object('name', '$name', 'sessionId', '$id-chat',
        'gigaSlug', CASE '$role' WHEN 'giga' THEN '$id' END, 'projects', json_array('$scratch')), 90, $now, $now);
    INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at) VALUES
      ('$id-chat', '$scratch', 'fx', '$FX_MODEL', 'auto', '$name', $now, $now);"
  done
  sqlite3 "$DB" "$sql"
  local giga habit
  for giga in "$GA:ADE55-A1" "$GC:ADE55-C1"; do
    habit="${giga#*:}"; giga="${giga%%:*}"
    mkdir -p "$APP_DATA/monos/$giga"
    jq -n --argjson now "$now" --arg habit "$habit" '[{id: "ade55", name: "ADE55", instructions: "\($habit): file the cards.",
      schedule: {scheduleKind: "daily"}, enabled: true, createdAt: $now, nextRunAt: ($now + 86400000), runRequested: true}]' \
      >"$APP_DATA/monos/$giga/habits.json"
  done
}

# The cards each Giga files, as file_approval takes them, in the instance's
# Giga workspace: a project on the rail of every run.
approval() {
  jq -n --arg project "$EVAL_HOME/MonoCode" --arg heading "$1" --arg summary "$2" --argjson options "${3:-[]}" \
    '{project: $project, heading: $heading, summary: $summary,
      tradeoffs: {pros: ["Unblocks the Mono"], cons: ["Costs a day"]},
      options: [$options[] | {label: .}], urgency: "blocking",
      links: [{kind: "pr", url: "https://github.com/tylers2222/Monocode-Unlocked/pull/1"}], origin: "ADE55 mono"}'
}

send() { jq -n --arg to "$1" --arg text "$2" '{tool: "send_message", arguments: {message: $text, send_to: $to, wanting_reply: false, intent: "new"}}'; }
file() { jq -n --argjson card "$(approval "$@")" '{tool: "file_approval", arguments: $card}'; }
start_session() {
  jq -n --arg prompt "$1" --arg model "$FX_MODEL" --arg rid "$2" '{action: "sessions.start", request_id: $rid,
    arguments: {prompt: $prompt, harness: "fx", model: $model, reveal: false, notifyOnComplete: false}}'
}

install_approvals_puppet() {
  mkdir -p "$EVAL_HOME/.local/bin" "$PUPPET"
  cp "$SCENARIOS/puppet/puppet.mjs" "$EVAL_HOME/.local/bin/fx"
  chmod +x "$EVAL_HOME/.local/bin/fx"
  : >"$PUPPET/log.jsonl"
  local host_update; host_update="$(approval "ADE55 Choose a host" "Updated by A" '["Fly","Railway"]' | jq 'del(.project) + {id: "$card:ADE55 Choose a host"}')"
  jq -n --arg app "$(target_bin instance)" \
    --argjson a1 "$(jq -s . <(file "ADE55 Ship the importer" "Ready to ship") <(file "ADE55 Drop the cache" "Cache is stale") \
      <(file "ADE55 Rename the CLI" "Name clash") <(file "ADE55 Choose a host" "Fly or Railway" '["Fly","Railway"]') \
      <(file "ADE55 Choose a host" "Fly, Railway or Render" '["Fly","Railway","Render"]') \
      <(file "ADE55 Withdraw me" "Filed to be withdrawn") <(send "$GM" "ADE55-M1 your turn"))" \
    --argjson m1 "$(jq -s . <(file "ADE55 Mono card" "A Mono files") <(start_session "ADE55-L1 lead" ade55-lead) \
      <(send "$GB" "ADE55-B1 your turn"))" \
    --argjson l1 "$(jq -s . <(file "ADE55 Lead card" "A lead files") <(start_session "ADE55-W1 worker" ade55-worker))" \
    --argjson w1 "$(jq -s . <(file "ADE55 Worker card" "A worker files"))" \
    --argjson b1 "$(jq -s --argjson update "$host_update" '. + [{tool: "update_approval", arguments: ($update + {summary: "B rewrites A"})},
        {tool: "withdraw_approval", arguments: {id: "$card:ADE55 Choose a host"}}]' \
      <(file "ADE55 Choose a host" "B wants this heading" '["Fly"]'))" \
    --argjson b2 "$(jq -s . <(file "ADE55 Pick a database" "Pick one or two" '["SQLite","Postgres","Server"]') <(send "$GA" "ADE55-A2 your turn"))" \
    --argjson a2 "$(jq -n --argjson update "$host_update" '[{tool: "update_approval", arguments: $update},
        {tool: "withdraw_approval", arguments: {id: "$card:ADE55 Withdraw me"}},
        {tool: "withdraw_approval", arguments: {id: "$card:ADE55 Withdraw me"}}]')" \
    --argjson c1 "$(jq -s . <(file "ADE55 Orphan card" "Filed by a Giga that gets removed"))" \
    '{app: $app, whoami: true, rules: [
      {match: "The user decided approval card", reply: "Noted."},
      {match: "ADE55-A1", calls: $a1, reply: "Filed."},
      {match: "ADE55-M1", calls: $m1, reply: "Escalated."},
      {match: "ADE55-L1", calls: $l1, reply: "Lead done."},
      {match: "ADE55-W1", calls: $w1, reply: "Worker done."},
      {match: "ADE55-B1", calls: ($b1 + $b2), reply: "Filed."},
      {match: "ADE55-A2", calls: $a2, reply: "Updated."},
      {match: "ADE55-C1", calls: $c1, reply: "Filed."}]}' >"$PUPPET/script.json"
}

# The code each logged call of rule `$1` answered, in order: "ok" or the
# refusal code.
codes() {
  jq -rs --arg rule "$1" '
    reduce .[] as $e ({rule: null, out: []};
      if $e.prompt then .rule = $e.rule
      elif .rule == $rule and ($e.tool // $e.action) then
        .out += [if $e.result.ok == true then "ok" else ($e.result.code // $e.result.error.code // $e.result.error // "?") end]
      else . end) | .out | join(" ")' "$PUPPET/log.jsonl"
}

# The decision prompts the puppet of Mono `$1` got: "<steers>:<cards>", the
# distinct steer flags of those prompts and how many cards they name. The
# window may hand several decisions over in one turn or one per turn.
decisions_to() {
  jq -rs --arg who "$1" '[.[] | select(.prompt and .who == $who and (.prompt | contains("The user decided approval card")))]
    | if length == 0 then "" else "\([.[].steer] | unique | map(tostring) | join(",")):\([.[].prompt | scan("APR-[0-9]+")] | length)" end' \
    "$PUPPET/log.jsonl"
}

# A read while the app is stopped: no writer, so no copy is needed.
qs() { sqlite3 "$DB" "$1"; }

decided_prompts() { jq -s '[.[] | select(.prompt and (.prompt | contains("The user decided approval card")))] | length' "$PUPPET/log.jsonl"; }

cards() {
  q "SELECT heading || '|' || status || '|' || coalesce(requested_by, '-') || '|' || requested_by_name || '|'
       || coalesce(decided_at, '-') || '|' || coalesce(withdrawn_at, '-') || '|' || coalesce(decision_note, '-')
       || '|' || coalesce(chosen_options, '-') || '|' || summary FROM approvals WHERE heading LIKE 'ADE55 %' ORDER BY number"
}

card_field() { q "SELECT $2 FROM approvals WHERE heading = '$1' ORDER BY number DESC LIMIT 1"; }
card_field_stopped() { qs "SELECT $2 FROM approvals WHERE heading = '$1' ORDER BY number DESC LIMIT 1"; }

run_approvals() {
  local run="$1"
  FAILED=0
  log "approvals run $run"
  "$INSTANCE" stop
  "$INSTANCE" home --fresh
  mkdir -p "$APP_DATA"
  UPTIME=0; WALL="$(date +%s)000"; BOOT_MS=$((WALL - 3600000))
  clock_write "$UPTIME" "$WALL"
  sqlite3 "file:$REAL_APP_DATA/monocode.db?mode=ro" ".backup '$DB'"
  install_approvals_puppet
  "$INSTANCE" scrub "$FX_MODEL"
  rm -f "$EVAL_OUT/sandbox-stray.txt"
  local since; since="$(date '+%Y-%m-%d %H:%M:%S')"
  trap 'echo "  FAIL a process other than the app and the puppet ran: $(cat "$EVAL_OUT/sandbox-stray.txt")"; exit 1' USR1
  local real_sessions refused_before
  real_sessions="$(sqlite3 "$DB" "SELECT count(*) FROM sessions")"
  refused_before="$(sqlite3 "$DB" "SELECT count(*) FROM mono_messages WHERE gate_result = 'not_a_giga'")"
  start_app
  wait_until 30 "the first launch writes mono_clock" holds "SELECT count(*) = 1 FROM mono_clock"
  "$INSTANCE" stop
  approvals_seed
  start_app
  wait_until 120 "A's second turn ends the chain" holds \
    "SELECT status = 'withdrawn' FROM approvals WHERE heading = 'ADE55 Withdraw me'"
  wait_until 60 "C files its card" holds "SELECT count(*) = 1 FROM approvals WHERE heading = 'ADE55 Orphan card'"
  wait_until 60 "the worker's call is logged" bash -c "[[ -n \"\$(jq -c 'select(.input.heading == \"ADE55 Worker card\")' '$PUPPET/log.jsonl')\" ]]"
  wait_until 30 "A's last call is logged" bash -c "[[ \$(jq -s '[.[] | select(.tool == \"withdraw_approval\")] | length' '$PUPPET/log.jsonl') == 3 ]]"
  echo "run $run readings (on a copy of the real database, $real_sessions sessions):"
  expect "A files six cards, one twice, then hands off" "$(codes ADE55-A1)" "ok ok ok ok ok ok ok"
  expect "filing the same heading again updates A's card" \
    "$(jq -rs '[.[] | select(.tool == "file_approval" and .input.heading == "ADE55 Choose a host" and .result.ok == true) | .result.result.id] | unique | length' "$PUPPET/log.jsonl")" 1
  expect "the Mono is refused, starts a lead, hands off" "$(codes ADE55-M1)" "not_a_giga ok ok"
  expect "the lead is refused, starts a worker" "$(codes ADE55-L1)" "not_a_giga ok"
  expect "the worker is refused" "$(codes ADE55-W1)" "not_a_giga"
  expect "B cannot take, update or withdraw A's card" "$(codes ADE55-B1)" "heading_taken not_your_approval not_your_approval ok ok"
  expect "A updates, withdraws, cannot withdraw again" "$(codes ADE55-A2)" "ok ok approval_not_open"
  expect "C files" "$(codes ADE55-C1)" "ok"
  expect "who each refused caller is" \
    "$(jq -rs '[.[] | select(.prompt and (.rule == "ADE55-M1" or .rule == "ADE55-L1" or .rule == "ADE55-W1")) | "\(.rule)=\(.who)"] | join(" ")' "$PUPPET/log.jsonl")" \
    "ADE55-M1=$GM ADE55-L1=null ADE55-W1=null"
  expect "no refused call wrote a card" \
    "$(q "SELECT count(*) FROM approvals WHERE heading IN ('ADE55 Mono card', 'ADE55 Lead card', 'ADE55 Worker card')")" 0
  expect "refusals on record" "$(( $(q "SELECT count(*) FROM mono_messages WHERE gate_result = 'not_a_giga'") - refused_before ))" 3
  expect "the worker's refusal names its lead" \
    "$(jq -rs '[.[] | select(.input.heading == "ADE55 Worker card") | .result.hint | test("your lead \\(")] | first' "$PUPPET/log.jsonl")" true
  expect "Choose a host: A's update, open" "$(card_field "ADE55 Choose a host" "status || '|' || summary || '|' || requested_by")" "open|Updated by A|$GA"
  expect "Withdraw me: withdrawn by A" "$(card_field "ADE55 Withdraw me" "status || '|' || (withdrawn_at >= created_at)")" "withdrawn|1"
  expect "each card's filer" \
    "$(q "SELECT group_concat(heading || '=' || requested_by, ', ') FROM (SELECT heading, requested_by FROM approvals WHERE heading LIKE 'ADE55 %' ORDER BY heading)")" \
    "ADE55 Choose a host=$GA, ADE55 Drop the cache=$GA, ADE55 Orphan card=$GC, ADE55 Pick a database=$GB, ADE55 Rename the CLI=$GA, ADE55 Ship the importer=$GA, ADE55 Withdraw me=$GA"
  expect "open cards" "$(q "SELECT count(*) FROM approvals WHERE status = 'open' AND heading LIKE 'ADE55 %'")" 6

  "$INSTANCE" stop
  WALL=$((WALL + 60000)); clock_write "$UPTIME" "$WALL"
  local decisions; decisions="$(user '[
    {"decide": "ADE55 Ship the importer", "verdict": "approved", "note": "Ship it Friday", "chosen": []},
    {"decide": "ADE55 Drop the cache", "verdict": "rejected", "note": "Keep the cache", "chosen": []},
    {"decide": "ADE55 Rename the CLI", "verdict": "changes_requested", "note": "Name it mc", "chosen": []},
    {"decide": "ADE55 Pick a database", "verdict": "approved", "note": "Both, SQLite first", "chosen": ["SQLite", "Server"]},
    {"remove_mono": "'"$GC"'"},
    {"decide": "ADE55 Orphan card", "verdict": "rejected", "note": "Handled by hand", "chosen": []}]')"
  expect "the user's six steps" "$(jq -s '[.[] | select(.done != null)] | length' <<<"$decisions")" 6
  expect "the removed Giga's card keeps its name, names no Giga" \
    "$(card_field_stopped "ADE55 Orphan card" "coalesce(requested_by, '-') || '|' || requested_by_name || '|' || status")" "-|ADE55 giga-c|rejected"
  expect "decision rows: to A, A, A, B" \
    "$(qs "SELECT group_concat(to_mono_id || ':' || delivery, ' ') FROM (SELECT to_mono_id, delivery FROM mono_messages WHERE from_session_id = 'approvals-board' ORDER BY created_at, rowid)")" \
    "$GA:queue $GA:queue $GA:queue $GB:queue"
  expect "nothing for the removed Giga's card" \
    "$(qs "SELECT count(*) FROM mono_messages WHERE request_id = (SELECT id FROM approvals WHERE heading = 'ADE55 Orphan card')")" 0

  start_app
  wait_until 60 "the Gigas take their decisions" holds \
    "SELECT count(*) = 4 FROM mono_messages WHERE from_session_id = 'approvals-board' AND delivered_at IS NOT NULL"
  wait_until 30 "the four decisions reach the puppets" bash -c \
    "[[ \$(jq -s '[.[] | select(.rule == \"The user decided approval card\") | .prompt | scan(\"APR-[0-9]+\")] | length' '$PUPPET/log.jsonl') -ge 4 ]]"
  local to_a to_b
  to_a="$(jq -rs --arg who "$GA" '[.[] | select(.prompt and .who == $who and .rule == "The user decided approval card") | .prompt] | join("\n")' "$PUPPET/log.jsonl")"
  to_b="$(jq -rs --arg who "$GB" '[.[] | select(.prompt and .who == $who and .rule == "The user decided approval card") | .prompt] | join("\n")' "$PUPPET/log.jsonl")"
  expect "A's decision prompts, never steered" "$(decisions_to "$GA")" "false:3"
  expect "B's decision prompts, never steered" "$(decisions_to "$GB")" "false:1"
  expect "C and M get no decision" "$(decisions_to "$GC")|$(decisions_to "$GM")" "|"
  expect "A reads approve with its comment" "$(grep -c 'decision: approved' <<<"$to_a")|$(grep -c 'comment: Ship it Friday' <<<"$to_a")" "1|1"
  expect "A reads reject with its comment" "$(grep -c 'decision: rejected' <<<"$to_a")|$(grep -c 'comment: Keep the cache' <<<"$to_a")" "1|1"
  expect "A reads ask for changes with its comment" "$(grep -c 'decision: changes_requested' <<<"$to_a")|$(grep -c 'comment: Name it mc' <<<"$to_a")" "1|1"
  expect "B reads the ticked options" "$(grep -c 'chosen_options: \["SQLite","Server"\]' <<<"$to_b")|$(grep -c 'comment: Both, SQLite first' <<<"$to_b")" "1|1"
  expect "A never reads B's card, B never reads A's" "$(grep -c 'Pick a database' <<<"$to_a")|$(grep -c 'Ship the importer' <<<"$to_b")" "0|0"

  local before sent; before="$(cards)"; sent="$(decided_prompts)"
  kill9
  WALL=$((WALL + 30000)); clock_write "$UPTIME" "$WALL"
  start_app
  wait_until 30 "the restart logs app_down" holds "SELECT count(*) >= 1 FROM mono_downtime WHERE cause = 'app_down'"
  advance 10000
  expect "every card and status after kill -9 and restart" "$(cards)" "$before"
  expect "no decision is sent again" "$(decided_prompts)" "$sent"
  expect "open cards after the restart" "$(q "SELECT count(*) FROM approvals WHERE status = 'open' AND heading LIKE 'ADE55 %'")" 1
  expect "steers" "$(jq -s '[.[] | select(.steer == true)] | length' "$PUPPET/log.jsonl")" 0
  "$INSTANCE" stop
  expect "processes other than the app and the puppet" "$(cat "$EVAL_OUT/sandbox-stray.txt" 2>/dev/null | wc -l | tr -d ' ')" 0
  echo "  info the sandbox refused: $("$INSTANCE" denied "$since")"
  return "$FAILED"
}
