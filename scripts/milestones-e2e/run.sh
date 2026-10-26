#!/usr/bin/env bash
# Backend milestones E2E: two Monos call the real app CLI against an isolated
# GigaEval instance whose monocode.db starts from fixtures/pre-m.sql.
#
#   npm run e2e:milestones [-- --build]
#   MILESTONES_E2E_APP=<.app on macOS, binary on Linux>   run that build instead of the last one
#
# Alpha owns project A, Beta owns project B. Each holds an app token through a
# requested habit run that sleeps (no screen input). One row per case goes to
# /tmp/milestones-e2e/results.tsv; exit 0 only when no case fails.
#
# On macOS the held turns run on omp with the real logins. On Linux (the E2E
# image: no model, no logins) they run on the fx puppet
# (scripts/scenarios/puppet), and MODEL_CASES, which test omp itself, are
# recorded as SKIP.
if [[ "${GIGA_EVAL_TARGET:-instance}" != instance ]]; then
  echo "milestones-e2e: refusing GIGA_EVAL_TARGET=$GIGA_EVAL_TARGET; only instance is allowed" >&2
  exit 2
fi
export GIGA_EVAL_TARGET=instance GIGA_EVAL_OUT=/tmp/milestones-e2e GIGA_EVAL_HOME=/tmp/milestones-e2e-home
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
G="$ROOT/scripts/giga-eval"
source "$G/lib.sh"

DATA="$EVAL_APP_DATA"
DB="$DATA/monocode.db"
A="$EVAL_HOME/projA"
B="$EVAL_HOME/projB"
RUN_LOG="$EVAL_OUT/run.log"
RUN_ID="$(date +%Y%m%d-%H%M%S)"

# Cases run in this order: migration, Beta's turn, ALPHA_CASES and PAGE_CASES
# while Alpha's turn is held, restart, then PAGE_RESTART_CASES.
ALPHA_CASES=(empty_list add_list edit_reorder_status_delete empty_target_cli empty_target_mcp omp_empty_target a_never_sees_b no_write_to_b settings_off)
# M-11 adds its page cases to these two lists.
PAGE_CASES=()
PAGE_RESTART_CASES=()
MODEL_CASES=(omp_empty_target)
if [[ "$EVAL_OS" == macos ]]; then HOLD_AGENT=omp; else HOLD_AGENT=puppet; fi
PUPPET="$EVAL_HOME/.puppet"

q() { sqlite3 "file:$DB?mode=ro" "$1" 2>>"$RUN_LOG" || true; }
rows() { sqlite3 -json "file:$DB?mode=ro" "$1" 2>>"$RUN_LOG" || true; }
# The app's CLI as one Mono; each call and reply also goes to run.log.
cli() {
  local reply
  reply="$("$G/cli.sh" "$@" 2>>"$RUN_LOG" || true)"
  printf '%s %s %s -> %s\n' "$1" "$2" "${3:-}" "$reply" >>"$RUN_LOG"
  printf '%s\n' "$reply"
}
jv() { jq -r "$2" <<<"$1" 2>/dev/null || true; }
code() { jv "$1" 'if .ok then "ok" else (.code // "uncoded") end'; }
# try <action> [json]: Alpha's call; appends its code ("ok" on success) to $codes.
try() { codes+="${codes:+ }$(code "$(cli alpha "$@")")"; }
# need <evidence on failure> <test...>: sets EV and fails the case.
need() {
  local why="$1"; shift
  "$@" && return 0
  EV="${why//$'\n'/ }"
  return 1
}

# A requested habit run whose agent sleeps, so the Mono's app token stays live.
hold() {
  mkdir -p "$DATA/monos/$1"
  local first=""
  [[ -n "${3:-}" ]] && first="First do only this: $3 Then run"
  cat >"$DATA/monos/$1/habits.json" <<EOF
[{"id":"hold-$1-$RUN_ID","name":"Hold","instructions":"${first:-Run} exactly this shell command and nothing else: sleep $2. When it finishes, reply with the single word held.","schedule":{"scheduleKind":"daily","time":"09:00","minute":0,"dayOfWeek":1},"enabled":false,"createdAt":0,"nextRunAt":0,"runRequested":true}]
EOF
  [[ "$HOLD_AGENT" == puppet ]] || return 0
  jq --arg s "$2" '.rules += [{match: "sleep \($s).", exec: [["sleep", $s]], reply: "held"}]' \
    "$PUPPET/script.json" >"$PUPPET/script.json.tmp"
  mv "$PUPPET/script.json.tmp" "$PUPPET/script.json"
}

