# shellcheck shell=bash
# ADE-112 through the rig: writes in the temp HOME never reach the user's own
# files. The logins come from a seeded folder under /tmp (GIGA_EVAL_LOGIN_HOME),
# so the check is real on a runner and no write goes near this machine's HOME.
# Sourced by `app.sh home-isolation`, which holds the shared helpers.

HI_LOGIN_HOME="$EVAL_OUT/login-home"
# The paths a d1 probe and a d4 Claude run wrote through the old links.
HI_PROBES=(.config/git/config .claude/settings.json .ssh/x)

# Path and SHA-256 of every file in the seeded login folder.
hi_sums() { (cd "$HI_LOGIN_HOME" && find . -type f -print0 | sort -z | xargs -0 shasum -a 256); }
# The seeded files made, changed or removed since <sums>, or "none".
hi_changed() {
  local changed
  changed="$({ diff <(printf '%s\n' "$1") <(hi_sums) || true; } | sed -n 's/^[<>] [0-9a-f]*  //p' | sort -u | paste -sd' ' -)"
  echo "${changed:-none}"
}

hi_seed() {
  local rel
  rm -rf "$HI_LOGIN_HOME"
  for rel in "${HI_PROBES[@]}" .omp/agent/config.yml .codex/auth.json .gitconfig; do
    mkdir -p "$HI_LOGIN_HOME/$(dirname "$rel")"
    echo "seeded $rel" >"$HI_LOGIN_HOME/$rel"
  done
  sqlite3 "$HI_LOGIN_HOME/.omp/agent/agent.db" "CREATE TABLE auth_credentials (v TEXT); INSERT INTO auth_credentials VALUES ('seeded')"
}

# hi_refused <what> <name> <target> <instance.sh command>: with one link
# <name> -> <target> in the temp HOME, the command stops and names the link.
hi_refused() {
  local out code=0
  ln -s "$3" "$EVAL_HOME/$2"
  out="$("$INSTANCE" "$4" 2>&1)" || code=$?
  rm "$EVAL_HOME/$2"
  expect "$1: exit code" "$code" 2
  expect "$1: the error names the link" "$(grep -cF "$EVAL_HOME/$2 resolves into" <<<"$out")" 1
}

run_home_isolation() {
  FAILED=0
  log "home isolation"
  "$INSTANCE" stop
  hi_seed
  local before rel
  before="$(hi_sums)"
  export GIGA_EVAL_LOGIN_HOME="$HI_LOGIN_HOME" GIGA_EVAL_LOGINS=1
  if ! "$INSTANCE" home --fresh; then
    expect "a fresh temp HOME with the logins" refused made
    return 1
  fi
  for rel in "${HI_PROBES[@]}"; do
    mkdir -p "$EVAL_HOME/$(dirname "$rel")"
    echo probe >>"$EVAL_HOME/$rel"
  done
  expect "seeded files changed by a write to ${HI_PROBES[*]}" "$(hi_changed "$before")" none
  expect "links in the temp HOME" "$(find "$EVAL_HOME" -type l | wc -l | tr -d ' ')" 0
  for rel in .omp/agent/config.yml .codex/auth.json .gitconfig; do
    expect "$rel is a copy" "$(cat "$EVAL_HOME/$rel")" "seeded $rel"
  done
  expect "omp's credentials are a copy" \
    "$(sqlite3 "file:$EVAL_HOME/.omp/agent/agent.db?mode=ro" 'SELECT v FROM auth_credentials')" seeded

  hi_refused "a start with a link into the login folder" .ssh-login "$HI_LOGIN_HOME/.ssh" start
  expect "the refused start launched nothing" "$(target_pid instance)" ""
  hi_refused "a dangling link into the login folder" .key "$HI_LOGIN_HOME/.ssh/none/key" home
  hi_refused "a link into the real HOME" .ssh-home "$REAL_HOME/.ssh" home
  expect "seeded files changed at the end" "$(hi_changed "$before")" none
  unset GIGA_EVAL_LOGIN_HOME GIGA_EVAL_LOGINS
  return "$FAILED"
}
