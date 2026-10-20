# Sourced by instance.sh: keep an instance that runs on a copy of the real
# app database from starting any real agent, reaching the network or
# writing outside /tmp. The app adds the login-shell PATH, the dirs under
# HOME, /opt/homebrew/bin and /usr/local/bin to its children's PATH
# (harness.rs `gui_search_path`) and probes the CLIs it finds there, so a
# narrow PATH protects nothing. Four layers do:
#   sandbox_scrub    on the copy only: every session runs on the puppet
#                    harness, queued turns are dropped, and in-flight
#                    sessions, automations and reminders are cleared;
#   sandbox_guard    a sandboxed start refuses a database where a session's
#                    CLI resolves on the app's PATH to anything but the puppet,
#                    or where an automation is enabled;
#   sandbox_profile  the app runs under sandbox-exec: exec only of the app,
#                    node, the puppet and git (the Giga workspace is a git
#                    repository); network only to localhost; writes only
#                    under /tmp, the per-user temp dir and /dev;
#   sandbox_watch    any process under the app whose executable is not one of
#                    those kills the instance at once. It polls every 0.1 s,
#                    so the profile, not the watcher, stops a short-lived one.

PUPPET_BIN="$EVAL_HOME/.local/bin/fx"
SANDBOX_STRAY="$EVAL_OUT/sandbox-stray.txt"

sandbox_scrub() {
  local db="$1" model="$2" now; now="$(date +%s)000"
  under_tmp "$db" >/dev/null || die "scrub only a copy under /tmp, not $db"
  sqlite3 "$db" "PRAGMA foreign_keys = ON;
    UPDATE sessions SET harness = 'fx', model = '$model', provider_session_id = NULL;
    UPDATE mono_messages SET status = 'dropped', closed_at = $now, notified_at = $now, updated_at = $now
      WHERE delivered_at IS NULL AND status IN ('open', 'delivered');
    DELETE FROM in_flight_sessions; DELETE FROM automations; DELETE FROM session_reminders;"
}

# A superset of the PATH the app's children get (gui_search_path): the login
# shell here also inherits this script's PATH.
sandbox_app_path() {
  local h="$EVAL_HOME" login
  login="$(HOME="$h" "${SHELL:-/bin/zsh}" -l -c 'printf %s "$PATH"' 2>/dev/null </dev/null || true)"
  printf '%s' "$login:$PATH:$h/.local/bin:$h/.cargo/bin:$h/.claude/local:$h/.local/share/claude:$h/.opencode/bin"
  printf '%s' ":$h/.grok/bin:$h/.npm-global/bin:$h/.bun/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/snap/bin"
}

# Read-only. A WAL database with no -shm file (a fresh `.backup`, a clean
# close) has no WAL content to read, but `mode=ro` alone cannot open it.
sandbox_guard() {
  local db="file:$1?mode=ro" path harness bin found unsafe="" automations queued
  [[ -e "$1-shm" ]] || db+="&immutable=1"
  path="$(sandbox_app_path)"
  for harness in $(sqlite3 "$db" "SELECT DISTINCT harness FROM sessions ORDER BY 1"); do
    case "$harness" in cursor) bin=cursor-agent ;; *) bin="$harness" ;; esac
    found="$(PATH="$path" command -v "$bin" || true)"
    [[ -z "$found" || "$found" == "$PUPPET_BIN" ]] || unsafe+=" $harness=$found"
  done
  automations="$(sqlite3 "$db" "SELECT count(*) FROM automations WHERE enabled")"
  queued="$(sqlite3 "$db" "SELECT count(*) FROM mono_messages WHERE delivered_at IS NULL AND status IN ('open', 'delivered')")"
  if [[ -n "$unsafe" || "$automations" != 0 ]]; then
    log "unsafe database, the app does not start: $queued queued turns, $automations automations; sessions on a real CLI:${unsafe:- none}"
    return 1
  fi
}