# Every session on fx, which resolves to the puppet ($HOME/.local/bin/fx comes first).
use_puppet() {
  "$G/instance.sh" scrub fx:zai/glm-5.2-fast >>"$RUN_LOG" 2>&1
  mkdir -p "$EVAL_HOME/.local/bin" "$PUPPET"
  install -m 755 "$ROOT/scripts/scenarios/puppet/puppet.mjs" "$EVAL_HOME/.local/bin/fx"
  jq -n --arg app "$(target_bin instance)" '{app: $app, rules: []}' >"$PUPPET/script.json"
}

wait_turn() {
  local deadline=$((SECONDS + $2))
  while ((SECONDS < deadline)); do
    "$G/cli.sh" "$1" monos.list >/dev/null 2>>"$RUN_LOG" && return 0
    sleep 3
  done
  return 1
}

# Processes whose cwd is in the eval HOME: only an earlier run can own them,
# such as harness processes orphaned when a run or its instance was killed.
sweep_orphans() {
  local pids
  pids="$(lsof -a -d cwd -F pn 2>/dev/null | awk -v h="$EVAL_HOME" '
    /^p/ { pid = substr($0, 2) }
    /^n/ { n = substr($0, 2); sub(/^\/private/, "", n); if (n == h || index(n, h "/") == 1) print pid }' | sort -u)"
  [[ -n "$pids" ]] || return 0
  log "stopping $(wc -l <<<"$pids" | tr -d ' ') leftover processes from an earlier run"
  kill -TERM $pids 2>/dev/null || true
  sleep 1
  kill -KILL $pids 2>/dev/null || true
}

git_repo() {
  mkdir -p "$1" && git -C "$1" init -q -b main && echo "# $(basename "$1")" >"$1/README.md" &&
    git -C "$1" add README.md && git -C "$1" -c user.name=e2e -c user.email=e2e@local commit -qm init
}

reset_state() {
  "$G/instance.sh" stop >>"$RUN_LOG" 2>&1 || true
  sweep_orphans
  rm -f "$RESULTS" "$EVAL_OUT"/cli-pid-*
  "$G/instance.sh" home --fresh >>"$RUN_LOG" 2>&1
  mkdir -p "$DATA"
  git_repo "$A"
  git_repo "$B"
  sed -e "s|@HOME@|$EVAL_HOME|g" -e "s|@A@|$A|g" -e "s|@B@|$B|g" "$HERE/fixtures/pre-m.sql" | sqlite3 "$DB"
  [[ "$HOLD_AGENT" == omp ]] || use_puppet
  hold beta 30 'call your add_milestone tool with title \"Omp dated\" and target \"2026-12-12\", then call your update_milestone tool with the id it returned and target set to the empty string \"\". Do not retry a refused call.'
  hold alpha 900
}

pick_app() {
  if [[ -n "${MILESTONES_E2E_APP:-}" ]]; then
    [[ -x "$(app_bin "$MILESTONES_E2E_APP")" ]] || die "no app at $MILESTONES_E2E_APP"
    echo "$MILESTONES_E2E_APP" >"$EVAL_OUT/instance.app"
  elif [[ "${1:-}" == --build || ! -s "$EVAL_OUT/instance.app" ]]; then
    "$G/instance.sh" build "$ROOT" head
  fi
  local app label
  app="$(cat "$EVAL_OUT/instance.app")"
  label="$(basename "$(dirname "$app")")"
  log "app $app (rev $(cut -c1-10 "$EVAL_OUT/src/$label/.giga-eval-rev" 2>/dev/null || echo unknown))"
}

