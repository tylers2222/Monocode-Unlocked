#!/usr/bin/env bash
# sandbox.sh: the watcher's rule on the lines from review round 2 of #163
# (r4328192833), and each protection of the start failing when it is
# removed (review round 1 of #243). macOS only (sandbox-exec). Nothing here
# links or reads a real login; the network probes reach no remote host.
# Run: bash scripts/giga-eval/sandbox.test.sh
export GIGA_EVAL_OUT=/tmp/sandbox-test GIGA_EVAL_HOME=/tmp/sandbox-test-home
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

rm -rf "$EVAL_OUT" "$EVAL_HOME"
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
start_ok() { sandbox_env_ok "$2" && sandbox_preflight "$1" "$2" "$APP" "$JAIL"; }

expect ok "the full profile and env pass the preflight" start_ok "$PROFILE" "$ENVS"
while IFS= read -r rule; do
  grep -vxF "$rule" "$PROFILE" >"$EVAL_OUT/cut.sb"
  expect fails "without $rule" start_ok "$EVAL_OUT/cut.sb" "$ENVS"
done < <(grep -E '^\(deny ' "$PROFILE")
sed "s|(subpath \"/dev\"))|(subpath \"/dev\") (subpath \"$(realpath "$(getconf DARWIN_USER_TEMP_DIR)")\"))|" "$PROFILE" >"$EVAL_OUT/wide.sb"
expect fails "writes allowed in the per-user temp dir" start_ok "$EVAL_OUT/wide.sb" "$ENVS"
expect fails "MONOCODE_APP_TOKEN in the env" start_ok "$PROFILE" "$ENVS"$'\n'"MONOCODE_APP_TOKEN=x"
expect fails "MONOCODE_APP_TOKEN reaches the jail" sandbox_preflight "$PROFILE" "$ENVS"$'\n'"MONOCODE_APP_TOKEN=x" "$APP" "$JAIL"

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
mv "$FX" "$FX.off"
expect fails "fx not resolving to the puppet" sandbox_guard "$DB"

rm -rf "$JAIL" "$EVAL_OUT" "$EVAL_HOME"
exit "$failed"
