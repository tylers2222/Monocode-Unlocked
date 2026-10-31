# ADE-89 scenarios for scripts/scenarios/app.sh, which sources this file and
# provides q, holds, wait_until, kill9, expect, fresh_pair, request_habit,
# prompts, saw and the instance paths.

# The notice rule goes first: the notice quotes the session's prompt. The
# Mono's agent holds its ADE89-BUSY turn, so a notice waits in its queue; the
# work session's second Continue ends once that turn has started.
# ade89_script <starts>: the Mono's ADE89-GO turn starts that many work
# sessions (1 or 2), so the window owes that many results. A `hold` counts
# every session's turns of its rule, so each work session holds one turn and
# one Continue.
NOTICE="MonoCode completion notification"
ade89_script() {
  jq -n --arg app "$(target_bin instance)" --arg notice "$NOTICE" --arg log "$PUPPET/log.jsonl" --argjson starts "$1" '{app: $app, rules: [
    {match: "ADE89-BUSY-HABIT", calls: [{tool: "send_message", arguments:
      {message: "ADE89-BUSY Wait for the work.", send_to: "ade89-mono", wanting_reply: false, intent: "new"}}], reply: "Asked."},
    {match: "ADE89-HABIT", calls: [{tool: "send_message", arguments:
      {message: "ADE89-GO Start the work session.", send_to: "ade89-mono", wanting_reply: false, intent: "new"}}], reply: "Asked."},
    {match: $notice, reply: "Reviewed."},
    {match: "Continue from where you left off.", hold: $starts, reply: "Done.", exec: [["sh", "-c",
      "for _ in $(seq 1 300); do grep -q \"\\\"rule\\\":\\\"ADE89-BUSY\\\"\" \"$0\" && exit 0; sleep 0.2; done; exit 1", $log]]},
    {match: "ADE89-BUSY", hold: 99, reply: "Done."},
    {match: "ADE89-WORK", hold: $starts, reply: "Done."},
    {match: "ADE89-GO", calls: ([{action: "sessions.start", arguments:
      {prompt: "ADE89-WORK Build it.", harness: "fx", model: "fx:zai/glm-5.2-fast"}},
      {action: "sessions.start", arguments:
      {prompt: "ADE89-WORK Test it.", harness: "fx", model: "fx:zai/glm-5.2-fast"}}] | .[:$starts]), reply: "Started."}]}' >"$PUPPET/script.json"
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

# started <n>: the puppet logged n sessions.start calls.
started() { (($(jq -s '[.[] | select(.action == "sessions.start")] | length' "$PUPPET/log.jsonl") == $1)); }

# each_once <mono chat> <when> <work session...>: each session's result is a
# row or a report to the Mono, never both and never twice.
each_once() {
  local mono_chat="$1" when="$2" w
  shift 2
  for w in "$@"; do
    expect "rows plus reports of $w $when" \
      $(($(q "SELECT count(*) FROM mono_completions WHERE request_id = '$w'") + $(reports "$mono_chat" "$w"))) 1
  done
}

