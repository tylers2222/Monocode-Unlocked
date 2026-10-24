# ADE-89 scenarios for scripts/scenarios/app.sh, which sources this file and
# provides q, holds, wait_until, kill9, expect, fresh_pair, request_habit,
# prompts, saw and the instance paths.

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