case_migration() {
  local tables kept
  tables="$(q "SELECT group_concat(name) FROM (SELECT name FROM sqlite_master WHERE name LIKE 'milestone%' ORDER BY name)")"
  need "milestone tables after migrate: ${tables:-none}" \
    [ "$tables" = "milestone_settings,milestones,milestones_project_order" ] || return 1
  kept="$(q "SELECT (SELECT count(*) FROM schema_migrations WHERE version BETWEEN 1 AND 19)
    || ',' || (SELECT title FROM sessions WHERE id = '33333333-3333-4333-8333-333333333333' AND cwd = '$A')
    || ',' || (SELECT group_concat(id) FROM (SELECT id FROM monos ORDER BY sort_order))")"
  need "pre-M rows after migrate: $kept" [ "$kept" = "19,Before M,beta,alpha" ] || return 1
  EV="tables $tables created; 19 version rows, the Before M session and both Monos kept"
}

case_empty_list() {
  local r
  r="$(cli alpha milestones.list)"
  need "list: ${r:0:200}" [ "$(jv "$r" '[.ok, .result.project, (.result.milestones | length), .result.settings.enabled] | @csv')" = "true,\"$A\",0,true" ] || return 1
  EV="project A lists 0 milestones, settings enabled"
}

case_add_list() {
  local r1 r2 l
  r1="$(cli alpha milestones.add '{"title":"Ship the harness","note":"After the E2E lands"}')"
  r2="$(cli alpha milestones.add '{"title":"Dark mode","status":"planned","target":"2026-12-01"}')"
  ID1="$(jv "$r1" '.result.id // empty')"
  ID2="$(jv "$r2" '.result.id // empty')"
  need "add 1: ${r1:0:200}" [ "$(jv "$r1" '.result | [.title, .note, .status, .target, .order, .createdBy] | @csv')" = '"Ship the harness","After the E2E lands","idea",,0,"mono:alpha"' ] || return 1
  need "add 2: ${r2:0:200}" [ "$(jv "$r2" '.result | [.title, .status, .target, .order, .createdBy] | @csv')" = '"Dark mode","planned","2026-12-01",1,"mono:alpha"' ] || return 1
  l="$(cli alpha milestones.list)"
  need "list: ${l:0:200}" [ "$(jv "$l" '[.result.milestones[] | .id + ":" + .title] | join(",")')" = "$ID1:Ship the harness,$ID2:Dark mode" ] || return 1
  need "rows not in project A" [ "$(q "SELECT count(*) FROM milestones WHERE project_path = '$A' AND id IN ('$ID1', '$ID2')")" = 2 ] || return 1
  EV="two adds by mono:alpha, list returns both in order, rows stored under A"
}

case_edit_reorder_status_delete() {
  local r ids
  r="$(cli alpha milestones.update "{\"id\":\"$ID1\",\"title\":\"Ship the E2E harness\",\"note\":\"Edited\"}")"
  need "edit: ${r:0:200}" [ "$(jv "$r" '.result | [.title, .note, .status] | @csv')" = '"Ship the E2E harness","Edited","idea"' ] || return 1
  r="$(cli alpha milestones.update "{\"id\":\"$ID1\",\"status\":\"in_progress\"}")"
  need "status: ${r:0:200}" [ "$(jv "$r" '.result | [.title, .status] | @csv')" = '"Ship the E2E harness","in_progress"' ] || return 1
  r="$(cli alpha milestones.update "{\"id\":\"$ID2\",\"target\":null}")"
  need "clear target: ${r:0:200}" [ "$(jv "$r" '.result | [.status, .target] | @csv')" = '"planned",' ] || return 1
  r="$(cli alpha milestones.reorder "{\"ids\":[\"$ID2\",\"$ID1\"]}")"
  need "reorder: ${r:0:200}" [ "$(jv "$r" '[.result.milestones[].id] | join(",")')" = "$ID2,$ID1" ] || return 1
  ids="$(jv "$(cli alpha milestones.list)" '[.result.milestones[].id] | join(",")')"
  need "list after reorder: $ids" [ "$ids" = "$ID2,$ID1" ] || return 1
  r="$(cli alpha milestones.remove "{\"id\":\"$ID2\"}")"
  need "remove: ${r:0:200}" [ "$(jv "$r" '.result.removed')" = "$ID2" ] || return 1
  ids="$(jv "$(cli alpha milestones.list)" '[.result.milestones[].id] | join(",")')"
  need "list after remove: $ids" [ "$ids" = "$ID1" ] || return 1
  r="$(cli alpha milestones.remove "{\"id\":\"$ID2\"}")"
  need "second remove: ${r:0:200}" [ "$(jv "$r" .code)" = unknown_milestone ] || return 1
  EV="edit, status in_progress, target cleared, reorder kept, remove leaves 1, second remove unknown_milestone"
}

