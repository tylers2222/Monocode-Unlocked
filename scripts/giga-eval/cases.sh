#!/usr/bin/env bash
# Live end-to-end cases for Change F (shared Giga workspace). Contract: /tmp/mc947/changeF-contract.md
#
#   cases.sh <instance|installed> <case...|all>
#
# instance  = the isolated GigaEval build (instance.sh), HOME=/tmp/giga-eval-home.
# installed = /Applications/MonoCode.app with the real HOME. Only case 1 runs there,
#             and only with GIGA_EVAL_CONFIRM_INSTALLED=yes.
#
# Monos the cases use (names; created on the instance when missing):
#   GIGA_EVAL_GIGA   (default "Giga Eval")      a Giga Mono
#   GIGA_EVAL_GIGA2  (default "Giga Eval Two")  a second Giga Mono, case 5
#   GIGA_EVAL_PROBE  (default "Probe")          a regular Mono: reads workspace.status and other chats
# Every check prints PASS/FAIL/INFO with its evidence and appends to /tmp/giga-eval/results.tsv.
source "$(dirname "$0")/lib.sh"
set +e   # a failed check is recorded, not fatal; the guards below return explicitly

TARGET="${1:?target: instance|installed}"; shift
[[ $# -gt 0 ]] || die "name the cases: 1..7 or all"
CASES=("$@"); [[ "${CASES[*]}" == "all" ]] && CASES=(7 1 2 3 4 5 6)
for c in "${CASES[@]}"; do
  [[ "$c" =~ ^[1-7]$ ]] || die "unknown case $c"
  [[ "$TARGET" == "installed" && "$c" != 1 ]] && die "case $c refuses target 'installed': only case 1 may touch the real HOME"
done
if [[ "$TARGET" == "installed" && "${GIGA_EVAL_CONFIRM_INSTALLED:-}" != "yes" ]]; then
  die "case 1 on 'installed' deletes the real ~/MonoCode after backing it up; set GIGA_EVAL_CONFIRM_INSTALLED=yes"
fi

export GIGA_EVAL_TARGET="$TARGET"
export RUN_ID="${RUN_ID:-$(date +%Y%m%d-%H%M%S)}"
HOME_T="$(target_home "$TARGET")"
ROOT="$HOME_T/MonoCode"
RUN_DIR="$EVAL_OUT/run-$RUN_ID"
GIGA="${GIGA_EVAL_GIGA:-Giga Eval}"
GIGA2="${GIGA_EVAL_GIGA2:-Giga Eval Two}"
PROBE="${GIGA_EVAL_PROBE:-Probe}"
SCRATCH="$HOME_T/$SCRATCH_NAME"
mkdir -p "$RUN_DIR"
log "run $RUN_ID target=$TARGET root=$ROOT evidence=$RUN_DIR"

ui() { "$EVAL_DIR/ui.sh" "$@"; }
cli() { "$EVAL_DIR/cli.sh" "$@"; }
nonce() { LC_ALL=C tr -dc 'a-z0-9' </dev/urandom | head -c 8; }
slugify() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | sed -E 's/[^a-z0-9]+/-/g; s/^-+//; s/-+$//' | cut -c1-48; }
file_sha() { shasum -a 256 "$1" 2>/dev/null | cut -c1-64; }
# Paths and sha256 of every file outside .git: the comparable content of a workspace.
manifest() { (cd "$1" && find . -path ./.git -prune -o -type f -print0 | sort -z | xargs -0 shasum -a 256); }
wait_path() {   # wait_path <path> <seconds> [test flag, default -e]
  local i
  for (( i = 0; i < $2; i++ )); do test "${3:--e}" "$1" && return 0; sleep 1; done
  return 1
}
receipt_count() { [[ -f "$ROOT/gigas/$1/log.jsonl" ]] && grep -c . "$ROOT/gigas/$1/log.jsonl" || echo 0; }
# Every case's Mono runs on this exact harness and model.
MODEL_TAB="omp" MODEL_ROW="Default, omp" MODEL_LABEL="omp Default" MODEL_HARNESS="omp" MODEL_ID="omp:default"

# ---- the instance's own database, read-only ------------------------------------
DB="$HOME_T/Library/Application Support/$EVAL_ID/monocode.db"
[[ "$TARGET" == installed ]] && DB="$REAL_APP_DATA/monocode.db"
dbq() { sqlite3 -readonly -separator $'\t' "$DB" "$@"; }
sqlq() { printf "'%s'" "${1//\'/\'\'}"; }   # a SQL string literal
# Every stored chat block: session id, block JSON. A Mono's chat is in mono_blocks,
# any other session's in sessions.blocks_json.
ALL_BLOCKS="select session_id, block_json from mono_blocks
  union all select s.id, b.value from sessions s, json_each(s.blocks_json) b"
# The Giga's chat session: the newest chat receipt in its log.jsonl.
giga_session() {   # giga_session <log.jsonl>
  jq -r 'select(.kind == "chat") | .sessionId // empty' "$1" 2>/dev/null | tail -1
}
# status_blocks <session|-> <out>: that chat's "Giga workspace:" blocks ("-" = every chat),
# one "<id> TAB <text>" line each in <out>; prints the count.
status_blocks() {
  local where=""; [[ "$1" == - ]] || where="and session_id = $(sqlq "$1")"
  dbq "select distinct json_extract(block_json, '\$.id'), replace(json_extract(block_json, '\$.text'), char(10), ' ') from ($ALL_BLOCKS)
       where json_extract(block_json, '\$.text') like 'Giga workspace:%' $where" >"$2"
  grep -c . "$2" || true
}
# assistant_replies <session> <word>: assistant blocks in that chat whose text holds <word>.
assistant_replies() {
  dbq "select count(distinct json_extract(block_json, '\$.id')) from ($ALL_BLOCKS)
       where session_id = $(sqlq "$1") and json_extract(block_json, '\$.role') = 'assistant'
       and lower(json_extract(block_json, '\$.text')) like $(sqlq "%$2%")"
}

# ---- app lifecycle per target ------------------------------------------------
app_stop() {
  if [[ "$TARGET" == instance ]]; then "$EVAL_DIR/instance.sh" stop
  else
    osascript -e 'quit app id "com.monocode.desktop"'
    local i; for i in $(seq 1 30); do [[ -z "$(target_pid installed)" ]] && return 0; sleep 1; done
    die "installed MonoCode did not quit"
  fi
}
app_start() {
  if [[ "$TARGET" == instance ]]; then "$EVAL_DIR/instance.sh" start
  else
    open -a "$INSTALLED_APP"
    local i; for i in $(seq 1 60); do [[ -n "$(target_pid installed)" ]] && { sleep 8; return 0; }; sleep 1; done
    die "installed MonoCode did not start"
  fi
}

# ---- reading the app -----------------------------------------------------------
# as_mono <mono> <action> <json> <out>: hold a turn on <mono> and run one CLI
# call during it. The app accepts the CLI only while the caller runs a turn.
as_mono() {
  local mono="$1" action="$2" json="$3" out="$4" i
  ui hold "$mono" 45 >/dev/null || return 1
  for i in $(seq 1 45); do
    if cli "$mono" "$action" "$json" >"$out" 2>>"$out.err"; then
      ui wait-idle "$mono" 180 >/dev/null
      return 0
    fi
    grep -q 'Unknown action' "$out" 2>/dev/null && break   # this build has no such action
    sleep 2
  done
  ui wait-idle "$mono" 180 >/dev/null
  return 1
}
# status_as <mono> <out>: workspace.status as that Mono. A Giga's held turn is
# itself a "turn" trigger; a regular Mono's turn is assumed to trigger nothing.
status_as() { as_mono "$1" workspace.status '{}' "$2"; }
jget() { jq -r "$1" "$2" 2>/dev/null; }

# tree_check <case> <label> <slug|-> [expect_log]: the contract's workspace layout.
tree_check() {
  local c="$1" label="$2" slug="$3" expect_log="${4:-0}" ev="$RUN_DIR/case$1-$2-tree.txt"
  (cd "$ROOT" 2>/dev/null && find . -path ./.git -prune -o -print | sort) >"$ev"
  local agents="$ROOT/AGENTS.md"
  check "$c" "$label: root is a git repo" "$ev" test -d "$ROOT/.git"
  check "$c" "$label: AGENTS.md has one managed block" "$agents" \
    test "$(grep -c '^<!-- monocode:managed:start -->$' "$agents" 2>/dev/null)" = 1 -a "$(grep -c '^<!-- monocode:managed:end -->$' "$agents" 2>/dev/null)" = 1
  check "$c" "$label: .gitignore has index/, gigas/*/state/, gigas/*/scratch/" "$ROOT/.gitignore" \
    bash -c 'for l in "index/" "gigas/*/state/" "gigas/*/scratch/"; do grep -qxF "$l" "$1" || exit 1; done' _ "$ROOT/.gitignore"
  check "$c" "$label: shared/CONVENTIONS.md" "$ev" test -f "$ROOT/shared/CONVENTIONS.md"
  local fts; fts="$(sqlite3 -readonly "$ROOT/index/index.db" "select sql from sqlite_master where name='entries'" 2>&1)"
  echo "$fts" >"$RUN_DIR/case$c-$label-index-sql.txt"
  check "$c" "$label: index/index.db has FTS5 table entries(source, giga, path, at, text)" "$RUN_DIR/case$c-$label-index-sql.txt" \
    bash -c '[[ "$1" == *[Ff][Tt][Ss]5* ]] && for k in source giga path at text; do [[ "$1" == *"$k"* ]] || exit 1; done' _ "$fts"
  check "$c" "$label: managed pre-commit hook" "$ROOT/.git/hooks/pre-commit" \
    bash -c '[[ -x "$1" && "$(sed -n 2p "$1")" == "# monocode:managed pre-commit" ]]' _ "$ROOT/.git/hooks/pre-commit"
  [[ "$slug" == "-" ]] && return 0
  local g="$ROOT/gigas/$slug" part missing=""
  for part in AGENTS.md briefs runbooks projects monos state scratch; do [[ -e "$g/$part" ]] || missing+=" $part"; done
  [[ "$expect_log" == 1 && ! -f "$g/log.jsonl" ]] && missing+=" log.jsonl"
  check "$c" "$label: gigas/$slug/ skeleton${missing:+ (missing:$missing)}" "$ev" test -z "$missing"
}

ensure_mono() {   # ensure_mono <name> <giga 0|1>
  local name="$1" giga="$2"
  # Create only when no sidebar row exists: a failed open must not make a second Mono of that name.
  if ui has-mono "$name"; then ui open-mono "$name" >/dev/null || die "cannot open Mono $name"
  else ui create-mono "$name" || die "cannot create Mono $name"; fi
  ui set-model "$name" "$MODEL_TAB" "$MODEL_ROW" "$MODEL_LABEL" >/dev/null || die "cannot set $name to $MODEL_ID"
  if [[ "$giga" == 1 ]]; then ui giga-on "$name" >/dev/null || die "cannot turn Giga on for $name"; fi
}
ensure_monos() {
  [[ "$TARGET" == instance ]] || return 0
  ensure_mono "$PROBE" 0
  ensure_mono "$GIGA" 1
}
giga_slug() {   # gigaSlug from workspace.status as that Giga; falls back to the contract's slug rule
  local out="$RUN_DIR/slug-$(slugify "$1").json"
  [[ -s "$out" ]] || status_as "$1" "$out" >/dev/null
  local s; s="$(jget '.result.gigaSlug // empty' "$out")"
  [[ -n "$s" ]] && echo "$s" || slugify "$1"
}
# model_check <case> <label> <session>: that chat's stored harness and model.
model_check() {
  local got; got="$(dbq "select harness || ' ' || model from sessions where id = $(sqlq "$3")")"
  check "$1" "$2 runs on $MODEL_HARNESS $MODEL_ID (sessions row)" "$DB sessions.id=$3: ${got:-no row}" \
    test "$got" = "$MODEL_HARNESS $MODEL_ID"
}

# ================================================================================
case_1() {
  local c=1 d="$RUN_DIR/case1" stamp; stamp="$(date +%Y%m%d-%H%M%S)"
  mkdir -p "$d"
  ensure_monos
  local backup="$HOME_T/MonoCode.giga-eval-backup-$stamp"
  local had_root=0
  if [[ -e "$ROOT" || -L "$ROOT" ]]; then
    had_root=1
    cp -a "$ROOT" "$backup" || { record $c "backup" FAIL "cp -a failed; nothing deleted"; return; }
    if ! diff -r --no-dereference "$ROOT" "$backup" >"$d/backup-diff.txt" 2>&1; then
      record $c "backup is identical to the source" FAIL "$d/backup-diff.txt; nothing deleted"; return
    fi
    manifest "$backup" >"$d/backup.manifest"
    git -C "$backup" rev-list --all >"$d/backup.revs" 2>/dev/null
    record $c "backup" PASS "$backup ($(grep -c . "$d/backup.manifest") files, $(grep -c . "$d/backup.revs") commits)"
  else
    record $c "backup" INFO "no $ROOT before the case"
  fi

  # Trigger: app start.
  app_stop; rm -rf "$ROOT"; app_start
  check $c "start: $ROOT recreated" "wait 90s for $ROOT/.git" wait_path "$ROOT/.git" 90 -d
  tree_check $c start "$(slugify "$GIGA")"
  status_as "$PROBE" "$d/status-start.json"
  check $c "start: lastEnsure.trigger == start (read as $PROBE)" "$d/status-start.json" \
    test "$(jget .result.lastEnsure.trigger "$d/status-start.json")" = start

  # Trigger: a Giga turn.
  rm -rf "$ROOT"
  status_as "$GIGA" "$d/status-turn.json"
  check $c "turn: $ROOT recreated" "$ROOT" wait_path "$ROOT/.git" 30 -d
  check $c "turn: lastEnsure.trigger == turn (read as $GIGA during its turn)" "$d/status-turn.json" \
    test "$(jget .result.lastEnsure.trigger "$d/status-turn.json")" = turn
  local slug; slug="$(jget '.result.gigaSlug // empty' "$d/status-turn.json")"; slug="${slug:-$(slugify "$GIGA")}"
  tree_check $c turn "$slug" 1

  # Trigger: Giga turned on.
  rm -rf "$ROOT"
  ui giga-off "$GIGA"; ui giga-on "$GIGA"
  check $c "giga-on: $ROOT recreated" "$ROOT" wait_path "$ROOT/.git" 30 -d
  tree_check $c giga-on "$slug"
  status_as "$PROBE" "$d/status-giga-on.json"
  check $c "giga-on: lastEnsure.trigger == giga-on (read as $PROBE)" "$d/status-giga-on.json" \
    test "$(jget .result.lastEnsure.trigger "$d/status-giga-on.json")" = giga-on

  local m n
  sleep 3   # the app saves a chat after its turn
  n="$(status_blocks - "$d/status-blocks.txt")"
  check $c "no 'Giga workspace:' status in any chat (instance database)" "$d/status-blocks.txt (count=$n)" test "$n" = 0
  ui shot "$d/after-triggers.png" >/dev/null

  [[ "$had_root" == 1 ]] || return 0
  # Restore the backup: its .git history and every user file.
  app_stop
  mv "$ROOT" "$d/recreated-final"
  cp -a "$backup" "$ROOT"
  app_start
  sleep 5
  git -C "$ROOT" rev-list --all >"$d/restored.revs"
  check $c "restore: every backup commit is in the restored history" "$d/backup.revs vs $d/restored.revs" \
    test -z "$(comm -23 <(sort "$d/backup.revs") <(sort "$d/restored.revs"))"
  check $c "restore: backup HEAD is an ancestor of the restored HEAD" "git log: $d/restored-log.txt" \
    git -C "$ROOT" merge-base --is-ancestor "$(git -C "$backup" rev-parse HEAD)" HEAD
  git -C "$ROOT" log --oneline -n 20 >"$d/restored-log.txt"
  manifest "$ROOT" >"$d/restored.manifest"
  # A refresh may rewrite the managed block and .gitignore; every other backup
  # file must be present with the same sha256. Lines left over are lost or changed files.
  comm -23 <(grep -vE '  \./(AGENTS\.md|\.gitignore)$' "$d/backup.manifest" | sort) <(sort "$d/restored.manifest") >"$d/tree-diff.txt"
  check $c "restore: every backup user file is present and byte-identical" "$d/tree-diff.txt ($(grep -c . "$d/tree-diff.txt") lost or changed)" test ! -s "$d/tree-diff.txt"
  outside_block() { sed '/^<!-- monocode:managed:start -->$/,/^<!-- monocode:managed:end -->$/d' "$1"; }
  check $c "restore: AGENTS.md text outside the managed block unchanged" "$backup/AGENTS.md" \
    diff <(outside_block "$backup/AGENTS.md") <(outside_block "$ROOT/AGENTS.md")
  record $c "restore: recreated workspace kept for inspection" INFO "$d/recreated-final"
}

case_2() {
  local c=2 d="$RUN_DIR/case2" tag; tag="$(nonce)"
  mkdir -p "$d"
  ensure_monos
  [[ -d "$ROOT/.git" ]] || status_as "$GIGA" "$d/status-setup.json"
  local slug; slug="$(giga_slug "$GIGA")"

  rm -f "$ROOT/AGENTS.md"
  rm -rf "$ROOT/gigas/$slug/runbooks"
  status_as "$GIGA" "$d/status-1.json"
  check $c "deleted AGENTS.md comes back" "$ROOT/AGENTS.md" test -f "$ROOT/AGENTS.md"
  check $c "deleted gigas/$slug/runbooks/ comes back" "$ROOT/gigas/$slug/runbooks" test -d "$ROOT/gigas/$slug/runbooks"

  local before_lines; before_lines="$(receipt_count "$slug")"
  [[ -n "$slug" ]] || { record $c "slug" FAIL "empty"; return; }
  rm -rf "${ROOT:?}/gigas/$slug"
  status_as "$GIGA" "$d/status-2.json"
  tree_check $c "subtree-deleted" "$slug" 1
  record $c "receipts in log.jsonl before/after deleting gigas/$slug" INFO "$before_lines -> $(receipt_count "$slug") (git still has the deleted history)"

  printf 'GIGA-EVAL user note above the block %s\n' "$tag" | cat - "$ROOT/AGENTS.md" >"$d/agents.tmp" && cp "$d/agents.tmp" "$ROOT/AGENTS.md"
  printf '\nGIGA-EVAL user note below the block %s\n' "$tag" >>"$ROOT/AGENTS.md"
  cp "$ROOT/AGENTS.md" "$d/AGENTS.before-refresh.md"
  app_stop; app_start                       # refresh on start
  status_as "$GIGA" "$d/status-3.json"      # and on a Giga turn
  cp "$ROOT/AGENTS.md" "$d/AGENTS.after-refresh.md"
  check $c "user text above the managed block survives a refresh" "$d/AGENTS.after-refresh.md" \
    grep -qF "GIGA-EVAL user note above the block $tag" "$ROOT/AGENTS.md"
  check $c "user text below the managed block survives a refresh" "$d/AGENTS.after-refresh.md" \
    grep -qF "GIGA-EVAL user note below the block $tag" "$ROOT/AGENTS.md"
  check $c "still exactly one managed block" "$d/AGENTS.after-refresh.md" \
    test "$(grep -c '^<!-- monocode:managed:start -->$' "$ROOT/AGENTS.md")" = 1
}

case_3() {
  local c=3 d="$RUN_DIR/case3"
  mkdir -p "$d"
  ensure_monos
  [[ -d "$ROOT/.git" ]] || status_as "$GIGA" "$d/status-setup.json"
  local slug gs; slug="$(giga_slug "$GIGA")"
  gs="$(giga_session "$ROOT/gigas/$slug/log.jsonl")"
  if [[ -z "$gs" ]]; then status_as "$GIGA" "$d/status-setup-2.json"; gs="$(giga_session "$ROOT/gigas/$slug/log.jsonl")"; fi
  [[ -n "$gs" ]] || { record $c "Giga chat session from its receipts" FAIL "no chat receipt in $ROOT/gigas/$slug/log.jsonl"; return; }
  record $c "Giga chat session (newest chat receipt)" INFO "$gs"
  # new_blocks <before> <after>: status blocks in <after> that <before> lacks.
  new_blocks() { comm -13 <(sort "$1") <(sort "$2"); }
  local saved="$d/MonoCode.saved"
  status_blocks "$gs" "$d/status-0.txt" >/dev/null
  mv "$ROOT" "$saved" || { record $c "move workspace aside" FAIL "$ROOT"; return; }

  # Phase A: ~/MonoCode is a plain file.
  head -c 4096 /dev/urandom >"$ROOT"; local sha_before; sha_before="$(file_sha "$ROOT")"
  local replies0; replies0="$(assistant_replies "$gs" held)"
  status_as "$GIGA" "$d/status-file.json"
  check $c "file: workspace.status state == blocked" "$d/status-file.json" test "$(jget .result.state "$d/status-file.json")" = blocked
  check $c "file: blocked status has a reason" "$(jget .result.reason "$d/status-file.json")" test -n "$(jget '.result.reason // empty' "$d/status-file.json")"
  check $c "file: ~/MonoCode bytes unchanged" "sha256 $sha_before" test "$(file_sha "$ROOT")" = "$sha_before" -a -f "$ROOT" -a ! -L "$ROOT"
  sleep 3   # the app saves a chat after its turn
  local replies1; replies1="$(assistant_replies "$gs" held)"
  check $c "file: Giga turn still replied (assistant 'held' blocks in its chat, instance database)" "$DB session $gs: $replies0 -> $replies1" test "$replies1" -gt "$replies0"
  status_blocks "$gs" "$d/status-file.txt" >/dev/null
  new_blocks "$d/status-0.txt" "$d/status-file.txt" >"$d/status-file-new.txt"
  check $c "file: at least one 'Giga workspace:' status (instance database)" "$d/status-file-new.txt ($(grep -c . "$d/status-file-new.txt") new)" \
    test "$(grep -c . "$d/status-file-new.txt")" -ge 1
  status_as "$GIGA" "$d/status-file-2.json"   # same reason again: no second status
  sleep 3
  status_blocks "$gs" "$d/status-file-2.txt" >/dev/null
  new_blocks "$d/status-file.txt" "$d/status-file-2.txt" >"$d/status-file-2-new.txt"
  check $c "file: no second status for the same reason on the next turn" "$d/status-file-2-new.txt ($(grep -c . "$d/status-file-2-new.txt") new)" \
    test ! -s "$d/status-file-2-new.txt"
  ui shot "$d/file-chat.png" >/dev/null
  rm -f "$ROOT"

  # Phase B: ~/MonoCode is a symlink to /tmp.
  local name; : >"$d/tmp-before.txt"
  for name in AGENTS.md .gitignore .git gigas shared index; do
    printf '%s %s\n' "$name" "$( [[ -e "/tmp/$name" ]] && (find "/tmp/$name" -type f -print0 2>/dev/null | sort -z | xargs -0 shasum -a 256 | shasum -a 256 | cut -c1-16) || echo absent)" >>"$d/tmp-before.txt"
  done
  ln -s /tmp "$ROOT"
  status_as "$GIGA" "$d/status-symlink.json"
  : >"$d/tmp-after.txt"
  for name in AGENTS.md .gitignore .git gigas shared index; do
    printf '%s %s\n' "$name" "$( [[ -e "/tmp/$name" ]] && (find "/tmp/$name" -type f -print0 2>/dev/null | sort -z | xargs -0 shasum -a 256 | shasum -a 256 | cut -c1-16) || echo absent)" >>"$d/tmp-after.txt"
  done
  check $c "symlink: workspace.status state == blocked" "$d/status-symlink.json" test "$(jget .result.state "$d/status-symlink.json")" = blocked
  check $c "symlink: blocked status has a reason" "$(jget .result.reason "$d/status-symlink.json")" test -n "$(jget '.result.reason // empty' "$d/status-symlink.json")"
  check $c "symlink: /tmp not written (workspace names under /tmp unchanged)" "$d/tmp-before.txt vs $d/tmp-after.txt" diff -q "$d/tmp-before.txt" "$d/tmp-after.txt"
  check $c "symlink: ~/MonoCode is still the same symlink" "$(readlink "$ROOT")" test "$(readlink "$ROOT")" = /tmp
  sleep 3
  status_blocks "$gs" "$d/status-symlink.txt" >/dev/null
  new_blocks "$d/status-file-2.txt" "$d/status-symlink.txt" >"$d/status-symlink-new.txt"
  check $c "symlink: at least one 'Giga workspace:' status (instance database)" "$d/status-symlink-new.txt ($(grep -c . "$d/status-symlink-new.txt") new)" \
    test "$(grep -c . "$d/status-symlink-new.txt")" -ge 1
  new_blocks "$d/status-0.txt" "$d/status-symlink.txt" | cut -f2- | sort | uniq -d >"$d/status-repeated.txt"
  check $c "at most one status per distinct reason over both phases" "$d/status-symlink.txt ($(grep -c . "$d/status-repeated.txt") repeated texts in $d/status-repeated.txt)" \
    test ! -s "$d/status-repeated.txt"
  ui shot "$d/symlink-chat.png" >/dev/null

  rm -f "$ROOT"; mv "$saved" "$ROOT"
  record $c "workspace restored" INFO "$ROOT"
}

case_4() {
  local c=4 d="$RUN_DIR/case4" tag; tag="$(nonce)"
  mkdir -p "$d"
  ensure_monos
  [[ -d "$ROOT/.git" ]] || status_as "$GIGA" "$d/status-setup.json"
  local slug; slug="$(giga_slug "$GIGA")"
  [[ -d "$SCRATCH/.git" ]] || { record $c "scratch project" FAIL "$SCRATCH is not a git repo (instance.sh start creates it)"; return; }
  bytes_under() { find "$@" -type f -print0 | xargs -0 stat -f%z | awk '{s += $1} END {print s + 0}'; }
  measure() {
    printf 'bytes=%s files=%s du_k=%s git_bytes=%s git_files=%s git_du_k=%s receipts=%s commits=%s\n' \
      "$(bytes_under "$ROOT" -path "$ROOT/.git" -prune -o)" "$(find "$ROOT" -path "$ROOT/.git" -prune -o -type f -print | wc -l | tr -d ' ')" \
      "$(du -sk "$ROOT" | cut -f1)" "$(bytes_under "$ROOT/.git")" "$(find "$ROOT/.git" -type f | wc -l | tr -d ' ')" \
      "$(du -sk "$ROOT/.git" | cut -f1)" "$(receipt_count "$slug")" "$(git -C "$ROOT" rev-list --count HEAD 2>/dev/null)"
  }
  measure >"$d/size-before.txt"
  local giga_turns_before; giga_turns_before="$(receipt_count "$slug")"
  ui send "$GIGA" "Eval chain $tag. Use the app CLI. 1) Run app monos.list. If no Mono named Worker exists, create it with app monos.create: name Worker, harness $MODEL_HARNESS, model $MODEL_ID, projects [\"$SCRATCH\"], giga false. 2) Send Worker this message with app messages.send, with send_to its mono_id from app messages.get_monos, wanting_reply false and intent \"new\": 'Start one project session with app sessions.start in project $SCRATCH, harness $MODEL_HARNESS, model $MODEL_ID, with this prompt: Append the line CHAIN $tag to chain.txt, then run git add chain.txt and git commit -m \"chain $tag\". Reply with the session ID it returned.' 3) Reply with the message_id that messages.send returned."
  local i committed=0
  for i in $(seq 1 120); do
    git -C "$SCRATCH" log --format='%H %s' | grep -q "chain $tag" && { committed=1; break; }
    sleep 10
  done
  ui wait-idle "$GIGA" 300 >/dev/null
  git -C "$SCRATCH" log --format='%H %an %s' -n 5 >"$d/project-log.txt"
  check $c "project commit 'chain $tag' in $SCRATCH" "$d/project-log.txt" test "$committed" = 1
  check $c "chain.txt has the line" "$SCRATCH/chain.txt" grep -qxF "CHAIN $tag" "$SCRATCH/chain.txt"

  local log="$ROOT/gigas/$slug/log.jsonl"
  grep -F "$tag" "$log" 2>/dev/null | tail -1 >"$d/receipt.json"
  check $c "receipt for the turn in gigas/$slug/log.jsonl" "$d/receipt.json" \
    bash -c 'jq -e ".v == 1 and .kind == \"chat\" and .outcome == \"completed\"" "$1" >/dev/null' _ "$d/receipt.json"
  local to sid; to="$(jget '.sent[0].toMonoId // empty' "$d/receipt.json")"; sid="$(jget '.sessionId // empty' "$d/receipt.json")"
  jq -r '.sent[]?.requestId' "$d/receipt.json" >"$d/receipt-request-ids.txt" 2>/dev/null
  check $c "receipt lists a messages.send request ID" "$d/receipt-request-ids.txt toMonoId=$to" bash -c 'grep -q "^app-" "$1" && [[ -n "$2" ]]' _ "$d/receipt-request-ids.txt" "$to"
  git -C "$ROOT" log --format='%H%x09%an <%ae>%x09%s' -n 10 >"$d/workspace-log.txt"
  check $c "Giga commit with the contract's author and subject" "$d/workspace-log.txt" \
    grep -qF "$(printf 'MonoCode Giga %s <giga+%s@monocode.local>\tgiga(%s): completed turn %s' "$slug" "$slug" "$slug" "${sid:0:8}")" "$d/workspace-log.txt"
  model_check $c "Giga chat" "$sid"

  # Link, all from the instance database: receipt sent[].requestId -> the Worker's user
  # block with that appRequestId -> the session it started in the scratch project.
  local req ids_sql; ids_sql="$(sed "s/'/''/g; s/.*/'&'/" "$d/receipt-request-ids.txt" | paste -sd, -)"
  dbq "select session_id, json_extract(block_json, '\$.appRequestId'), replace(json_extract(block_json, '\$.text'), char(10), ' ')
       from ($ALL_BLOCKS) where json_extract(block_json, '\$.role') = 'user'
       and json_extract(block_json, '\$.appRequestId') in (${ids_sql:-''})" >"$d/worker-user-block.tsv"
  local wses; wses="$(cut -f1 "$d/worker-user-block.tsv" | head -1)"; req="$(cut -f2 "$d/worker-user-block.tsv" | head -1)"
  check $c "Worker's user block carries the receipt's request ID as appRequestId" "$d/worker-user-block.tsv (requestId=${req:-none}, Worker chat session=${wses:-none})" \
    bash -c '[[ -n "$1" && "$2" != "$3" ]] && grep -qF "$4" "$5"' _ "$wses" "$wses" "$sid" "$tag" "$d/worker-user-block.tsv"
  model_check $c "Worker chat" "$wses"
  dbq "select id, cwd, harness, model, created_at from sessions where (cwd like '%$SCRATCH_NAME%' or worktree_cwd like '%$SCRATCH_NAME%')
       and blocks_json like $(sqlq "%CHAIN $tag%")" >"$d/started-session.tsv"
  local wsid; wsid="$(cut -f1 "$d/started-session.tsv" | head -1)"
  dbq "select count(*) from mono_blocks where session_id = $(sqlq "$wses") and block_json like $(sqlq "%$wsid%")" >"$d/worker-names-started.txt"
  check $c "Worker's sessions.start session: a scratch-project session with the task, named in the Worker's chat" \
    "$d/started-session.tsv (session=${wsid:-none}); Worker blocks naming it: $(cat "$d/worker-names-started.txt")" \
    test -n "$wsid" -a "$(cat "$d/worker-names-started.txt")" -gt 0
  model_check $c "Started project session" "$wsid"

  # The index finds this turn by a word only its asked text has.
  sqlite3 -readonly -separator $'\t' "$ROOT/index/index.db" "select source, giga, path, at, substr(text, 1, 120) from entries where entries match $(sqlq "\"$tag\"")" >"$d/index-match.tsv" 2>&1
  check $c "index: FTS MATCH '$tag' (from asked) finds this turn under giga $slug" "$d/index-match.tsv" \
    bash -c 'awk -F"\t" -v g="$2" "\$2 == g" "$1" | grep -q .' _ "$d/index-match.tsv" "$slug"

  measure >"$d/size-after.txt"
  local turns=$(( $(receipt_count "$slug") - giga_turns_before ))
  record $c "workspace growth over $turns Giga turn(s)" INFO "before: $(cat "$d/size-before.txt") | after: $(cat "$d/size-after.txt")"
}

