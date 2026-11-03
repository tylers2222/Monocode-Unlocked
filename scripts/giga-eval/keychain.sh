# shellcheck shell=bash
# Sourced by instance.sh: what keeps an instance off the user's keychain
# (ADE-233). rate_limits.rs runs `security` by name to read Claude Code's
# login, and the keychain belongs to the user, not to HOME: a temp HOME and
# another bundle id alone still let the instance read the real token. So
# `home` writes a stub `security` into the temp HOME's bin dir, every PATH the
# app gets starts with that dir, and `start` refuses a PATH where `security`
# is anything else. The stub logs its arguments to $KEYCHAIN_LOG and exits 44.
# In sandbox mode the profile also refuses its exec, so a call fails unlogged.

KEYCHAIN_STUB="$EVAL_HOME/.local/bin/security"
KEYCHAIN_LOG="$EVAL_OUT/security.log"

keychain_stub_text() {
  local log; log="$(printf %q "$KEYCHAIN_LOG")"
  cat <<STUB
#!/bin/bash
# giga-eval keychain stub (keychain.sh): the instance never reads the user's keychain.
printf '%s %s\n' "\$(date +%H:%M:%S)" "\$*" >>$log
exit 44
STUB
}

# After unsafe_home_link: the write must not go through a link.
keychain_stub_write() {
  mkdir -p "$(dirname "$KEYCHAIN_STUB")"
  keychain_stub_text >"$KEYCHAIN_STUB"
  chmod 755 "$KEYCHAIN_STUB"
}

# instance_path <path>: the PATH the app gets, the stub's dir first.
instance_path() { printf '%s\n' "$(dirname "$KEYCHAIN_STUB"):$1"; }

# keychain_stub_ok <PATH>: `security` under <PATH> is the stub, unchanged.
keychain_stub_ok() {
  local got; got="$(PATH="$1" command -v security)" || got=""
  [[ "$got" == "$KEYCHAIN_STUB" ]] && cmp -s "$KEYCHAIN_STUB" <(keychain_stub_text) && return 0
  log "security under the instance's PATH is ${got:-nothing}, not the stub $KEYCHAIN_STUB (as keychain.sh writes it)"
  return 1
}

# keychain_proc_ok <pid>: the running process's PATH starts with the stub's
# dir. For a launch that does not take our env, such as one through `open`.
keychain_proc_ok() {
  local cmd; cmd="$(ps eww -o command= -p "$1" 2>/dev/null)" || cmd=""
  [[ "$cmd" == *" PATH=$(dirname "$KEYCHAIN_STUB"):"* ]] && return 0
  log "pid $1 does not have the keychain stub's dir first on its PATH"
  return 1
}
