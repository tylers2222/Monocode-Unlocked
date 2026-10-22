#!/usr/bin/env bash
# sandbox.sh: the watcher's rule on the lines from review round 2 of #163
# (r4328192833), and each protection of the start failing when it is
# removed (review rounds 1 to 3 of #243). macOS only (sandbox-exec). Nothing
# here links or reads a real login, looks up a name, reaches a remote host or
# connects to a real unix socket. Two runs never share a path.
# Run: bash scripts/giga-eval/sandbox.test.sh
TEST_DIR="$(mktemp -d /private/tmp/giga-eval-test.XXXXXX)"
export GIGA_EVAL_OUT="$TEST_DIR/out" GIGA_EVAL_HOME="$TEST_DIR/home"
# A canary under the real name: a launch without `env -i` would pass it on.
export MONOCODE_APP_TOKEN=giga-eval-test-canary
source "$(dirname "$0")/lib.sh"
source "$(dirname "$0")/sandbox.sh"
log() { :; }

APP="$EVAL_OUT/apps/T.app/Contents/MacOS/monocode"
NODE="$(realpath "$(command -v node)")"
FX="$PUPPET_BIN"
failed=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1"; failed=1; }

check() {
  local want="$1" exe="$2" args="$3" got=deny
  sandbox_allowed "$exe" "$args" "$APP" && got=allow
  [[ "$got" == "$want" ]] && pass "$got: $args" || fail "$got, expected $want: $args"
}
check allow "$APP" "$APP app-mcp"
check deny "$HOME/.local/bin/claude" "claude -p --mcp-config {\"mcpServers\":{\"monocode\":{\"command\":\"$APP\",\"args\":[\"app-mcp\"]}}}"
check deny /opt/homebrew/Cellar/gh/2.74.1/bin/gh "/opt/homebrew/bin/gh api /user"
check deny "$NODE" "node /tmp/evil.js $FX x"
check allow "$NODE" "node $FX acp"
check allow /usr/bin/env "/usr/bin/env node $FX models --json"
check deny /usr/bin/env "/usr/bin/env gh api /user"
check allow /usr/bin/git "git -C /tmp/x status"
check allow "$NODE" "(node)"
check deny /opt/homebrew/Cellar/gh/2.74.1/bin/gh "(gh)"

mkdir -p "$(dirname "$APP")" "$EVAL_HOME/.local/bin"
cp /usr/bin/true "$APP"
printf '#!/usr/bin/env node\nconsole.log("fx acp (stub)")\n' >"$FX"
chmod +x "$FX"
touch "$SANDBOX_HOME_MARK"
JAIL="$(mktemp -d /private/tmp/giga-eval-jail.XXXXXX)"
PROFILE="$EVAL_OUT/instance.sb"
ENVS="$(sandbox_env "$JAIL")"
sandbox_profile "$APP" "$JAIL" "$PROFILE"

# expect <ok|fails> <what> <command...>
expect() {
  local want="$1" what="$2" got=fails; shift 2
  "$@" >/dev/null 2>&1 && got=ok
  [[ "$got" == "$want" ]] && pass "$want: $what" || fail "$got, expected $want: $what"
}
start_ok() { sandbox_env_ok "$2" && sandbox_launch "$1" "$2" && sandbox_preflight "$APP" "$JAIL"; }

expect ok "the full profile and env pass the preflight" start_ok "$PROFILE" "$ENVS"
while IFS= read -r rule; do
  grep -vxF "$rule" "$PROFILE" >"$EVAL_OUT/cut.sb"
  expect fails "without $rule" start_ok "$EVAL_OUT/cut.sb" "$ENVS"