# "" is Later on the CLI path: add stores NULL, update with "" clears a date.
case_empty_target_cli() {
  local r1 r2 r3 id1 id2 stored
  r1="$(cli alpha milestones.add '{"title":"CLI later","target":""}')"
  r2="$(cli alpha milestones.add '{"title":"CLI dated","target":"2026-12-12"}')"
  id1="$(jv "$r1" '.result.id // empty')"
  id2="$(jv "$r2" '.result.id // empty')"
  r3="$(cli alpha milestones.update "{\"id\":\"$id2\",\"target\":\"\"}")"
  stored="$(q "SELECT group_concat(title || ':' || ifnull(target, 'NULL'), ',') FROM (SELECT title, target FROM milestones WHERE id IN ('$id1', '$id2') ORDER BY sort_order)")"
  cli alpha milestones.remove "{\"id\":\"$id1\"}" >/dev/null
  cli alpha milestones.remove "{\"id\":\"$id2\"}" >/dev/null
  need "add \"\": ${r1:0:200}" [ "$(jv "$r1" '[.ok, .result.target] | @csv')" = "true," ] || return 1
  need "update \"\": ${r3:0:200}" [ "$(jv "$r3" '[.ok, .result.target] | @csv')" = "true," ] || return 1
  need "rows: $stored" [ "$stored" = "CLI later:NULL,CLI dated:NULL" ] || return 1
  EV="add target \"\" answers null and stores NULL; update \"\" clears 2026-12-12 to NULL"
}

# The same through the real app-mcp server's tools/call.
case_empty_target_mcp() {
  local r1 r2 r3 id1 id2 stored
  r1="$(cli alpha app-mcp '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"add_milestone","arguments":{"title":"MCP later","target":""}}}')"
  r2="$(cli alpha milestones.add '{"title":"MCP dated","target":"2026-12-12"}')"
  id1="$(jv "$r1" '.result.structuredContent.result.id // empty')"
  id2="$(jv "$r2" '.result.id // empty')"
  r3="$(cli alpha app-mcp "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"update_milestone\",\"arguments\":{\"id\":\"$id2\",\"target\":\"\"}}}")"
  stored="$(q "SELECT group_concat(title || ':' || ifnull(target, 'NULL'), ',') FROM (SELECT title, target FROM milestones WHERE id IN ('$id1', '$id2') ORDER BY sort_order)")"
  cli alpha milestones.remove "{\"id\":\"$id1\"}" >/dev/null
  cli alpha milestones.remove "{\"id\":\"$id2\"}" >/dev/null
  need "mcp add \"\": ${r1:0:300}" [ "$(jv "$r1" '.result.structuredContent | [.ok, .result.target] | @csv')" = "true," ] || return 1
  need "mcp update \"\": ${r3:0:300}" [ "$(jv "$r3" '.result.structuredContent | [.ok, .result.target] | @csv')" = "true," ] || return 1
  need "rows: $stored" [ "$stored" = "MCP later:NULL,MCP dated:NULL" ] || return 1
  EV="app-mcp add_milestone target \"\" stores NULL; update_milestone \"\" clears a date to NULL"
}

# Beta's held omp turn added a dated row with its host tool, then cleared the
# date with update_milestone target "". A field omp dropped, or a refusal,
# would leave the date.
case_omp_empty_target() {
  local row deadline=$((SECONDS + 90))
  until row="$(q "SELECT count(*) || ':' || ifnull(max(target), 'NULL') || ':' || ifnull(max(created_by), '') || ':' || (max(updated_at) > max(created_at)) FROM milestones WHERE project_path = '$B' AND title = 'Omp dated'")"
    [[ "$row" == 1:NULL:* ]] || ((SECONDS > deadline)); do sleep 3; done
  need "B rows titled Omp dated (count:target:by:updated): $row" [ "$row" = "1:NULL:mono:beta:1" ] || return 1
  EV="omp host tools: add_milestone with 2026-12-12, then update_milestone target \"\" cleared it to NULL (by mono:beta)"
}

