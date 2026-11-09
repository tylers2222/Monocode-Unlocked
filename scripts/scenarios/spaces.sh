# shellcheck shell=bash
# ADE-182 (MA-13): Spaces end to end through the real app. Each part seeds
# Spaces, Monos and logins in a fresh temp HOME, lets habits and messages run
# turns with the fx puppet or the ADE-167 shim as the Claude CLI, and reads
# what the app did: which login and folder each child started with, which
# rows it wrote, what each tool call answered. No model runs.
# Sourced by `app.sh spaces [part]`, which holds the shared helpers.
#
#   login      ADE-172: a session starts with its Space's login (Space 1: the
#              default, Space 2: its own), a Space with no Claude login is
#              refused, and new rows record their Space
#   checkout   ADE-173: the same project in two Spaces runs in two folders,
#              Space 2 in its own worktree on a space-2/ branch, each leased,
#              each side commits and pushes. ADE-189 1-2: a helper a Space 2
#              session owns (the Codex store helper) runs in that session's
#              copy on Space 2's login
#   messages   ADE-174: a Mono messages a Mono in another Space; a Giga is
#              refused both ways. ADE-175: the app CLI lists only the
#              caller's Space
#   grants     ADE-176: a valid grant keeps a session on the granted login;
#              once it has ended, the next turn is refused and the grant is
#              removed
#
# Pending, one part each as its PR merges (no spec runs against unmerged code):
#   TODO(ADE-171) logins kept in SQLite: the Space login part reads them
#   TODO(ADE-177) a write into the other Space's checkout is refused
#   TODO(ADE-178) the importer: MonoCode 2's data lands as Space 2
#   TODO(ADE-179) switch Spaces while both run
#   TODO(ADE-180) Settings > Spaces
#   TODO(ADE-181) limit hit, prompt, grant, reroute and TTL expiry error,
#                 the ADE-167 shim as the CLI and a 1-minute TTL
#   TODO(ADE-189 4/4) the last helpers that keep their caller's folder

SP_BIN="$SCENARIOS/usage-limit/bin"
SP_OK="claude-2.1.312-2026-11-02-ok.jsonl"
SP_CODEX_OK="codex-0.165.0-2026-11-02-ok.jsonl"
SP_LOGINS="$APP_DATA/provider-accounts"

# sp_local <key> <json>: one value of the app's localStorage, read at the next
# start (claude_override in usage-limit/app.test.sh writes it the same way).
sp_local() {
  local db value
  db="$(find "$EVAL_HOME/Library/WebKit" -name localstorage.sqlite3 | head -1)"
  [[ -n "$db" ]] || die "no localStorage database under the temp HOME"
  value="$(printf '%s' "$2" | iconv -f UTF-8 -t UTF-16LE | xxd -p | tr -d '\n')"
  sqlite3 "$db" "INSERT OR REPLACE INTO ItemTable (key, value) VALUES ('$1', X'$value')"
}

# sp_clis: the claude and codex CLIs for the app's path override. Each logs
# its CLI, process id, folder, login folder and arguments, then runs the
# ADE-167 shim, which replays the fixture of its login. Codex also copies
# what the app sends it to .spaces/in-<pid>.jsonl, which names the helper a
# start is: the Codex store helper reads config/read (codexStore.ts), the
# catalog probe model/list (codexCatalog.ts).
sp_clis() {
  local cli bin env run
  mkdir -p "$EVAL_HOME/.spaces/bin"
  for cli in claude codex; do
    bin="$EVAL_HOME/.spaces/bin/$cli"
    # shellcheck disable=SC2016 # $$ and $@ are the wrapper's, at its run
    env=CLAUDE_CONFIG_DIR run='exec "'"$SP_BIN/$cli"'" "$@"'
    # shellcheck disable=SC2016
    [[ $cli == codex ]] && env=CODEX_HOME run='tee "'"$EVAL_HOME"'/.spaces/in-$$.jsonl" | "'"$SP_BIN/$cli"'" "$@"'
    cat >"$bin" <<SH
#!/bin/sh
printf '%s\n' "\$@" | jq -R . | jq -sc --arg cli $cli --argjson pid \$\$ --arg cwd "\$(pwd -P)" --arg login "\${$env:-default}" \\
  '{cli: \$cli, pid: \$pid, cwd: \$cwd, login: \$login, args: .}' >>"$EVAL_HOME/.spaces/cli.jsonl"
$run
SH
    chmod +x "$bin"
  done
  : >"$EVAL_HOME/.spaces/cli.jsonl"
  sp_local monocode.providerBinaryPaths.v1 \
    "{\"claude\": \"$EVAL_HOME/.spaces/bin/claude\", \"codex\": \"$EVAL_HOME/.spaces/bin/codex\"}"
}