case_5() {
  local c=5 d="$RUN_DIR/case5"
  mkdir -p "$d"
  ensure_monos
  ensure_mono "$GIGA2" 1
  [[ -d "$ROOT/.git" ]] || status_as "$GIGA" "$d/status-setup.json"
  local s1 s2; s1="$(giga_slug "$GIGA")"; s2="$(giga_slug "$GIGA2")"
  local r1 r2 c1 c2
  r1="$(receipt_count "$s1")"; r2="$(receipt_count "$s2")"
  c1="$(git -C "$ROOT" rev-list --count --author="giga+$s1@" HEAD)"; c2="$(git -C "$ROOT" rev-list --count --author="giga+$s2@" HEAD)"
  local t0 t1; t0="$(date +%s)"
  ui send "$GIGA" "Run exactly this shell command and nothing else: sleep 40. Then reply done."
  t1="$(date +%s)"
  # The second sleep ends with the first: subtract the time the first send took.
  ui send "$GIGA2" "Run exactly this shell command and nothing else: sleep $(( 40 - (t1 - t0) )). Then reply done."
  ui wait-idle "$GIGA" 300 >/dev/null; ui wait-idle "$GIGA2" 300 >/dev/null
  sleep 5
  git -C "$ROOT" log --format='%H %at %an %s' -n 6 >"$d/workspace-log.txt"
  check $c "$s1 got one more receipt" "$r1 -> $(receipt_count "$s1")" test "$(receipt_count "$s1")" = "$((r1 + 1))"
  check $c "$s2 got one more receipt" "$r2 -> $(receipt_count "$s2")" test "$(receipt_count "$s2")" = "$((r2 + 1))"
  check $c "$s1 commit landed" "$d/workspace-log.txt" test "$(git -C "$ROOT" rev-list --count --author="giga+$s1@" HEAD)" = "$((c1 + 1))"
  check $c "$s2 commit landed" "$d/workspace-log.txt" test "$(git -C "$ROOT" rev-list --count --author="giga+$s2@" HEAD)" = "$((c2 + 1))"
  check $c "no .git/index.lock left" "$ROOT/.git/index.lock" test ! -e "$ROOT/.git/index.lock"
  # The app empties the lock file on release rather than deleting it; a lock left
  # behind is one that still names an owner or is still open.
  local lockf="$ROOT/.git/monocode-giga.lock"
  { ls -l "$lockf" 2>&1; echo "content: [$(cat "$lockf" 2>/dev/null)]"; echo "open by: $(lsof -t "$lockf" 2>/dev/null | paste -sd, -)"; } >"$d/giga-lock.txt"
  check $c "no .git/monocode-giga.lock held (absent or empty, open by no process)" "$d/giga-lock.txt" \
    bash -c '[[ ! -s "$1" ]] && [[ -z "$(lsof -t "$1" 2>/dev/null)" ]]' _ "$lockf"
  git -C "$ROOT" fsck --no-dangling >"$d/fsck.txt" 2>&1
  check $c "git fsck clean" "$d/fsck.txt" test ! -s "$d/fsck.txt"
  check $c "working tree clean under both subtrees" "git status" \
    test -z "$(git -C "$ROOT" status --porcelain -- "gigas/$s1" "gigas/$s2")"
  record $c "commit times (s since epoch)" INFO "$(awk '{print $2}' "$d/workspace-log.txt" | head -2 | paste -sd' ' -)"
}

