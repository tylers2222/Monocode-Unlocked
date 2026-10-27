# shellcheck shell=bash
# ADE-112 through the rig: writes in the temp HOME never reach the user's own
# files. The logins come from a seeded folder under /tmp (GIGA_EVAL_LOGIN_HOME),
# so the check is real on a runner and no write goes near this machine's HOME:
# every probe writes only after the temp HOME is proven to hold no link.
# Sourced by `app.sh home-isolation`, which holds the shared helpers.

HI_LOGIN_HOME="$EVAL_OUT/login-home"
HI_TEXT=(.omp/agent/config.yml .codex/auth.json .gitconfig)
HI_DB=.omp/agent/agent.db

# Path and SHA-256 of every file in the seeded login folder.
hi_sums() { (cd "$HI_LOGIN_HOME" && find . -type f -print0 | sort -z | xargs -0 shasum -a 256); }
# The seeded files made, changed or removed since <sums>, or "none".
hi_changed() {
  local changed
  changed="$({ diff <(printf '%s\n' "$1") <(hi_sums) || true; } | sed -n 's/^[<>] [0-9a-f]*  //p' | sort -u | paste -sd' ' -)"
  echo "${changed:-none}"
}

# omp's agent.db is WAL. Closed, it has no -wal and no -shm (macOS's sqlite3
# keeps both, so they go here). Any sqlite open of it, even read-only, can make
# both beside it, so the copy must not open it with sqlite.
hi_seed() {
  local rel
  rm -rf "$HI_LOGIN_HOME"
  for rel in "${HI_TEXT[@]}"; do
    mkdir -p "$HI_LOGIN_HOME/$(dirname "$rel")"
    echo "seeded $rel" >"$HI_LOGIN_HOME/$rel"
  done
  sqlite3 "$HI_LOGIN_HOME/$HI_DB" "PRAGMA journal_mode = WAL; CREATE TABLE auth_credentials (v TEXT);
    INSERT INTO auth_credentials VALUES ('seeded'); PRAGMA wal_checkpoint(TRUNCATE)" >/dev/null
  rm -f "$HI_LOGIN_HOME/$HI_DB-wal" "$HI_LOGIN_HOME/$HI_DB-shm"
}

# hi_refused <what> <name> <target> <instance.sh command>: with one link
# <name> -> <target> in the temp HOME, the command stops and names the path a
# write takes through that link.
hi_refused() {
  local out code=0
  ln -s "$3" "$EVAL_HOME/$2"
  out="$("$INSTANCE" "$4" 2>&1)" || code=$?
  rm "$EVAL_HOME/$2"
  expect "$1: exit code" "$code" 2
  expect "$1: the error names the link" "$(grep -F "$EVAL_HOME/$2" <<<"$out" | grep -cF ' resolves to ')" 1
}

run_home_isolation() {
  FAILED=0
  log "home isolation"
  "$INSTANCE" stop
  hi_seed
  local before rel db=("$HI_LOGIN_HOME/$HI_DB"*)
  before="$(hi_sums)"
  expect "files of the seeded agent.db (no -wal, no -shm)" "${#db[@]}" 1
  export GIGA_EVAL_LOGIN_HOME="$HI_LOGIN_HOME" GIGA_EVAL_LOGINS=1
  if ! "$INSTANCE" home --fresh; then
    expect "a fresh temp HOME with the logins" refused made
    return 1
  fi
  # A link could carry a write out of the temp HOME: none before any probe.
  expect "links in the temp HOME" "$(find -H "$EVAL_HOME" -type l | wc -l | tr -d ' ')" 0
  ((FAILED == 0)) || return 1
  for rel in "${HI_TEXT[@]}"; do
    expect "$rel is a copy" "$(cat "$EVAL_HOME/$rel")" "seeded $rel"
  done
  expect "omp's credentials are a copy" "$(sqlite3 "$EVAL_HOME/$HI_DB" 'SELECT v FROM auth_credentials')" seeded
  expect "the copies are private" "$(stat -f %Lp "$EVAL_HOME/$HI_DB" "${HI_TEXT[@]/#/$EVAL_HOME/}" | sort -u)" 600

  # A copy that shares storage with its source (a hard link, a link above the
  # file) passes the link count; these writes show it in the seeded folder.
  for rel in "${HI_TEXT[@]}"; do echo probe >>"$EVAL_HOME/$rel"; done
  sqlite3 "$EVAL_HOME/$HI_DB" "INSERT INTO auth_credentials VALUES ('probe')"
  expect "seeded files changed by a write to each copy" "$(hi_changed "$before")" none

  hi_refused "a start with a link into the login folder" .ssh-login "$HI_LOGIN_HOME/.ssh" start
  expect "the refused start launched nothing" "$(target_pid instance)" ""
  hi_refused "a dangling link into the login folder" .key "$HI_LOGIN_HOME/.ssh/none/key" home
  hi_refused "a link into the real HOME" .ssh-home "$REAL_HOME/.ssh" home
  hi_refused "a link to an ancestor of the real HOME" .up-home "$(dirname "$REAL_HOME")" home
  hi_refused "a dangling link that climbs out with .." .up "$EVAL_OUT/none/../../../..$REAL_HOME/.ssh" home
  mkdir -p "$EVAL_OUT/hop" && ln -sfn "$REAL_HOME/.ssh" "$EVAL_OUT/hop/ssh"
  hi_refused "a second hop through /tmp" .hop "$EVAL_OUT/hop" home
  rm -rf "$EVAL_OUT/hop"
  expect "seeded files changed at the end" "$(hi_changed "$before")" none
  unset GIGA_EVAL_LOGIN_HOME GIGA_EVAL_LOGINS
  return "$FAILED"
}