# sp_accounts <folder>=<fixture>...: the shim's script, each login folder
# under both spellings of /tmp on macOS ("default" is no login folder). The
# shim keys by folder alone, so "default" serves both CLIs: a Codex
# `--version` there must not read the Claude recording, and a CLI with no
# fixture for its folder answers `--version` from any recording of its own.
sp_accounts() {
  local pair accounts="{}"
  for pair in "$@"; do
    accounts="$(jq -c --arg f "${pair%%=*}" --arg x "${pair#*=}" \
      'if $f == "default" then . + {default: $x} else . + {($f): $x, ("/private" + $f): $x} end' <<<"$accounts")"
  done
  jq -n --argjson accounts "$accounts" '{speed: 20, accounts: $accounts}' >"$USAGE_LIMIT_SHIM_DIR/script.json"
  : >"$USAGE_LIMIT_SHIM_DIR/log.jsonl"
}

# sp_starts [cli]: CLI starts, not `--version` checks: "<login> <cwd>" each.
sp_starts() {
  jq -r --arg cli "${1:-claude}" 'select(.cli == $cli and .args[0] != "--version") | "\(.login) \(.cwd)"' \
    "$EVAL_HOME/.spaces/cli.jsonl"
}
sp_count() { sp_starts | grep -c "$1" || true; }
# The Codex store helper's starts, "<login> <cwd>" once each: the Codex
# starts whose input holds its config/read request.
sp_store_starts() {
  local pids
  pids="$(grep -l '"method":"config/read"' "$EVAL_HOME"/.spaces/in-*.jsonl 2>/dev/null | sed 's/.*in-\([0-9]*\)\.jsonl/\1/' | paste -sd, -)"
  jq -r --argjson pids "[${pids}]" 'select(.cli == "codex" and (.pid | IN($pids[]))) | "\(.login) \(.cwd)"' \
    "$EVAL_HOME/.spaces/cli.jsonl" | sort -u
}
sp_started() { grep -q "$1" "$EVAL_HOME/.spaces/cli.jsonl"; }
# sp_ran <cli>: the CLI has started at least once, `--version` checks aside.
sp_ran() { [[ -n "$(sp_starts "$1")" ]]; }

# sp_fresh <prefix>: a fresh temp HOME with the puppet, launched once so the
# DB is migrated, then stopped. Every SQL insert below goes in while it is.
sp_fresh() {
  fresh_home
  install_puppet_bin
  unset CLAUDE_CONFIG_DIR CODEX_HOME
  export USAGE_LIMIT_SHIM_DIR="$EVAL_HOME/.usage-limit"
  mkdir -p "$USAGE_LIMIT_SHIM_DIR"
  "$INSTANCE" start
  wait_until 30 "the first launch writes mono_clock" holds "SELECT count(*) = 1 FROM mono_clock"
  "$INSTANCE" stop
  SP_NOW="$(date +%s)000"
  # The e2e job uploads $GIGA_EVAL_OUT/*.log; a failed part also prints its
  # puppet's prompts and calls, so the job log names the step that stalled.
  # shellcheck disable=SC2064 # the part's name now, the exit status then
  trap "sp_logs $1 \$?" EXIT
}

sp_logs() {
  cp "$PUPPET/log.jsonl" "$EVAL_OUT/spaces-$1-puppet.log" 2>/dev/null || true
  cp "$EVAL_HOME/.spaces/cli.jsonl" "$EVAL_OUT/spaces-$1-cli.log" 2>/dev/null || true
  (($2 == 0)) && return
  echo "spaces $1: the puppet's prompts and calls:"
  jq -c '{rule, steer, tool, action, exec, message: (.input.message // .input.prompt), ok: .result.ok, code: .result.code,
    error: (.result.error // .result.hint)} | with_entries(select(.value != null))' "$PUPPET/log.jsonl" 2>/dev/null | sed 's/^/  /'
  sed 's/^/  cli: /' "$EVAL_HOME/.spaces/cli.jsonl" 2>/dev/null || true
}

sp_space() { sqlite3 "$DB" "INSERT INTO spaces (id, name, created_at) VALUES ('$1', '$2', $SP_NOW)"; }

# sp_login <space> <account> [cli]: a login (Claude unless named), Space <space>'s.
sp_login() {
  local cli="${3:-claude}"
  sqlite3 "$DB" "INSERT OR IGNORE INTO provider_accounts (provider, id, label, created_at) VALUES ('$cli', '$2', '$2', $SP_NOW);
    INSERT INTO space_accounts (space_id, provider, account_id) VALUES ('$1', '$cli', '$2')"
}

# sp_mono <id> <role> <space> <project> <harness> <model> [account]: a Mono in
# Space <space> with its chat, both rows in that Space.
sp_mono() {
  local chat name account="${7:-}"
  chat="$(uuidgen | tr '[:upper:]' '[:lower:]')"; name="$(tr '[:lower:]' '[:upper:]' <<<"$1")"
  sqlite3 "$DB" <<SQL
INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at, space_id)
  SELECT '$1', '$name', '$2', CASE '$2' WHEN 'giga' THEN json_set(data, '\$.gigaSlug', '$1') ELSE data END,
    (SELECT count(*) FROM monos), $SP_NOW, $SP_NOW, '$3'
  FROM (SELECT json_object('name', '$name', 'sessionId', '$chat', 'projects', json_array('$4')) AS data);
INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at, space_id, provider_account_id)
  VALUES ('$chat', '$4', '$5', '$6', 'auto', '$name', $SP_NOW, $SP_NOW, '$3', NULLIF('$account', ''));