case_6() {
  local c=6 d="$RUN_DIR/case6" tag; tag="$(nonce)"
  mkdir -p "$d"
  ensure_monos
  [[ -d "$ROOT/.git" ]] || status_as "$GIGA" "$d/status-setup.json"
  local slug; slug="$(giga_slug "$GIGA")"
  local intr="gigas/$slug/briefs/intrusion-$tag.md"

  # A regular Mono's session commits into a Giga's subtree.
  ui send "$PROBE" "Run this in a shell and reply with its exit code and stderr verbatim: cd \"$ROOT\" && echo intrusion > \"$intr\" && git add \"$intr\" && git commit -m \"intrusion $tag\""
  ui wait-idle "$PROBE" 300 >/dev/null
  git -C "$ROOT" log --all --format='%H %s' >"$d/log-after-intrusion.txt"
  check $c "regular Mono's commit into gigas/$slug/ did not land" "$d/log-after-intrusion.txt" bash -c '! grep -qF "intrusion $1" "$2"' _ "$tag" "$d/log-after-intrusion.txt"
  # The same commit from a plain shell shows the hook's message.
  [[ -f "$ROOT/$intr" ]] || echo intrusion >"$ROOT/$intr"
  git -C "$ROOT" add "$intr"
  (cd "$ROOT" && env -u MONOCODE_GIGA_SLUG git commit -m "intrusion $tag direct") >"$d/hook-intrusion.out" 2>&1; local rc=$?
  check $c "hook rejects it: exit 1 and 'monocode pre-commit: $intr: <rule>'" "$d/hook-intrusion.out (rc=$rc)" \
    bash -c '[[ "$1" == 1 ]] && grep -qE "^monocode pre-commit: $2: .+" "$3"' _ "$rc" "$intr" "$d/hook-intrusion.out"
  git -C "$ROOT" reset -q -- "$intr"; rm -f "$ROOT/$intr"

  # Secret-looking files inside the Giga's subtree.
  local tok="gigas/$slug/notes/api.token" leak="gigas/$slug/briefs/leak-$tag.md"
  mkdir -p "$ROOT/gigas/$slug/notes"
  echo "token $tag" >"$ROOT/$tok"
  printf 'deploy key: ghp_%s\n' "$(LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c 36)" >"$ROOT/$leak"
  local gc0; gc0="$(git -C "$ROOT" rev-list --count --author="giga+$slug@" HEAD)"
  status_as "$GIGA" "$d/status.json"   # a Giga turn commits gigas/$slug/
  sleep 3
  git -C "$ROOT" log --all --name-only --format='commit %H %s' >"$d/log-names.txt"
  check $c "$tok never committed" "$d/log-names.txt" bash -c '! grep -qxF "$1" "$2"' _ "$tok" "$d/log-names.txt"
  check $c "$leak (ghp_ content) never committed" "$d/log-names.txt" bash -c '! grep -qxF "$1" "$2"' _ "$leak" "$d/log-names.txt"
  local gc1; gc1="$(git -C "$ROOT" rev-list --count --author="giga+$slug@" HEAD)"
  check $c "the Giga turn still committed its subtree (giga commits $gc0 -> $gc1)" "$d/log-names.txt" test "$gc1" -gt "$gc0"
  check $c "secret files left on disk, untracked" "$ROOT/$tok $ROOT/$leak" test -f "$ROOT/$tok" -a -f "$ROOT/$leak"
  git -C "$ROOT" add -f "$leak"
  (cd "$ROOT" && MONOCODE_GIGA_SLUG="$slug" git commit -m "leak $tag") >"$d/hook-secret.out" 2>&1; rc=$?
  check $c "hook rejects a staged ghp_ file even as the Giga" "$d/hook-secret.out (rc=$rc)" \
    bash -c '[[ "$1" == 1 ]] && grep -qE "^monocode pre-commit: $2: .+" "$3"' _ "$rc" "$leak" "$d/hook-secret.out"
  git -C "$ROOT" reset -q -- "$leak"; rm -f "$ROOT/$tok" "$ROOT/$leak"
}