case_a_never_sees_b() {
  local l r
  need "Beta's adds to B: $B_EV" [ -n "$B_IDS" ] || return 1
  l="$(cli alpha milestones.list)"
  need "list: ${l:0:200}" [ "$(jv "$l" '.ok')" = true ] || return 1
  need "A's list holds a B row: ${l:0:300}" [ "$(jq -r --argjson b "$B_IDS" '[.result.milestones[].id | select(IN($b[]))] | length' <<<"$l")" = 0 ] || return 1
  r="$(cli alpha milestones.list "{\"project\":\"$B\"}")"
  need "list of B: ${r:0:200}" [ "$(jv "$r" '[.ok, (.error | startswith("Not one of your projects"))] | @csv')" = "false,true" ] || return 1
  EV="Beta stored 2 rows in B; A's list has none of them; listing B is refused"
}

case_no_write_to_b() {
  local before after b1 b2 codes=""
  need "Beta's adds to B: $B_EV" [ -n "$B_IDS" ] || return 1
  b1="$(jq -r '.[0]' <<<"$B_IDS")"
  b2="$(jq -r '.[1]' <<<"$B_IDS")"
  before="$(rows "SELECT * FROM milestones WHERE project_path = '$B' ORDER BY sort_order")"
  try milestones.add "{\"title\":\"Sneak\",\"project\":\"$B\"}"
  try milestones.update "{\"id\":\"$b1\",\"title\":\"Hijacked\",\"status\":\"done\"}"
  try milestones.update "{\"id\":\"$b1\",\"title\":\"Hijacked\",\"project\":\"$B\"}"
  try milestones.remove "{\"id\":\"$b2\"}"
  try milestones.reorder "{\"ids\":[\"$ID1\",\"$b1\"]}"
  after="$(rows "SELECT * FROM milestones WHERE project_path = '$B' ORDER BY sort_order")"
  need "B rows changed by Alpha: $after" [ "$before" = "$after" ] || return 1
  need "codes: $codes" [ "$codes" = "invalid_input unknown_milestone invalid_input unknown_milestone stale_order" ] || return 1
  EV="add/update/remove/reorder on B refused ($codes); B rows unchanged"
}

case_settings_off() {
  local before after codes="" l r
  sqlite3 "$DB" "INSERT INTO milestone_settings (project_path, enabled, hidden_statuses, updated_at)
    VALUES ('$B', 0, '[]', 0) ON CONFLICT (project_path) DO UPDATE SET enabled = 0"
  r="$(cli alpha milestones.add '{"title":"While B is off"}')"
  l="$(cli alpha milestones.list)"
  need "A with only B off: add ${r:0:150}; list ${l:0:150}" [ "$(jv "$r" .ok),$(jv "$l" .result.settings.enabled)" = "true,true" ] || return 1
  before="$(rows "SELECT * FROM milestones WHERE project_path = '$A' ORDER BY sort_order")"
  sqlite3 "$DB" "INSERT INTO milestone_settings (project_path, enabled, hidden_statuses, updated_at)
    VALUES ('$A', 0, '[]', 0) ON CONFLICT (project_path) DO UPDATE SET enabled = 0"
  try milestones.add '{"title":"While off"}'
  try milestones.update "{\"id\":\"$ID1\",\"status\":\"done\"}"
  try milestones.remove "{\"id\":\"$ID1\"}"
  try milestones.reorder "{\"ids\":[\"$ID1\"]}"
  l="$(cli alpha milestones.list)"
  after="$(rows "SELECT * FROM milestones WHERE project_path = '$A' ORDER BY sort_order")"
  sqlite3 "$DB" "UPDATE milestone_settings SET enabled = 1 WHERE project_path = '$A'"
  need "codes while off: $codes" [ "$codes" = "milestones_off milestones_off milestones_off milestones_off" ] || return 1
  need "list while off: ${l:0:200}" [ "$(jv "$l" '[.ok, .result.settings.enabled, (.result.milestones | length)] | @csv')" = "true,false,2" ] || return 1
  need "A rows changed while off" [ "$before" = "$after" ] || return 1
  r="$(cli alpha milestones.add '{"title":"Voice notes"}')"
  need "add after turning A on, B still off: ${r:0:200}" [ "$(jv "$r" '.ok')" = true ] || return 1
  EV="B off leaves A on (add ok, enabled:true); A off: 4 writes milestones_off, list enabled:false, rows unchanged; A on again, add works"
}