SQL
}

chat_of() { q "SELECT json_extract(data_json, '\$.sessionId') FROM monos WHERE id = '$1'"; }
habit_of() { jq -r ".[0].$2 // \"\"" "$APP_DATA/monos/$1/habits.json"; }
habit_done() { [[ -n "$(habit_of "$1" lastOutcome)" ]]; }
# A git project of its own, with one commit.
sp_project() {
  mkdir -p "$1"
  git -C "$1" init -q -b main
  echo "# $(basename "$1")" >"$1/README.md"
  git -C "$1" add README.md
  git -C "$1" -c user.name=spaces -c user.email=spaces@local commit -qm initial
}
# The puppet's exec log line whose argv holds <text>: "<code> <stdout>".
ran() { probe "$1" '"\(.code) \(.stdout | rtrimstr("\n") | gsub("\n"; " "))"'; }
called() {
  jq -rs --arg m "$1" "first(.[] | select((.input.message // .input.prompt // \"\") | startswith(\$m)) | .result) | $2" \
    "$PUPPET/log.jsonl"
}
# listed <rule> <tool or action> <jq>: the answer to that call in the turn of
# the puppet that got the prompt matching <rule> (one puppet per child).
listed() {
  jq -rs --arg r "$1" --arg a "$2" "(first(.[] | select(.rule == \$r)) | .pid) as \$pid
    | first(.[] | select(.pid == \$pid and (.action == \$a or .tool == \$a)) | .result) | $3" "$PUPPET/log.jsonl"
}
# logged <rule> <tool or action>: `listed` can read that call. The puppet logs
# a turn's calls in order, each after its answer, so a turn's last call means
# its earlier ones are in the log too.
logged() { [[ -n "$(listed "$1" "$2" true)" ]]; }
# wait_for <seconds> <command...>: wait_until that returns 1 instead of
# stopping the run, so the readings after it still print.
wait_for() {
  local secs="$1" _
  shift
  for _ in $(seq 1 "$secs"); do "$@" && return 0; sleep 1; done
  return 1
}
# Prompts that carry <text> anywhere, whatever rule they ran.
heard() { jq -s --arg t "$1" '[.[] | select(.prompt // "" | contains($t))] | length' "$PUPPET/log.jsonl"; }