case_7() {
  local c=7 d="$RUN_DIR/case7"
  mkdir -p "$d"
  "$EVAL_DIR/instance.sh" stop
  "$EVAL_DIR/instance.sh" start --fresh-home
  record $c "after app start alone" INFO "$( [[ -e "$ROOT" ]] && echo "$ROOT exists" || echo "$ROOT absent")"
  ui create-mono "$GIGA" || { record $c "create Mono" FAIL "ui.sh create-mono"; return; }
  ui set-model "$GIGA" "$MODEL_TAB" "$MODEL_ROW" "$MODEL_LABEL" || { record $c "Mono on $MODEL_ID" FAIL "ui.sh set-model"; return; }
  ui giga-on "$GIGA" || { record $c "Giga on by the UI" FAIL "ui.sh giga-on"; return; }
  ui shot "$d/giga-on.png" >/dev/null
  check $c "workspace appears after Giga on, with no setup step" "$ROOT" wait_path "$ROOT/.git" 30 -d
  tree_check $c fresh "$(slugify "$GIGA")"
  check $c "gigas/ holds exactly the new Giga's slug" "$(ls "$ROOT/gigas" 2>/dev/null | paste -sd, -)" \
    test "$(ls "$ROOT/gigas" 2>/dev/null)" = "$(slugify "$GIGA")"
}

for c in "${CASES[@]}"; do
  log "case $c"
  "case_$c" 2>&1 | tee "$RUN_DIR/case$c.log"
done
if [[ "$TARGET" == instance ]]; then
  "$EVAL_DIR/instance.sh" lsof-check >"$RUN_DIR/lsof-check.txt" 2>&1
  record iso "lsof: no open file under the real app data or real ~/MonoCode" "$(grep -q '^PASS isolation' "$RUN_DIR/lsof-check.txt" && echo PASS || echo FAIL)" "$RUN_DIR/lsof-check.txt"
fi
"$EVAL_DIR/report.sh" "$RUN_ID"