sandbox_git() { xcrun --find git 2>/dev/null || echo /usr/bin/git; }

sandbox_profile() {
  local out="$EVAL_OUT/instance.sb"
  cat >"$out" <<SB
(version 1)
(allow default)
(deny process-exec)
(allow process-exec (literal "$(realpath "$1")") (literal "$(realpath "$(command -v node)")")
  (literal "$(realpath "$PUPPET_BIN")") (literal "/usr/bin/env") (literal "/usr/bin/git") (literal "$(sandbox_git)"))
(deny network-outbound)
(allow network-outbound (remote ip "localhost:*"))
(allow network-outbound (remote unix-socket))
(deny network-outbound (remote unix-socket (path-literal "/private/var/run/mDNSResponder")))
(deny file-write*)
(allow file-write* (subpath "/private/tmp") (subpath "$(realpath "${TMPDIR:-/tmp}")") (subpath "/dev"))
SB
  echo "$out"
}

# sandbox_allowed <executable> <command line>: the app's own binary (the
# app, app-mcp, the `app` CLI), git, or node running the puppet, directly or
# through /usr/bin/env. Matched on the executable, never on the arguments
# alone: a real agent's argv can name the app (`--mcp-config`). A process
# that is exiting has no argv left (ps shows `(node)`); its executable
# decides.
sandbox_allowed() {
  local exe="$1" args="$2" app="$3" puppet="$PUPPET_BIN" real
  real="$(realpath "$puppet" 2>/dev/null || echo "$puppet")"
  case "$exe" in
    "$app" | /usr/bin/git | "$(sandbox_git)") return 0 ;;
    /usr/bin/env) args="${args#/usr/bin/env }" ;;
    "$(realpath "$(command -v node)")") ;;
    *) return 1 ;;
  esac
  [[ "$args" != "("*")" ]] || return 0
  args="${args#* }"
  [[ "$args " == "$puppet "* || "$args " == "$real "* ]]
}

# sandbox_watch <app pid> <app executable>: until the app exits. On a stray
# process: record it, kill the app and every descendant, and send USR1 to
# $GIGA_EVAL_WATCH_NOTIFY when set.
sandbox_watch() {
  local root="$1" app="$2" pid exe args kids
  while kill -0 "$root" 2>/dev/null; do
    sleep 0.1
    kids="$(descendants "$root" | paste -sd, -)"
    [[ -n "$kids" ]] || continue
    while read -r pid exe; do
      args="$(/bin/ps -o args= -p "$pid" 2>/dev/null)" || continue
      sandbox_allowed "$exe" "$args" "$app" && continue
      echo "$pid $exe: $args" >>"$SANDBOX_STRAY"
      kill -KILL "$root" $(descendants "$root") 2>/dev/null || true
      rm -f "$EVAL_OUT/instance.pid"
      [[ -z "${GIGA_EVAL_WATCH_NOTIFY:-}" ]] || kill -USR1 "$GIGA_EVAL_WATCH_NOTIFY" 2>/dev/null || true
      return
    done < <(lsof -a -d txt -Fpn -p "$kids" 2>/dev/null \
      | awk '/^p/ {p = substr($0, 2); first = 1; next} /^n/ && first {print p, substr($0, 2); first = 0}')
  done
}

# What the profile's rules (exec, network, writes) refused the instance's
# processes since `$1` (a `log show --start` date), with counts.
sandbox_denied() {
  /usr/bin/log show --style compact --start "$1" \
    --predicate 'sender == "Sandbox" AND eventMessage CONTAINS "deny(1)"' 2>/dev/null \
    | grep -v 'duplicate report' \
    | grep -oE ' (monocode|node|env|git)\([0-9]+\) deny\(1\) (process-exec|network-outbound|file-write)[a-z*-]* [^ ]+' \
    | awk '{print $3 " " $4}' | sort | uniq -c \
    | awk '{printf "%s%s %s(%s)", sep, $2, $3, $1; sep = ", "}'
}
