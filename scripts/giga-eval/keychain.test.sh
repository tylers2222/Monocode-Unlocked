#!/usr/bin/env bash
# keychain.sh (ADE-233): every `security` an instance runs by name is the stub,
# never the user's keychain: the app's own, and its children's through the
# login shell (review r1 of #517). The temp HOME comes from `instance.sh home
# --fresh`; `instance.sh start` launches a fake app on the direct route and,
# on macOS, a fake com.example bundle through `open`. Each fake app runs
# probe.sh, which resolves `security` by name as rate_limits.rs does and in a
# `$SHELL -lic` child as harness.rs does, and calls it only when it is the
# stub. A decoy `security` stands before /usr/bin on every PATH given here.
# Nothing here launches a real app or reads a keychain. Two runs never share
# a path.
# Run: bash scripts/giga-eval/keychain.test.sh
# shellcheck disable=SC2015,SC2016,SC2329 # pass and fail return 0; literal $PATH lines; overrides of instance.sh functions
TEST_DIR="$(mktemp -d /private/tmp/giga-eval-test.XXXXXX)"
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
trap '[[ ! -d "$TEST_DIR/Probe.app" ]] || "$LSREGISTER" -u "$TEST_DIR/Probe.app" 2>/dev/null; rm -rf "$TEST_DIR"' EXIT
export GIGA_EVAL_OUT="$TEST_DIR/out" GIGA_EVAL_HOME="$TEST_DIR/home"
unset GIGA_EVAL_SANDBOX GIGA_EVAL_OPEN GIGA_EVAL_LOGINS
source "$(dirname "$0")/lib.sh"
source "$(dirname "$0")/keychain.sh"
log() { :; }
INSTANCE="$EVAL_DIR/instance.sh"
NODE="$(command -v node)"
KEEP='setTimeout(() => {}, 30000)'
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
if [[ "$EVAL_OS" == macos ]]; then FALLBACK_SHELL=/bin/zsh; else FALLBACK_SHELL=/bin/bash; fi

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

# The login shell (path_helper puts /usr/bin first on macOS).
for sh in /bin/zsh /bin/bash; do
  [[ -x "$sh" ]] || continue
  expect ok "keychain_login_ok in a $sh login shell" keychain_login_ok env -i HOME="$EVAL_HOME" SHELL="$sh" PATH="$P"
done
echo 'export PATH=/usr/bin:$PATH' >>"$EVAL_HOME/.zshrc"
echo 'export PATH=/usr/bin:$PATH' >>"$EVAL_HOME/.bash_profile"
expect fails "keychain_login_ok with a dotfile line after the stub's" keychain_login_ok env -i HOME="$EVAL_HOME" SHELL="$FALLBACK_SHELL" PATH="$P"
expect ok "instance.sh home moves the stub's line last again" env PATH="$BASE" bash "$INSTANCE" home
expect ok "keychain_login_ok after home" keychain_login_ok env -i HOME="$EVAL_HOME" SHELL="$FALLBACK_SHELL" PATH="$P"

# probe.sh <out dir>: what a fake app records, in its own env.
cat >"$TEST_DIR/probe.sh" <<PROBE
#!/bin/sh
out="\$1" stub='$KEYCHAIN_STUB' sh="\${SHELL:-$FALLBACK_SHELL}"
mkdir -p "\$out"
p="\$(command -v security)"; echo "\$p" >"\$out/app"
[ "\$p" != "\$stub" ] || { security ${FAKE[*]} >/dev/null 2>&1; echo \$? >>"\$out/app"; }
p="\$("\$sh" -lic 'command -v security' </dev/null 2>/dev/null | tail -n 1)"; echo "\$p" >"\$out/child"
[ "\$p" != "\$stub" ] || { "\$sh" -lic 'security x' </dev/null >/dev/null 2>&1; echo \$? >>"\$out/child"; }
echo done >"\$out/done"
PROBE
joined() { if [[ -f "$1" ]]; then paste -sd' ' - <"$1"; fi; }
log_lines() { if [[ -f "$KEYCHAIN_LOG" ]]; then wc -l <"$KEYCHAIN_LOG" | tr -d ' '; else echo 0; fi; }
# probed <route> <log lines before>: the probe of <route> ran the stub as the
# app and as a child, and both calls are in the log.
probed() {
  local d="$TEST_DIR/$1" _ app child
  for _ in $(seq 1 50); do [[ -f "$d/done" ]] && break; sleep 0.1; done
  app="$(joined "$d/app")" child="$(joined "$d/child")"
  [[ "$app" == "$KEYCHAIN_STUB 44" ]] && pass "$1: the app runs the stub (exit 44)" || fail "$1: the app saw ${app:-nothing}"
  [[ "$child" == "$KEYCHAIN_STUB 44" ]] && pass "$1: its login-shell child runs the stub (exit 44)" || fail "$1: its login-shell child saw ${child:-nothing}"
  [[ "$(log_lines)" -eq $(($2 + 2)) && "$(tail -n 1 "$KEYCHAIN_LOG" 2>/dev/null)" == *" x" ]] \
    && pass "$1: both calls are in $KEYCHAIN_LOG" || fail "$1: the calls are not in $KEYCHAIN_LOG"
}