# ADE-172 and ADE-189 1-2. Space 1 has no login of its own (the default),
# Space 2 has acct-2 and Space 3 none. One Mono per Space, each with a Claude
# chat on its own project; each habit run is a Claude child of that Mono.
run_spaces_login() {
  log "spaces login"
  sp_fresh login
  local p1="$EVAL_HOME/lg-one" p2="$EVAL_HOME/lg-two" p3="$EVAL_HOME/lg-three"
  sp_project "$p1"; sp_project "$p2"; sp_project "$p3"
  sp_space space-2 "Space 2"; sp_space space-3 "Space 3"
  sp_login space-2 acct-2
  sqlite3 "$DB" "INSERT INTO space_projects (project_path, space_id) VALUES ('$p2', 'space-2'), ('$p3', 'space-3')"
  sp_mono lg1-mono mono space-1 "$p1" claude claude:haiku-4.5
  sp_mono lg2-mono mono space-2 "$p2" claude claude:haiku-4.5
  sp_mono lg3-mono mono space-3 "$p3" claude claude:haiku-4.5
  sp_clis
  sp_accounts "default=$SP_OK" "$SP_LOGINS/claude/acct-2=$SP_OK"
  for mono in lg1-mono lg2-mono lg3-mono; do request_habit "$mono" "Say ok" "LG-HABIT: say ok."; done
  "$INSTANCE" start
  wait_until 90 "Space 1's habit starts Claude" sp_started '"login":"default"'
  wait_until 90 "Space 2's habit starts Claude" sp_started claude/acct-2
  wait_until 90 "Space 3's habit run ends" habit_done lg3-mono
  local c1 c2 c3 real2 real1
  c1="$(chat_of lg1-mono)"; c2="$(chat_of lg2-mono)"; c3="$(chat_of lg3-mono)"
  real1="$(cd "$p1" && pwd -P)"; real2="$(cd "$p2" && pwd -P)"
  echo "spaces login readings:"
  sp_starts | sed 's/^/  claude start: /'
  expect "Space 1's habit run: Claude starts in its project on the default login" "$(($(sp_count "^default $real1\$") >= 1))" 1
  expect "Space 2's habit run: Claude starts in its project on Space 2's login" \
    "$(($(sp_count "/provider-accounts/claude/acct-2 $real2\$") >= 1))" 1
  # Any other Claude child here (none is required: titles of habit runs are
  # set) is Space 2's too.
  expect "Claude children in Space 2's project on any other login" \
    "$(sp_starts | grep " $real2" | grep -vc '/provider-accounts/claude/acct-2 ' || true)" 0
  expect "Claude children in Space 3's project (no Claude login)" "$(sp_count "$(cd "$p3" && pwd -P)")" 0
  expect "Space 3's habit run: outcome, error" \
    "$(habit_of lg3-mono lastOutcome) $(habit_of lg3-mono lastError | grep -c 'Space 3 has no Claude login')" "failed 1"
  expect "new rows record their Space: Space 1's habit runs" \
    "$(q "SELECT group_concat(DISTINCT space_id) FROM sessions WHERE id LIKE 'app-$c1-habit-%'")" space-1
  expect "new rows record their Space: Space 2's habit runs" \
    "$(q "SELECT group_concat(DISTINCT space_id) FROM sessions WHERE id LIKE 'app-$c2-habit-%'")" space-2
  q "SELECT '  session ' || id || ' ' || space_id || ' ' || ifnull(provider_account_id, '-') FROM sessions
       WHERE id LIKE 'app-$c1-%' OR id LIKE 'app-$c2-%' OR id LIKE 'app-$c3-%'"
  "$INSTANCE" stop
}

