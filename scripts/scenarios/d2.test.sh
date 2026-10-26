# shellcheck shell=bash
# D2 (ADE-42) through the real app: database encryption off, then on with the
# GigaEval build's key file (cargo feature test-db-key), then a wrong key, no
# key, and a plaintext database with encryption on, each of which must stop the
# app with the database untouched. Sourced by `app.sh d2`, which holds the
# shared helpers.

D2_ON="$APP_DATA/db-encryption-on"
D2_KEY="$APP_DATA/test-db-key"

d2_kind() {
  if [[ ! -s "$DB" ]]; then echo empty
  elif [[ "$(head -c 15 "$DB")" == "SQLite format 3" ]]; then echo plaintext
  else echo encrypted; fi
}
# Size and SHA-256 of monocode.db, -wal and -shm, or "absent" for each.
d2_files() {
  local file
  for file in "$DB" "$DB-wal" "$DB-shm"; do
    if [[ -e "$file" ]]; then
      printf '%s:%s:%s ' "${file##*/}" "$(wc -c <"$file" | tr -d ' ')" "$(shasum -a 256 "$file" | cut -d' ' -f1)"
    else
      printf '%s:absent ' "${file##*/}"
    fi
  done
}
d2_key() { if [[ -e "$D2_KEY" ]]; then echo made; else echo none; fi; }
d2_up() { sleep 2; if [[ -n "$(target_pid instance)" ]]; then echo running; else echo stopped; fi; }

# d2_refused <case> <text the log must hold>: a start that stops on the
# database error and leaves monocode.db, -wal and -shm as they were.
d2_refused() {
  local before; before="$(d2_files)"
  "$INSTANCE" start || true
  expect "$1: the app stops" "$(d2_up)" stopped
  expect "$1: the log names the cause" "$(grep -q -- "$2" "$EVAL_OUT/instance.log" && echo yes || echo no)" yes
  expect "$1: monocode.db, -wal and -shm are unchanged" "$(d2_files)" "$before"
  "$INSTANCE" stop
}

run_d2() {
  FAILED=0
  log "D2 off"
  fresh_home
  "$INSTANCE" start
  wait_until 30 "the first launch writes mono_clock" holds "SELECT count(*) = 1 FROM mono_clock"
  "$INSTANCE" stop
  expect "off: the database" "$(d2_kind)" plaintext
  expect "off: sqlite3 reads it" "$(sqlite3 "file:$DB?mode=ro" "SELECT count(*) FROM mono_clock")" 1
  expect "off: key" "$(d2_key)" none

  log "D2 on"
  fresh_home
  : >"$D2_ON"
  "$INSTANCE" start
  expect "on: first launch" "$(d2_up)" running
  "$INSTANCE" stop
  expect "on: key" "$(grep -cE '^[0-9a-f]{64}$' "$D2_KEY")" 1
  expect "on: the database" "$(d2_kind)" encrypted
  expect "on: sqlite3 cannot read it" "$(sqlite3 "file:$DB?mode=ro" "SELECT count(*) FROM sqlite_master" 2>&1 | grep -c 'file is not a database')" 1
  "$INSTANCE" start
  expect "on: the app opens it again with the key" "$(d2_up)" running
  "$INSTANCE" stop
  # instance.sh stop sends SIGTERM: the app does not close the database, so
  # its last commit stays in -wal, the state a wrong key must not touch.
  expect "on: SIGTERM leaves a -wal" "$([[ -s "$DB-wal" ]] && echo yes || echo no)" yes
  printf '%064x' 1 >"$D2_KEY"
  d2_refused "wrong key" "does not open"
  rm "$D2_KEY"
  d2_refused "missing key" "is encrypted, but its key is not in"
  expect "missing key: key" "$(d2_key)" none

  log "D2 on over a plaintext database"
  fresh_home
  "$INSTANCE" start
  wait_until 30 "the first launch writes mono_clock" holds "SELECT count(*) = 1 FROM mono_clock"
  "$INSTANCE" stop
  : >"$D2_ON"
  d2_refused "on over plaintext" "is a plaintext database"
  expect "on over plaintext: the database" "$(d2_kind)" plaintext
  expect "on over plaintext: key" "$(d2_key)" none
  return "$FAILED"
}