# The direct route. `start` stops on "instance exited"; an earlier stop is a
# keychain check refusing the launch.
if [[ "$EVAL_OS" == macos ]]; then APP="$EVAL_OUT/apps/T/T.app"; else APP="$EVAL_OUT/apps/T/monocode"; fi
BIN="$(app_bin "$APP")"
mkdir -p "$(dirname "$BIN")"
printf '#!/bin/sh\nexec /bin/sh %s %s\n' "$TEST_DIR/probe.sh" "$TEST_DIR/direct" >"$BIN"
chmod +x "$BIN"
echo "$APP" >"$EVAL_OUT/instance.app"
# A caller's ZDOTDIR whose .zshrc puts /usr/bin first: the app's zsh must
# read the temp HOME's dotfiles instead.
mkdir -p "$TEST_DIR/zdot"
echo 'export PATH=/usr/bin:$PATH' >"$TEST_DIR/zdot/.zshrc"
before="$(log_lines)"
out="$(env PATH="$BASE" ZDOTDIR="$TEST_DIR/zdot" SHELL="$FALLBACK_SHELL" DISPLAY="${DISPLAY:-:0}" bash "$INSTANCE" start 2>&1)" || true
[[ "$out" == *"instance exited"* ]] && pass "direct: start passes the keychain checks" || fail "direct: start stopped before the launch: $out"
probed direct "$before"
printf '#!/bin/sh\necho /usr/bin/security\n' >"$TEST_DIR/badshell"
chmod +x "$TEST_DIR/badshell"
out="$(env PATH="$BASE" SHELL="$TEST_DIR/badshell" DISPLAY="${DISPLAY:-:0}" bash "$INSTANCE" start 2>&1)" || true
[[ "$out" == *"login shell is /usr/bin/security"*"the app does not start"* ]] \
  && pass "start refuses a login shell where security is /usr/bin/security" || fail "start did not refuse the login shell: $out"

# The check once the window shows, on a running app (node: macOS shows no env
# of an Apple-signed binary). instance.sh sourced, with has_window true once
# the app runs. A launch that loses the PATH must be stopped.
printf '#!%s\n%s\n' "$NODE" "$KEEP" >"$BIN"
started() {
  # shellcheck disable=SC1090
  ( PATH="$BASE"; source "$INSTANCE"
    has_window() { [[ "$(ps -o comm= -p "$1")" == *node ]]; }
    if [[ -n "${1:-}" ]]; then
      BAD="$1"
      launch() { exec nohup env "${APP_ENV[@]}" PATH="$BAD" "$NODE" -e "$KEEP"; }
    fi
    cmd_start && echo STARTED; cmd_stop ) 2>&1
}
out="$(started)" || true
[[ "$out" == *STARTED* ]] && pass "post-window: start keeps an app with the instance PATH" || fail "post-window: start stopped a good app: $out"
out="$(started "$BASE")" || true
[[ "$out" == *"stopped the instance"* && "$out" != *STARTED* ]] \
  && pass "post-window: start stops an app that lost the stub's PATH" || fail "post-window: start kept an app without the stub's PATH: $out"

