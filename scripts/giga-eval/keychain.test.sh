#!/usr/bin/env bash
# keychain.sh (ADE-233): the `security` an instance runs is the stub, never the
# user's keychain. The temp HOME comes from `instance.sh home --fresh`, and
# `instance.sh start` launches a fake app (a script that resolves and calls
# `security` by name, as rate_limits.rs does). A decoy `security` stands
# before /usr/bin on every PATH here, so a PATH that misses the stub reaches
# the decoy, never the real one. Nothing here launches a real app or reads a
# keychain. Two runs never share a path.
# Run: bash scripts/giga-eval/keychain.test.sh
# shellcheck disable=SC2015 # pass and fail always return 0
TEST_DIR="$(mktemp -d /private/tmp/giga-eval-test.XXXXXX)"
trap 'rm -rf "$TEST_DIR"' EXIT
export GIGA_EVAL_OUT="$TEST_DIR/out" GIGA_EVAL_HOME="$TEST_DIR/home"
unset GIGA_EVAL_SANDBOX GIGA_EVAL_OPEN GIGA_EVAL_LOGINS
source "$(dirname "$0")/lib.sh"
source "$(dirname "$0")/keychain.sh"
# shellcheck disable=SC2329 # keychain.sh calls it
log() { :; }
INSTANCE="$EVAL_DIR/instance.sh"
failed=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1"; failed=1; }
# expect <ok|fails> <what> <command...>
expect() {
  local want="$1" what="$2" got=fails; shift 2
  "$@" >/dev/null 2>&1 && got=ok
  [[ "$got" == "$want" ]] && pass "$want: $what" || fail "$got, expected $want: $what"
}

SYS="$TEST_DIR/sys" DECOY_LOG="$TEST_DIR/decoy.log"
mkdir -p "$SYS"
printf '#!/bin/sh\necho "$*" >>%s\nexit 0\n' "$DECOY_LOG" >"$SYS/security"
chmod +x "$SYS/security"
BASE="$SYS:/usr/bin:/bin:/usr/sbin:/sbin"
FAKE=(find-generic-password -s giga-eval-test-fake-service)

expect ok "instance.sh home --fresh" env PATH="$BASE" bash "$INSTANCE" home --fresh
expect ok "home writes an executable stub" test -x "$KEYCHAIN_STUB"
P="$(instance_path "$BASE")"
got="$(PATH="$P"; command -v security)" || got=""
if [[ "$got" == "$KEYCHAIN_STUB" ]]; then
  pass "security on the instance PATH is the stub"
  code=0; env -i PATH="$P" security "${FAKE[@]}" >/dev/null 2>&1 || code=$?
  [[ "$code" == 44 ]] && pass "a call exits 44" || fail "a call exits $code, expected 44"
  grep -qF " ${FAKE[*]}" "$KEYCHAIN_LOG" 2>/dev/null && pass "the call is in $KEYCHAIN_LOG" || fail "the call is not in $KEYCHAIN_LOG"
else
  fail "security on the instance PATH is ${got:-nothing}, not the stub (no call made)"
fi
expect ok "keychain_stub_ok on the instance PATH" keychain_stub_ok "$P"
expect fails "keychain_stub_ok with the stub's dir after the decoy" keychain_stub_ok "$BASE:$(dirname "$KEYCHAIN_STUB")"
expect fails "keychain_stub_ok without the stub's dir" keychain_stub_ok "$BASE"
echo 'exit 0' >>"$KEYCHAIN_STUB" 2>/dev/null || true
expect fails "keychain_stub_ok on a changed stub" keychain_stub_ok "$P"
expect ok "instance.sh home writes it again" env PATH="$BASE" bash "$INSTANCE" home
expect ok "keychain_stub_ok after home" keychain_stub_ok "$P"

# The launch: a fake app that records what `security` is on its PATH, calls
# it, and exits. `start` then stops on "instance exited"; any earlier stop is
# its keychain check refusing the PATH.
if [[ "$EVAL_OS" == macos ]]; then APP="$EVAL_OUT/apps/T/T.app"; else APP="$EVAL_OUT/apps/T/monocode"; fi
BIN="$(app_bin "$APP")"
mkdir -p "$(dirname "$BIN")"
printf '#!/bin/sh\ncommand -v security >%s/app-security\nsecurity %s >/dev/null 2>&1\necho $? >>%s/app-security\n' \
  "$TEST_DIR" "${FAKE[*]}" "$TEST_DIR" >"$BIN"
chmod +x "$BIN"
echo "$APP" >"$EVAL_OUT/instance.app"
out="$(env PATH="$BASE" DISPLAY="${DISPLAY:-:0}" bash "$INSTANCE" start 2>&1)" || true
[[ "$out" == *"instance exited"* ]] && pass "start passes the keychain check" || fail "start stopped before the launch: $out"
for _ in $(seq 1 20); do [[ "$(wc -l <"$TEST_DIR/app-security" 2>/dev/null)" -ge 2 ]] && break; sleep 0.1; done
app_saw="$(paste -sd' ' - <"$TEST_DIR/app-security" 2>/dev/null)"
[[ "$app_saw" == "$KEYCHAIN_STUB 44" ]] && pass "the launched app runs the stub (exit 44)" || fail "the launched app saw: ${app_saw:-nothing}"
[[ "$(grep -cF " ${FAKE[*]}" "$KEYCHAIN_LOG")" == 2 ]] && pass "the app's call is in $KEYCHAIN_LOG" || fail "the app's call is not in $KEYCHAIN_LOG"

# A launch that does not take our env (`open`) is checked on the process.
# proc_with <PATH>: node (macOS shows no env of an Apple-signed binary such as
# sleep; the ad-hoc signed app shows it) with that PATH, once it runs as node.
NODE="$(command -v node)"
proc_with() {
  env PATH="$1" "$NODE" -e 'setTimeout(() => {}, 30000)' &
  PROC=$!
  for _ in $(seq 1 50); do [[ "$(ps -o comm= -p "$PROC")" == *node ]] && return; sleep 0.05; done
}
proc_with "$P"
expect ok "keychain_proc_ok on a process with the instance PATH" keychain_proc_ok "$PROC"
kill "$PROC"; wait "$PROC" 2>/dev/null || true
proc_with "$BASE:$(dirname "$KEYCHAIN_STUB")"
expect fails "keychain_proc_ok on a process with the stub's dir last" keychain_proc_ok "$PROC"
kill "$PROC"; wait "$PROC" 2>/dev/null || true

[[ ! -e "$DECOY_LOG" ]] && pass "the decoy never ran" || fail "the decoy ran: $(paste -sd' ' - <"$DECOY_LOG")"
exit "$failed"