# ADE-173 and ADE-189 1-2. One project, a Mono on it in Space 1 and one in
# Space 2. Each habit run writes a file, prints its folder and branch, and
# commits and pushes. Space 2's run also messages a Space 2 Mono whose chat
# is Codex: before its turn the app starts the Codex store helper
# (codexStore.ts), a helper the chat owns, so the spawn hook places it as
# its owner: in Space 2's copy of the project, on Space 2's Codex login.
run_spaces_checkout() {
  log "spaces checkout"
  sp_fresh checkout
  local p="$EVAL_HOME/co-project" remote="$EVAL_HOME/co-remote.git" real
  sp_project "$p"
  real="$(cd "$p" && pwd -P)"
  git init -q --bare "$remote"
  git -C "$p" remote add origin "$remote"
  git -C "$p" config user.name spaces; git -C "$p" config user.email spaces@local
  sp_space space-2 "Space 2"
  sp_login space-2 acct-2 codex
  sp_mono co1-mono mono space-1 "$p" fx fx:zai/glm-5.2-fast
  sp_mono co2-mono mono space-2 "$p" fx fx:zai/glm-5.2-fast
  sp_mono co2-codex mono space-2 "$p" codex codex:gpt-5.4
  sp_clis
  sp_accounts "$SP_LOGINS/codex/acct-2=$SP_CODEX_OK"
  jq -n --arg app "$(target_bin instance)" '{app: $app, rules: [
    {match: "CO1-HABIT", exec: [["sh", "-c", "echo one > co-one.txt; pwd -P; git branch --show-current"],
      ["sh", "-c", "git add co-one.txt && git commit -qm one && git push -q origin HEAD 2>&1 && echo pushed-one"]], reply: "Done."},
    {match: "CO2-HABIT", exec: [["sh", "-c", "echo two > co-two.txt; pwd -P; git branch --show-current"],
      ["sh", "-c", "git add co-two.txt && git commit -qm two && git push -q origin HEAD 2>&1 && echo pushed-two"]],
     calls: [{tool: "send_message", arguments: {message: "CO-CODEX Say ok.", send_to: "co2-codex", wanting_reply: false, intent: "new"}}],
     reply: "Done."}]}' \
    >"$PUPPET/script.json"
  request_habit co1-mono "Write one" "CO1-HABIT: write one."
  request_habit co2-mono "Write two" "CO2-HABIT: write two."
  "$INSTANCE" start
  wait_until 90 "both habit runs ran their probes" holds_log '[.[] | select(.exec)] | length == 4'
  wait_until 30 "Space 1's run ends" habit_done co1-mono
  wait_until 30 "Space 2's run ends" habit_done co2-mono
  wait_for 90 sp_ran codex \
    || q "SELECT '  (no Codex start) message ' || status || ', chat ' || substr(ifnull(blocks_json, '-'), 1, 300)
          FROM mono_messages, sessions WHERE to_mono_id = 'co2-codex' AND sessions.id = '$(chat_of co2-codex)'"
  local c2 run2 copy
  c2="$(chat_of co2-mono)"
  run2="$(q "SELECT id FROM sessions WHERE id LIKE 'app-$c2-habit-%' LIMIT 1")"
  copy="$real-worktrees/space-2/$run2"
  echo "spaces checkout readings (Space 2 run $run2):"
  expect "Space 1's run: folder, branch" "$(ran 'echo one >')" "0 $real main"
  expect "Space 2's run: its own copy, on a space-2/ branch" "$(ran 'echo two >')" "0 $copy space-2/$run2"
  # Space 2's copy starts from the project's HEAD, which may already hold
  # Space 1's commit: only Space 2's own file is checked on both sides.
  expect "Space 2's file is in its copy only" \
    "$([[ -e "$p/co-two.txt" ]] && echo main) $([[ -f "$copy/co-two.txt" ]] && echo copy)" " copy"
  expect "leases: the main checkout, the copy" \
    "$(q "SELECT group_concat(space_id, ' ') FROM (SELECT space_id FROM checkout_leases WHERE checkout_path IN ('$p', '$real') UNION ALL
          SELECT space_id FROM checkout_leases WHERE checkout_path LIKE '%-worktrees/space-2/$run2')")" "space-1 space-2"
  expect "pushes from each: Space 1, Space 2" "$(ran pushed-one) $(ran pushed-two)" "0 pushed-one 0 pushed-two"
  expect "the remote: main has Space 1's file and not Space 2's; space-2/ has Space 2's" \
    "$(git -C "$remote" ls-tree --name-only main | paste -sd, -) $(git -C "$remote" ls-tree --name-only "space-2/$run2" | grep -c co-two.txt)" \
    "README.md,co-one.txt 1"
  expect "Space 2's run records its copy" \
    "$(q "SELECT worktree_cwd LIKE '%co-project-worktrees/space-2/$run2' FROM sessions WHERE id = '$run2'")" 1
  # ADE-189 1-2: each start of the store helper (the Codex start that sent
  # config/read), wherever the hook put it.
  local codex_chat
  codex_chat="$(chat_of co2-codex)"
  sp_starts codex | sed 's/^/  codex start: /'
  expect "the helper Space 2's Codex chat owns: its login, its folder" "$(sp_store_starts)" \
    "$SP_LOGINS/codex/acct-2 $real-worktrees/space-2/$codex_chat"
  q "SELECT '  lease ' || checkout_path || ' ' || space_id FROM checkout_leases ORDER BY checkout_path"
  "$INSTANCE" stop
}