# ADE-89: a waiting completion result is a mono_completions row from the
# moment the Mono starts the session. It survives two exits with no quit save,
# its notice is in the Mono's saved queue before the delivered mark, and a
# kill right after the mark still leaves the Mono exactly one notice. Mode
# `two` runs the build-two instance: main and window-0 boot together at every
# launch and both claim, and the Mono owes two results, each reported once.
run_ade89() {
  local mode="$1" run="$2" owed=1 mono_chat work works early _
  FAILED=0
  log "ADE-89 run $run, exit by $mode"
  [[ "$mode" != two || "$(cat "$EVAL_OUT/instance.app")" == *scenario-two* ]] || die "mode two needs app.sh build-two"
  [[ "$mode" == two || "$(cat "$EVAL_OUT/instance.app")" != *scenario-two* ]] || die "mode $mode needs app.sh build"
  [[ "$mode" == two ]] && owed=2
  fresh_pair ade89
  ade89_script "$owed"
  request_habit ade89-giga "Start the work" "ADE89-HABIT: ask ADE89 Mono to start the work."
  mono_chat="$(chat_of ade89-mono)"
  mark_after_the_notice "$mono_chat"
  "$INSTANCE" start
  # The Mono's sessions are the ids its sessions.start calls answered. The
  # Giga's habit run is in flight too, alone before the Mono's turn starts,
  # so in_flight_sessions cannot name them (ADE-124).
  wait_until 90 "the Mono starts its sessions" started "$owed"
  work="$(jq -rs '[.[] | select(.action == "sessions.start") | (.result.result // .result).id] | sort | .[]' "$PUPPET/log.jsonl")"
  works="$(printf "'%s'," $work)"; works="${works%,}"
  wait_until 30 "the Mono's sessions run, alone in flight" \
    holds "SELECT count(*) = $owed AND sum(session_id IN ($works)) = $owed FROM in_flight_sessions"
  wait_until 30 "the Mono's turn ends" holds "SELECT count(*) = 0 FROM in_flight_sessions WHERE session_id = '$mono_chat'"
  wait_until 30 "each waiting result is a row" holds "SELECT count(*) = $owed FROM mono_completions WHERE request_id IN ($works)"
  echo "ade89 ($mode) run $run readings:"
  expect "sessions.start calls that failed" "$(jq -s '[.[] | select(.action and .result.ok != true)] | length' "$PUPPET/log.jsonl")" 0
  # #284 r2: a home from an earlier head keeps `claimed_at`, and every hold
  # fails there. fresh_pair wipes GIGA_EVAL_HOME; this proves it did.
  expect "store columns from an earlier head" "$(q "SELECT count(*) FROM pragma_table_info('mono_completions') WHERE name = 'claimed_at'")" 0
  expect "workspace rows that carry results" "$(q "SELECT (SELECT count(*) FROM workspace_snapshot WHERE json_type(snapshot_json, '$.monoCompletions') IS NOT NULL)
    + (SELECT count(*) FROM workspace_window_snapshots WHERE json_type(snapshot_json, '$.monoCompletions') IS NOT NULL)")" 0
  kill9

  "$INSTANCE" start
  wait_until 60 "the relaunch continues the session" saw "Continue from where you left off."
  if [[ "$mode" == two ]]; then
    # Both windows restore the sessions, and the second one's harness bind can
    # end the first one's Continue turn (ADE-93), so a result may be out.
    sleep 5
    expect "windows that hold a row, at most one" "$(q "SELECT count(DISTINCT window_label) <= 1 FROM mono_completions")" 1
    each_once "$mono_chat" "after the first relaunch" $work
  else
    expect "rows after the first relaunch" "$(rows)" 1
    expect "windows that hold them" "$(q "SELECT count(DISTINCT window_label) FROM mono_completions")" 1
    expect "notices before the second exit" "$(prompts "$NOTICE")" 0
  fi
  exit_unsaved "$mode"

  early="$(q "SELECT count(*) FROM mono_completion_deliveries")"
  request_habit ade89-giga "Keep the Mono busy" "ADE89-BUSY-HABIT: ask ADE89 Mono to wait for the work."
  "$INSTANCE" start
  if [[ "$mode" == two ]]; then
    wait_until 60 "every result is marked delivered" \
      holds "SELECT (SELECT count(*) FROM mono_completions) = 0 AND (SELECT count(*) FROM mono_completion_deliveries) > 0"
    each_once "$mono_chat" "at the mark" $work
    expect "windows that hold a row, at most one" "$(q "SELECT count(DISTINCT window_label) <= 1 FROM mono_completions")" 1
  else
    wait_until 60 "the notice is marked delivered" holds "SELECT count(*) = 1 FROM mono_completion_deliveries"
  fi
  if [[ "$early" == 0 && "$mode" != two ]]; then
    expect "rows once it is marked" "$(rows)" 0
    expect "notices in the Mono's saved queue at the mark" "$(queued_notices "$mono_chat")" 1
    expect "notices the Mono's agent got before the kill" "$(prompts "$NOTICE")" 0
  fi
  # Review r1's probe: the app dies right after the delivered mark.
  kill9

  "$INSTANCE" start
  for _ in $(seq 1 60); do (($(notices "$mono_chat") > 0)) && break; sleep 1; done
  # A copy the boot sends again would follow within the boot's first seconds.
  sleep 10
  if [[ "$mode" == two ]]; then
    each_once "$mono_chat" "after a kill after the mark" $work
  else
    expect "notices to the Mono after a kill after the mark" "$(notices "$mono_chat")" 1
    expect "delivered groups" "$(q "SELECT count(*) FROM mono_completion_deliveries")" 1
  fi
  expect "rows once it is delivered" "$(rows)" 0
  echo "  Continue prompts: $(prompts "Continue from where you left off.")"
  if [[ "$mode" == kill ]]; then
    crash_before_the_mark "$mono_chat" "$work"
    part_release_then_kill "$mono_chat" a b c
    part_release_then_kill "$mono_chat" e f d
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
# the two not again, whichever request id sorts first. `reports` counts the
# whole puppet log, so each call names its own sessions.
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