# keychain_proc_ok reads the env part, and only PATH.
# proc_with <env args...>: a node with that env, once it runs as node.
proc_with() {
  env "$@" "$NODE" -e "$KEEP" ${PROC_ARGS[@]+"${PROC_ARGS[@]}"} &
  PROC=$!
  for _ in $(seq 1 50); do [[ "$(ps -o comm= -p "$PROC")" == *node ]] && return; sleep 0.05; done
}
DIR="$(dirname "$KEYCHAIN_STUB")" PROC_ARGS=()
# proc_case <ok|fails> <what> <env args...>
proc_case() {
  local want="$1" what="$2"; shift 2
  proc_with "$@"; expect "$want" "keychain_proc_ok on $what" keychain_proc_ok "$PROC"
  kill "$PROC"; wait "$PROC" 2>/dev/null || true
}
proc_case ok "a process with the instance PATH" PATH="$P"
proc_case fails "a process with the stub's dir last" PATH="$BASE:$DIR"
proc_case fails "a process whose XPATH, not PATH, starts with the stub's dir" PATH="$BASE" XPATH="$DIR:x"
PROC_ARGS=(" PATH=$DIR:x")
proc_case fails "a process whose argv, not env, has PATH=<stub's dir>" PATH="$BASE"
PROC_ARGS=()

# The open route (macOS): a compiled fake bundle that runs probe.sh and stays
# up 4 s. Without the stub's PATH in --env, start must stop it as soon as it
# has a pid. The front app must not change.
if [[ "$EVAL_OS" == macos ]]; then
  OPEN_APP="$TEST_DIR/Probe.app"
  mkdir -p "$OPEN_APP/Contents/MacOS" "$TEST_DIR/src"
  cat >"$OPEN_APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.example.giga-eval-keychain-test</string>
<key>CFBundleExecutable</key><string>monocode</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSBackgroundOnly</key><true/>
</dict></plist>
PLIST
  printf '#include <stdlib.h>\n#include <unistd.h>\nint main(void) { system("/bin/sh %s %s"); sleep(4); return 0; }\n' \
    "$TEST_DIR/probe.sh" "$TEST_DIR/open" >"$TEST_DIR/src/probe.c"
  if cc -o "$OPEN_APP/Contents/MacOS/monocode" "$TEST_DIR/src/probe.c" 2>/dev/null; then
    echo "$OPEN_APP" >"$EVAL_OUT/instance.app"
    front() { lsappinfo info -only name "$(lsappinfo front)" 2>/dev/null; }
    front_before="$(front)" before="$(log_lines)"
    out="$(env PATH="$BASE" GIGA_EVAL_OPEN=1 bash "$INSTANCE" start 2>&1)" || true
    [[ "$out" == *"instance exited"* ]] && pass "open: start passes the keychain checks" || fail "open: start stopped: $out"
    probed open "$before"
    # The same launch with the PATH left out of --env (instance.sh sourced).
    # shellcheck disable=SC1090
    out="$( (PATH="$BASE"; export GIGA_EVAL_OPEN=1; source "$INSTANCE"
      eval "$(declare -f open_launch | sed '1s/open_launch/real_open_launch/')"
      open_launch() { local v keep=(); for v in "${APP_ENV[@]}"; do [[ "$v" == PATH=* ]] || keep+=("$v"); done
        APP_ENV=("${keep[@]}"); real_open_launch "$@"; }
      cmd_start) 2>&1)" || true
    [[ "$out" == *"does not have the keychain stub's dir first"*"stopped the instance"* && "$out" != *"instance exited"* ]] \
      && pass "open: start stops an app opened without the stub's PATH, at once" || fail "open: start did not stop it at once: $out"
    [[ "$(front)" == "$front_before" ]] && pass "open: the front app did not change" || fail "open: the front app changed from $front_before to $(front)"
  else
    fail "open: cc could not build the fake bundle"
  fi
fi

[[ ! -e "$DECOY_LOG" ]] && pass "the decoy never ran" || fail "the decoy ran: $(paste -sd' ' - <"$DECOY_LOG")"
exit "$failed"