# ADE-174 and ADE-175. Space 1: a Mono. Space 2: a Mono and a Giga. Each
# refused call is one the gate allows inside a Space, so only the Space rule
# refuses it: Space 1's Mono steers Space 2's Mono (a Mono may steer the turn
# its message started) and starts a session in its project that messages Space
# 2's Mono, which lists that project too; Space 2's Mono messages a saved
# Space 1 session in a project it lists; Space 2's Giga messages and asks
# Space 1's Mono (a Giga asks any Mono). Mono to Giga and session to Mono are
# refused before the Space rule (each needs the recipient's open ask, which
# cannot cross), so they are checked only as refused, with nothing written.
# Space 2's Mono answers across and lists what it sees; Space 2's Giga lists
# too.
run_spaces_messages() {
  log "spaces messages"
  sp_fresh messages
  local p1="$EVAL_HOME/ms-one" p2="$EVAL_HOME/ms-two"
  sp_project "$p1"; sp_project "$p2"
  sp_space space-2 "Space 2"
  # The Giga's habit runs in its own workspace folder, which is Space 2's.
  sqlite3 "$DB" "INSERT INTO space_projects (project_path, space_id) VALUES ('$p2', 'space-2'),
    ('$EVAL_HOME/MonoCode', 'space-2'), ('$EVAL_HOME/MonoCode/gigas/ms2-giga', 'space-2')"
  sp_mono ms1-mono mono space-1 "$p1" fx fx:zai/glm-5.2-fast
  sp_mono ms2-mono mono space-2 "$p2" fx fx:zai/glm-5.2-fast
  sp_mono ms2-giga giga space-2 "$p2" fx fx:zai/glm-5.2-fast
  # A session messages a Mono that has its project (direction_forbidden
  # otherwise): Space 2's Mono lists Space 1's project after its own.
  sqlite3 "$DB" "UPDATE monos SET data_json = json_insert(data_json, '\$.projects[#]', '$p1') WHERE id = 'ms2-mono'"
  # A saved Space 1 session in that project, which any Mono of it may message
  # inside a Space.
  sqlite3 "$DB" "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at, space_id)
    VALUES ('ms1-lead', '$p1', 'fx', 'fx:zai/glm-5.2-fast', 'auto', 'MS1 lead', $SP_NOW, $SP_NOW, 'space-1')"
  # Two Monos message each other when they share a project or one allowlists
  # the other (design 6.1); a project is in one Space, so the allowlist.
  sqlite3 "$DB" "UPDATE monos SET data_json = json_set(data_json, '\$.messageAllowlist', json_array('ms2-mono')) WHERE id = 'ms1-mono'"
  # Messages first: a prompt carrying a message may quote a habit's text.
  jq -n --arg app "$(target_bin instance)" '{app: $app, rules: [
    {match: "MS-HELLO", calls: [
      {tool: "send_message", arguments: {message: "MS-BACK Hello from Space 2.", send_to: "ms1-mono", wanting_reply: false, intent: "new"}},
      {tool: "send_message", arguments: {message: "MS-TO-SESSION Hello, session.", send_to: "ms1-lead", wanting_reply: false, intent: "new"}},
      {tool: "get_monos", arguments: {}},
      {action: "monos.list", arguments: {}},
      {action: "sessions.list", arguments: {}}], reply: "Answered."},
    {match: "MS-BACK", reply: "Noted."},
    {match: "MS-KID", calls: [
      {tool: "send_message", arguments: {message: "MS-FROM-KID Hello from a session.", send_to: "ms2-mono", wanting_reply: false, intent: "new"}}],
      reply: "Sent."},
    {match: "MS1-HABIT", calls: [
      {tool: "send_message", arguments: {message: "MS-HELLO Hello from Space 1.", send_to: "ms2-mono", wanting_reply: false, intent: "new"}},
      {tool: "send_message", arguments: {message: "MS-TO-GIGA Hello, Giga.", send_to: "ms2-giga", wanting_reply: false, intent: "new"}},
      {tool: "send_message", arguments: {message: "MS-STEER Stop.", send_to: "ms2-mono", wanting_reply: false, intent: "steer", scope: "turn"}},
      {action: "sessions.start", arguments: {prompt: "MS-KID You are the session.", harness: "fx", model: "fx:zai/glm-5.2-fast", notifyOnComplete: false}}],
      reply: "Sent."},
    {match: "MS2-GIGA-HABIT", calls: [
      {tool: "send_message", arguments: {message: "MS-FROM-GIGA Hello, Space 1.", send_to: "ms1-mono", wanting_reply: false, intent: "new"}},
      {tool: "ask", arguments: {message: "MS-ASK Is CI green?", send_to: "ms1-mono"}},
      {tool: "get_monos", arguments: {}}],
      reply: "Sent."}]}' >"$PUPPET/script.json"
  request_habit ms1-mono "Say hello" "MS1-HABIT: say hello across."
  request_habit ms2-giga "Say hello" "MS2-GIGA-HABIT: say hello across."
  "$INSTANCE" start
  wait_until 90 "Space 1's Mono gets Space 2's answer" saw MS-BACK
  # The last call of each turn the readings below read: the Mono's and the Giga's
  # turns both call get_monos, so `logged` matches the turn and not only the tool.
  wait_until 60 "Space 2's Mono made its last call" logged MS-HELLO sessions.list
  wait_until 60 "Space 2's Giga made its last call" logged MS2-GIGA-HABIT get_monos
  wait_for 60 holds_log '[.[] | select(.input.message // "" | startswith("MS-FROM-KID"))] | length == 1' \
    || echo "  (Space 1's session never sent; its start: $(called MS-KID 'tostring'))"
  local c1
  c1="$(chat_of ms1-mono)"
  echo "spaces messages readings:"
  expect "Mono to a Mono in another Space: ok" "$(called MS-HELLO .ok)" true
  expect "the Mono in another Space gets it" "$(prompts MS-HELLO)" 1
  expect "its answer across: ok" "$(called MS-BACK .ok)" true
  expect "Mono to a Giga in another Space: refused" "$(called MS-TO-GIGA .ok)" false
  expect "Giga asks a Mono in another Space: code" "$(called MS-ASK .code)" cross_space_forbidden
  expect "Mono steers a Mono in another Space: code" "$(called MS-STEER .code)" cross_space_forbidden
  expect "Mono to a session in another Space: code" "$(called MS-TO-SESSION .code)" cross_space_forbidden
  expect "a session to a Mono in another Space: refused" "$(called MS-FROM-KID .ok)" false
  expect "Giga to a Mono in another Space: code" "$(called MS-FROM-GIGA .code)" cross_space_forbidden
  expect "the refused messages reach no one" \
    "$(heard MS-TO-GIGA) $(heard MS-ASK) $(heard MS-STEER) $(heard MS-FROM-KID) $(heard MS-FROM-GIGA)" "0 0 0 0 0"
  expect "no message to or from Space 2's Giga is written (rejected rows aside)" \
    "$(q "SELECT count(*) FROM mono_messages WHERE 'ms2-giga' IN (from_mono_id, to_mono_id) AND status <> 'rejected'")" 0
  expect "written to Space 2's Mono: Space 1's Mono's message only; to Space 1's session: none (rejected rows aside)" \
    "$(q "SELECT (SELECT count(*) FROM mono_messages WHERE to_mono_id = 'ms2-mono' AND status <> 'rejected') || ' '
          || (SELECT count(*) FROM mono_messages WHERE to_session_id = 'ms1-lead' AND status <> 'rejected')")" "1 0"
  expect "messages written across: the two allowed only" \
    "$(q "SELECT group_concat(from_mono_id || '>' || to_mono_id, ' ') FROM (SELECT * FROM mono_messages
          WHERE to_mono_id IS NOT NULL AND status <> 'rejected' ORDER BY created_at)")" "ms1-mono>ms2-mono ms2-mono>ms1-mono"
  # ADE-175: what the app CLI shows a Space 2 caller.
  expect "Space 2's Mono, get_monos: its Space, and Space 1's regular Mono marked" \
    "$(listed MS-HELLO get_monos '(.monos // .result.monos) | map("\(.mono_id):\(.other_space // "-")") | sort | join(" ")')" \
    "ms1-mono:Space 1 ms2-giga:- ms2-mono:-"
  expect "Space 2's Mono, monos.list: its own Space only" \
    "$(listed MS-HELLO monos.list '(.result.monos // .monos) | map(.id) | sort | join(" ")')" "ms2-giga ms2-mono"
  expect "Space 2's Mono, sessions.list: no session of Space 1" \
    "$(listed MS-HELLO sessions.list "[(.result.sessions // .sessions)[] | select(.id | startswith(\"app-$c1\") or . == \"$c1\")] | length")" 0
  expect "Space 2's Giga, get_monos: its own Space only" \
    "$(listed MS2-GIGA-HABIT get_monos '(.monos // .result.monos) | map(.mono_id) | sort | join(" ")')" "ms2-giga ms2-mono"
  q "SELECT '  message ' || from_mono_id || ' -> ' || ifnull(to_mono_id, to_session_id) || ' ' || status FROM mono_messages ORDER BY created_at"
  "$INSTANCE" stop
}

# ADE-176 (+ ADE-181 1/2's return). A Space 1 Mono's Claude chat runs on
# acct-2 under a grant from Space 1's login (the default). A Giga's habit
# messages it once per launch. Under the grant the turn starts on acct-2.
# Between launches the grant becomes a 1-minute one that ended a second ago
# (grants run on the wall clock, which the fake clock does not move); the
# next turn is refused with route_grant_expired, the grant is removed, and the
# chat goes back to its first login, stopped. The limit itself waits for a recorded
# out-of-usage fixture (ADE-167) and the card's Yes for ADE-181 2/2.
run_spaces_grants() {
  log "spaces grants"
  sp_fresh grants
  local p="$EVAL_HOME/gr-project" chat before
  sp_project "$p"
  sqlite3 "$DB" "INSERT INTO provider_accounts (provider, id, label, created_at) VALUES ('claude', 'acct-2', 'acct-2', $SP_NOW)"
  sp_mono gr-giga giga space-1 "$p" fx fx:zai/glm-5.2-fast
  sp_mono gr-mono mono space-1 "$p" claude claude:haiku-4.5 acct-2
  chat="$(chat_of gr-mono)"
  sqlite3 "$DB" "INSERT INTO account_route_grants (session_id, provider, from_account_id, to_account_id, granted_at, expires_at)
    VALUES ('$chat', 'claude', 'default', 'acct-2', $SP_NOW, $((SP_NOW + 3600000)))"
  sp_clis
  sp_accounts "$SP_LOGINS/claude/acct-2=$SP_OK"
  # The window refuses a turn on a login its own list lacks ("a removed
  # provider account"); until ADE-171 that list lives in localStorage.
  sp_local monocode.providerAccounts.v1 '{"claude": [{"id": "acct-2", "provider": "claude", "label": "Account 2"}]}'
  jq -n --arg app "$(target_bin instance)" '{app: $app, rules: [
    {match: "GR-HABIT", calls: [{tool: "send_message", arguments:
      {message: "GR-TURN Say ok.", send_to: "gr-mono", wanting_reply: false, intent: "new"}}], reply: "Sent."}]}' \
    >"$PUPPET/script.json"
  request_habit gr-giga "Message" "GR-HABIT: message the Mono."
  "$INSTANCE" start
  wait_until 90 "the granted turn starts Claude" sp_started claude/acct-2
  echo "spaces grants readings:"
  sp_starts | sed 's/^/  claude start: /'
  expect "under the grant: Claude starts on the granted login" "$(($(sp_count "/provider-accounts/claude/acct-2 ") >= 1))" 1
  expect "the grant stays while it is valid" "$(q "SELECT count(*) FROM account_route_grants WHERE session_id = '$chat'")" 1
  "$INSTANCE" stop
  before="$(sp_count "/provider-accounts/claude/acct-2 ")"
  local now; now="$(date +%s)000"
  sqlite3 "$DB" "UPDATE account_route_grants SET granted_at = $((now - 61000)), expires_at = $((now - 1000))
    WHERE session_id = '$chat'"
  expect "the grant: 1 minute, ended" \
    "$(q "SELECT (expires_at - granted_at) || ' ' || (expires_at < CAST(strftime('%s', 'now') AS INTEGER) * 1000) FROM account_route_grants")" "60000 1"
  : >"$PUPPET/log.jsonl"
  request_habit gr-giga "Message" "GR-HABIT: message the Mono."
  "$INSTANCE" start
  wait_until 90 "the Giga's second message is sent" holds_log '[.[] | select(.tool == "send_message" and .result.ok == true)] | length == 1'
  wait_until 60 "the ended grant is removed" holds "SELECT count(*) = 0 FROM account_route_grants WHERE session_id = '$chat'"
  # A Mono chat's transcript is in mono_blocks (mono_transcript.rs).
  wait_for 30 holds "SELECT count(*) > 0 FROM mono_blocks WHERE session_id = '$chat' AND block_json LIKE '%route_grant_expired%'" \
    || true
  "$INSTANCE" stop
  expect "after the minute: the turn is refused with route_grant_expired" \
    "$(q "SELECT count(*) > 0 FROM mono_blocks WHERE session_id = '$chat' AND block_json LIKE '%route_grant_expired%'")" 1
  expect "after the minute: no new Claude start on the granted login" "$(sp_count "/provider-accounts/claude/acct-2 ")" "$before"
  expect "the chat is back on its first login" "$(q "SELECT ifnull(provider_account_id, 'default') FROM sessions WHERE id = '$chat'")" default
  q "SELECT '  chat block ' || seq || ': ' || substr(block_json, 1, 200) FROM mono_blocks WHERE session_id = '$chat' ORDER BY seq DESC LIMIT 3"
}

SP_PARTS=(login checkout messages grants)

# run_spaces [part]: one part, or every part, each in its own process (a
# part called in `||` would run with errexit off) and its own temp HOME.
run_spaces() {
  local part="${1:-all}" failed=() p
  if [[ "$part" != all ]]; then
    [[ " ${SP_PARTS[*]} " == *" $part "* ]] || die "usage: app.sh spaces [all|${SP_PARTS[*]// /|}]"
    FAILED=0
    "run_spaces_$part"
    return "$FAILED"
  fi
  for p in "${SP_PARTS[@]}"; do
    "$0" spaces "$p" || failed+=("$p")
    "$INSTANCE" stop
  done
  log "spaces: $(( ${#SP_PARTS[@]} - ${#failed[@]} ))/${#SP_PARTS[@]} passed${failed[*]:+, failed: ${failed[*]}}"
  ((${#failed[@]} == 0))
}