done < <(grep -E '^\(deny ' "$PROFILE")
sed "s|(subpath \"/dev\"))|(subpath \"/dev\") (subpath \"$(realpath "$(getconf DARWIN_USER_TEMP_DIR)")\"))|" "$PROFILE" >"$EVAL_OUT/wide.sb"
expect fails "writes allowed in the per-user temp dir" start_ok "$EVAL_OUT/wide.sb" "$ENVS"
sed -E 's|^\(allow network-outbound \(remote unix-socket .*|(allow network-outbound (remote unix-socket))|' "$PROFILE" >"$EVAL_OUT/sock.sb"
expect fails "unix sockets allowed everywhere" start_ok "$EVAL_OUT/sock.sb" "$ENVS"
jail_sees() { sandbox_launch "$1" "$2" && sandbox_preflight "${3:-$APP}" "$JAIL"; }
# fails_on <probe> <what> <command...>: the start fails, and the preflight
# names <probe> among the results it wanted and did not get.
fails_on() {
  local probe="$1" what="$2" msg; shift 2
  if msg="$(log() { echo "$*"; }; "$@" 2>&1)"; then fail "ok, expected fails on $probe: $what"; return; fi
  msg="${msg#*wanted }"
  [[ " ${msg%%; got*} " == *" $probe "* ]] && pass "fails on $probe: $what" || fail "fails, but not on $probe: $what"
}
# without <profile> <app> <filter>: start with one filter cut from the
# profile's deny file-write* rule; a cut that changes nothing fails.
without() {
  local l; while IFS= read -r l; do [[ "$l" == "(deny file-write* "* ]] && l="${l/" $3"/}"; printf '%s\n' "$l"; done <"$1" >"$EVAL_OUT/cut.sb"
  ! cmp -s "$1" "$EVAL_OUT/cut.sb" && sandbox_launch "$EVAL_OUT/cut.sb" "$ENVS" && sandbox_preflight "$2" "$JAIL"
}
H="$(realpath "$EVAL_HOME")"
fails_on puppet=EPERM 'only (subpath "<home>/.local/bin") removed' without "$PROFILE" "$APP" "(subpath \"$H/.local/bin\")"
fails_on rename_local=EPERM 'only (literal "<home>/.local") removed' without "$PROFILE" "$APP" "(literal \"$H/.local\")"
fails_on rename_home=EPERM 'only (literal "<home>") removed' without "$PROFILE" "$APP" "(literal \"$H\")"
# The app's rule matters only where writes are allowed: an app under HOME.
HOME_APP="$EVAL_HOME/A.app/Contents/MacOS/monocode"
mkdir -p "$(dirname "$HOME_APP")" && cp /usr/bin/true "$HOME_APP"
sandbox_profile "$HOME_APP" "$JAIL" "$EVAL_OUT/home-app.sb"
expect ok "an app under HOME with the full profile" jail_sees "$EVAL_OUT/home-app.sb" "$ENVS" "$HOME_APP"
fails_on app=EPERM 'only (subpath "<app bundle>") removed' without "$EVAL_OUT/home-app.sb" "$HOME_APP" "(subpath \"$H/A.app\")"
rm -rf "$EVAL_HOME/A.app"
expect fails "MONOCODE_APP_TOKEN in the env" start_ok "$PROFILE" "$ENVS"$'\n'"MONOCODE_APP_TOKEN=x"
expect fails "MONOCODE_APP_TOKEN reaches the jail" jail_sees "$PROFILE" "$ENVS"$'\n'"MONOCODE_APP_TOKEN=x"
eval "$(declare -f sandbox_launch | sed 's/env -i /env /')"
expect fails "a launch without env -i" start_ok "$PROFILE" "$ENVS"
source "$(dirname "$0")/sandbox.sh"
log() { :; }
expect ok "the shared launch again" start_ok "$PROFILE" "$ENVS"

expect ok "a fresh home" sandbox_home_ok
ln -s /usr/share "$EVAL_HOME/.config"
expect fails "a home that links outside /tmp" sandbox_home_ok
rm "$EVAL_HOME/.config" "$SANDBOX_HOME_MARK"
expect fails "a home not made fresh in sandbox mode" sandbox_home_ok

DB="$EVAL_OUT/guard.db"
sqlite3 "$DB" "PRAGMA journal_mode = WAL; CREATE TABLE sessions (id, harness); CREATE TABLE automations (enabled);
  CREATE TABLE mono_messages (delivered_at, status); INSERT INTO sessions VALUES ('s', 'fx');" >/dev/null
expect ok "a scrubbed copy" sandbox_guard "$DB"
sqlite3 "$DB" "UPDATE sessions SET harness = 'claude'"
expect fails "a session not on the puppet" sandbox_guard "$DB"
sqlite3 "$DB" "UPDATE sessions SET harness = 'fx'"
cp "$DB" "$DB-wal"
expect fails "a copy with a -wal" sandbox_guard "$DB"
rm "$DB-wal"
sqlite3 "$DB" "INSERT INTO automations VALUES (1)"
expect fails "an enabled automation" sandbox_guard "$DB"
sqlite3 "$DB" "DELETE FROM automations"
mkdir -p "$EVAL_HOME/.fx/bin"
cp /usr/bin/true "$EVAL_HOME/.fx/bin/fx"
mv "$FX" "$FX.off"
expect fails "fx resolving to another binary" sandbox_guard "$DB"
rm "$EVAL_HOME/.fx/bin/fx"
expect fails "fx not resolving to the puppet" sandbox_guard "$DB"

rm -rf "$JAIL" "$TEST_DIR"
exit "$failed"
