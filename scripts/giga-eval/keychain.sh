# shellcheck shell=bash
# Sourced by instance.sh: what keeps an instance off the user's keychain
# (ADE-233). The keychain belongs to the user, not to HOME: a temp HOME and
# another bundle id alone still let the instance read the real token. Two kinds
# of caller run `security` by name:
#   the app itself: rate_limits.rs (`Command::new("security")`, the app's own PATH);
#   the app's children: harness CLIs (`claude` reads its login with
#     `security find-generic-password` through a shell), terminals, git and
#     checkpoint turns. Their PATH is harness.rs `gui_search_path`, which puts
#     the PATH of `$SHELL -lic printenv` first, and on macOS that login shell's
#     path_helper (/etc/zprofile, /etc/profile) moves /usr/bin ahead of the
#     inherited PATH.
# So `home` writes a stub `security` into the temp HOME's bin dir, and a line
# that puts that dir first again as the last line of the temp HOME's login
# dotfiles, which run after path_helper. `start` refuses unless `security` is
# the stub both on the app's PATH and in that login shell. The stub logs its
# arguments to $KEYCHAIN_LOG and exits 44.
#
# NOT covered: a caller that runs /usr/bin/security by absolute path. The app's
# source has none (rate_limits.rs:591,605 use the name; the one absolute
# /usr/bin/security is a test, src-tauri/src/tests/scenario/logins.rs:245), but
# an agent's shell command in the instance can still run it. Only sandbox mode
# stops that: its profile refuses every exec but the app, node, the puppet,
# /usr/bin/env and git, so there the stub's exec fails too, unlogged.

KEYCHAIN_STUB="$EVAL_HOME/.local/bin/security"
KEYCHAIN_LOG="$EVAL_OUT/security.log"
KEYCHAIN_MARK="# giga-eval keychain stub (keychain.sh)"
# Every login file a zsh or bash `-lic` reads after path_helper.
KEYCHAIN_DOTFILES=(.zprofile .zshrc .bash_profile .profile)

keychain_stub_text() {
  local log; log="$(printf %q "$KEYCHAIN_LOG")"
  cat <<STUB
#!/bin/bash
# giga-eval keychain stub (keychain.sh): \`security\` by name never reaches the user's keychain.
printf '%s %s\n' "\$(date +%H:%M:%S)" "\$*" >>$log
exit 44
STUB
}

# shellcheck disable=SC2016 # $PATH is the login shell's, at its login
keychain_dotfile_line() { printf 'export PATH=%q:"$PATH" %s\n' "$(dirname "$KEYCHAIN_STUB")" "$KEYCHAIN_MARK"; }

# After unsafe_home_link: no write may go through a link. In each dotfile the
# line is moved to the end, so nothing else in the file runs after it.
keychain_stub_write() {
  local f
  mkdir -p "$(dirname "$KEYCHAIN_STUB")"
  keychain_stub_text >"$KEYCHAIN_STUB"
  chmod 755 "$KEYCHAIN_STUB"
  for f in "${KEYCHAIN_DOTFILES[@]}"; do
    f="$EVAL_HOME/$f"
    { grep -vF "$KEYCHAIN_MARK" "$f" 2>/dev/null || true; keychain_dotfile_line; } >"$f.giga-eval"
    mv "$f.giga-eval" "$f"
  done
}

# instance_path <path>: the PATH the app gets, the stub's dir first.
instance_path() { printf '%s\n' "$(dirname "$KEYCHAIN_STUB"):$1"; }

keychain_is_stub() { [[ "$1" == "$KEYCHAIN_STUB" ]] && cmp -s "$KEYCHAIN_STUB" <(keychain_stub_text); }

# keychain_stub_ok <PATH>: `security` under <PATH> is the stub, unchanged.
keychain_stub_ok() {
  local got; got="$(PATH="$1" command -v security)" || got=""
  keychain_is_stub "$got" && return 0
  log "security under the instance's PATH is ${got:-nothing}, not the stub $KEYCHAIN_STUB (as keychain.sh writes it)"
  return 1
}

# keychain_login_ok <env command...>: in the app's env (<env command>, such as
# `env -i NAME=value...`), the login shell the app takes its children's PATH
# from (harness.rs load_unix_login_shell_env: `$SHELL -lic`) resolves
# `security` to the stub.
keychain_login_ok() {
  local got
  # shellcheck disable=SC2016 # $SHELL is the app env's, expanded by sh
  got="$("$@" /bin/sh -c 'exec "$SHELL" -lic "command -v security"' </dev/null 2>/dev/null | tail -n 1)" || got=""
  keychain_is_stub "$got" && return 0
  log "security in the instance's login shell is ${got:-nothing}, not the stub $KEYCHAIN_STUB"
  return 1
}

# keychain_proc_ok <pid>: the running process's own PATH starts with the
# stub's dir. Read from its env, the part of `ps eww` after its argv, and it
# must hold exactly one PATH. For a launch that does not take our env, such as
# one through `open`. Fails when ps shows no env (an Apple-signed binary).
keychain_proc_ok() {
  local args all envs n
  args="$(ps -ww -o command= -p "$1" 2>/dev/null)" && all="$(ps eww -o command= -p "$1" 2>/dev/null)" || all=""
  envs="${all#"$args"}"
  n="$(grep -o ' PATH=' <<<"$envs" | wc -l | tr -d ' ')"
  [[ -n "$all" && "$envs" != "$all" && "$n" == 1 && "$envs" == *" PATH=$(dirname "$KEYCHAIN_STUB"):"* ]] && return 0
  log "pid $1 does not have the keychain stub's dir first on its PATH"
  return 1
}
