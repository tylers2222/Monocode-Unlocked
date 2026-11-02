# shellcheck shell=bash
# ADE-167 through the real app: the shim's bin/claude is the app's Claude
# binary override, and the Giga's habit turn starts it with a login folder
# that has no fixture, so it exits before init. The turn ends with Claude
# Code's exit as its error, before the 8 s init timeout.
# Sourced by `app.sh usage-limit`, which holds the shared helpers.

UL_HABITS="$APP_DATA/monos/ul-giga/habits.json"

# The app's CLI path setting, in its localStorage, read at the next start.
# WebKit stores each value as UTF-16LE.
claude_override() {
  local db value
  db="$(find "$EVAL_HOME/Library/WebKit" -name localstorage.sqlite3 | head -1)"
  [[ -n "$db" ]] || die "no localStorage database under the temp HOME"
  value="$(jq -cn --arg path "$1" '{claude: $path}' | iconv -f UTF-8 -t UTF-16LE | xxd -p | tr -d '\n')"
  sqlite3 "$db" "INSERT OR REPLACE INTO ItemTable (key, value) VALUES ('monocode.providerBinaryPaths.v1', X'$value')"
}

ul_failed() { jq -e '.[0].lastOutcome == "failed"' "$UL_HABITS" >/dev/null 2>&1; }

run_usage_limit() {
  FAILED=0
  log "usage-limit run"
  # The app's Claude Code children inherit both.
  export USAGE_LIMIT_SHIM_DIR="$EVAL_HOME/.usage-limit"
  export CLAUDE_CONFIG_DIR="$USAGE_LIMIT_SHIM_DIR/logins/no-fixture"
  fresh_pair ul
  # The e2e job uploads $GIGA_EVAL_OUT/*.log: every start of the shim.
  trap 'cp "$USAGE_LIMIT_SHIM_DIR/log.jsonl" "$EVAL_OUT/usage-limit-shim.log"' EXIT
  mkdir -p "$CLAUDE_CONFIG_DIR"
  # Another login has the recording, so `--version` answers and the start does not.
  jq -n --arg recorded "$USAGE_LIMIT_SHIM_DIR/logins/recorded" \
    '{speed: 1, accounts: {($recorded): "claude-2.1.312-2026-11-02-ok.jsonl"}}' >"$USAGE_LIMIT_SHIM_DIR/script.json"
  : >"$USAGE_LIMIT_SHIM_DIR/log.jsonl"
  sqlite3 "$DB" "UPDATE sessions SET harness = 'claude', model = 'claude:haiku-4.5'
    WHERE id = (SELECT json_extract(data_json, '\$.sessionId') FROM monos WHERE id = 'ul-giga')"
  claude_override "$SCENARIOS/usage-limit/bin/claude"
  request_habit ul-giga "Say ok" "UL-HABIT: say ok."
  "$INSTANCE" start
  wait_until 60 "the habit run fails" ul_failed
  local starts gap
  # The shim's session starts, not the `--version` checks: login folder, fixture, time.
  starts="$(jq -c 'select(.args | index("stream-json")) | [.folder, .fixture, .at]' "$USAGE_LIMIT_SHIM_DIR/log.jsonl")"
  # The app spends seconds on the run before it starts the CLI, so the bound
  # counts from the CLI's own start.
  gap=$(($(jq '.[0].runs[0].endedAt' "$UL_HABITS") - $(jq -s '.[0][2]' <<<"$starts")))
  echo "usage-limit readings:"
  expect "the shim's session starts: login folder, fixture" "$(jq -rs 'map("\(.[0]) \(.[1])") | join(", ")' <<<"$starts")" \
    "$CLAUDE_CONFIG_DIR null"
  expect "the run's error" "$(jq -r '.[0].lastError' "$UL_HABITS")" "Claude Code exited"
  expect "the run ends within the 8 s init timeout of the CLI's start ($gap ms)" "$((gap < 8000))" 1
  "$INSTANCE" stop
  return "$FAILED"
}