case_restart() {
  local shape='[.result.milestones[] | [.id, .title, .status, .target, .order, .createdBy]]' before after
  before="$(jv "$(cli alpha milestones.list)" "$shape")"
  "$G/instance.sh" stop >>"$RUN_LOG" 2>&1
  hold alpha 120
  "$G/instance.sh" start >>"$RUN_LOG" 2>&1 || { EV="instance did not restart"; return 1; }
  need "Alpha's held run after restart never started" wait_turn alpha 300 || return 1
  after="$(jv "$(cli alpha milestones.list)" "$shape")"
  need "no rows before restart" [ "$(jq length <<<"$before" 2>/dev/null)" -gt 0 ] || return 1
  need "after restart: $(jq -c . <<<"$after")" [ "$before" = "$after" ] || return 1
  EV="$(jq length <<<"$before") rows with ids, fields and order unchanged after restart"
}

# run_case <kind> <name>: one results.tsv row per case.
run_case() {
  EV=""
  if [[ "$HOLD_AGENT" == puppet && " ${MODEL_CASES[*]} " == *" $2 "* ]]; then
    record "$2" "$1" SKIP "needs omp and a real model; runs on macOS only"
  elif [[ -n "$SKIP" ]]; then
    record "$2" "$1" FAIL "$SKIP"
  elif "case_$2"; then
    record "$2" "$1" PASS "$EV"
  else
    record "$2" "$1" FAIL "$EV"
  fi
}

beta_turn() {
  local r1 r2
  if ! wait_turn beta 300; then B_EV="Beta's held run never started"; return; fi
  r1="$(cli beta milestones.add '{"title":"B roadmap one"}')"
  r2="$(cli beta milestones.add '{"title":"B roadmap two","status":"planned"}')"
  B_EV="${r1:0:150} ${r2:0:150}"
  if [[ "$(jv "$r1" .result.createdBy)$(jv "$r2" .result.createdBy)" == "mono:betamono:beta" ]]; then
    B_IDS="$(jq -c -n --arg a "$(jv "$r1" .result.id)" --arg b "$(jv "$r2" .result.id)" '[$a, $b]')"
  fi
}

main() {
  local started=$SECONDS name
  : >"$RUN_LOG"
  pick_app "${1:-}"
  reset_state
  trap '"$G/instance.sh" stop >>"$RUN_LOG" 2>&1 || true' EXIT
  "$G/instance.sh" start >>"$RUN_LOG" 2>&1 || die "instance did not start, see $RUN_LOG"
  SKIP="" ID1="" ID2="" B_IDS="" B_EV=""
  run_case backend migration
  beta_turn
  wait_turn alpha 300 || SKIP="Alpha's held run never started"
  for name in "${ALPHA_CASES[@]}"; do run_case backend "$name"; done
  for name in ${PAGE_CASES[@]+"${PAGE_CASES[@]}"}; do run_case page "$name"; done
  run_case backend restart
  for name in ${PAGE_RESTART_CASES[@]+"${PAGE_RESTART_CASES[@]}"}; do run_case page "$name"; done
  "$G/instance.sh" stop >>"$RUN_LOG" 2>&1 || true
  local total failed skipped
  total="$(wc -l <"$RESULTS" | tr -d ' ')"
  failed="$(awk -F'\t' '$4 != "PASS" && $4 != "SKIP"' "$RESULTS" | wc -l | tr -d ' ')"
  skipped="$(awk -F'\t' '$4 == "SKIP"' "$RESULTS" | wc -l | tr -d ' ')"
  echo "milestones-e2e: $((total - failed - skipped))/$total cases passed, $skipped skipped, in $((SECONDS - started))s; results $RESULTS"
  [[ "$failed" == 0 ]]
}

main "$@"
