# Sourced by instance.sh: what keeps an instance that runs on a copy of the
# real app database from acting as the user. The app builds its children's
# PATH itself (harness.rs `gui_search_path`: the login-shell PATH, the dirs
# under HOME, /opt/homebrew/bin, /usr/local/bin) and probes the CLIs it finds,
# so a narrow PATH protects nothing.
#
# The primary protection is the database: sandbox_scrub moves every session
# of the copy to the fx puppet and drops its queued turns, and sandbox_guard
# refuses to start the app on any database where a session is not on the
# puppet, an automation is enabled, a -wal holds unread changes, or fx would
# not resolve to the puppet. Then, around the app process:
#   sandbox_launch   the one prefix every sandboxed process starts with:
#                    `env -i`, the allowlist from sandbox_env (fixed PATH; no
#                    MONOCODE_*, token or SSH_AUTH_SOCK, so the app cannot
#                    drive the user's running MonoCode over localhost), then
#                    sandbox-exec with the profile;
#   sandbox_home_ok  the temp HOME was made fresh in sandbox mode and links
#                    nothing outside /tmp (no real login);
#   sandbox_profile  sandbox-exec rules for the app and the processes it
#                    execs: exec only of the app, node, the puppet,
#                    /usr/bin/env and git; network only to localhost TCP and
#                    to unix sockets under the temp HOME and the jail dir. The
#                    app needs no other: in the approvals scenario it tries
#                    only /private/var/run/mDNSResponder (DNS) and
#                    /private/var/run/syslog, and passes with both refused.
#                    Writes only to the temp HOME, a
#                    fresh jail TMPDIR and /dev, and never to the app bundle,
#                    the puppet's dir, or a parent of it that could be
#                    renamed away. They do not cover work that launchd runs for the
#                    app: WebKit's network process and apps opened through
#                    NSWorkspace run outside the sandbox;
#   sandbox_preflight  before every start, probes run inside the same env and
#                    profile and fail the start if any rule does not hold;
#   sandbox_watch    polls the app's process tree every 0.1 s and kills the
#                    instance on an executable it does not allow. It can miss
#                    a process that lives under 0.1 s or leaves the tree.
# git cannot exec hooks in the sandbox, so the Giga workspace's pre-commit
# hook (giga_workspace.rs) does not run there; git commits without it.

PUPPET_BIN="$EVAL_HOME/.local/bin/fx"
SANDBOX_STRAY="$EVAL_OUT/sandbox-stray.txt"
SANDBOX_HOME_MARK="$EVAL_HOME/.giga-eval-sandbox-home"
SANDBOX_SECRET='^(MONOCODE_[A-Z_]*|SSH_AUTH_SOCK|[A-Z_]*(TOKEN|SECRET|API_KEY|PASSWORD)[A-Z_]*)='

sandbox_scrub() {
  local db="$1" model="$2" now; now="$(date +%s)000"
  under_tmp "$db" >/dev/null || die "scrub only a copy under /tmp, not $db"
  sqlite3 "$db" "PRAGMA foreign_keys = ON;
    UPDATE sessions SET harness = 'fx', model = '$model', provider_session_id = NULL;
    UPDATE mono_messages SET status = 'dropped', closed_at = $now, notified_at = $now, updated_at = $now
      WHERE delivered_at IS NULL AND status IN ('open', 'delivered');
    DELETE FROM in_flight_sessions; DELETE FROM automations; DELETE FROM session_reminders;"
}

# The instance's own copy after a kill: fold its -wal in and empty it, so the
# guard reads every row. (macOS sqlite keeps the -wal file, empty, on close.)
sandbox_checkpoint() {
  under_tmp "$1" >/dev/null || die "checkpoint only a copy under /tmp, not $1"
  [[ ! -s "$1-wal" ]] || sqlite3 "$1" "PRAGMA wal_checkpoint(TRUNCATE);" >/dev/null
}

# The first executable where the app looks for fx (harness.rs `resolve_fx`),
# in its order, by path only: nothing runs. It must be the puppet. (The app
# also skips a candidate whose `--help` does not name acp; one after the
# puppet could only run if the puppet failed, and the profile refuses its
# exec.)
sandbox_fx_first() {
  local h="$EVAL_HOME" c
  for c in "$h/.local/bin/fx" "$h/.fx/bin/fx" "$h/.npm-global/bin/fx" "$h/.cargo/bin/fx" "$h/n/bin/fx" \
    /opt/homebrew/bin/fx /usr/local/bin/fx /usr/bin/fx /snap/bin/fx; do
    [[ -f "$c" && -x "$c" ]] && { echo "$c"; return; }
  done
}

# Read-only. With an empty or no -wal every row is in the main file, and
# `immutable=1` reads it as is (`mode=ro` alone cannot open a WAL copy that
# lacks its -wal or -shm).
sandbox_guard() {
  local db="file:$1?mode=ro&immutable=1" other automations queued fx
  if [[ -s "$1-wal" ]]; then
    log "unsafe database, the app does not start: $1-wal holds rows the guard may not see; checkpoint the copy"
    return 1
  fi
  other="$(sqlite3 "$db" "SELECT group_concat(harness || '(' || n || ')', ' ') FROM
    (SELECT harness, count(*) AS n FROM sessions WHERE harness <> 'fx' GROUP BY harness)")"
  automations="$(sqlite3 "$db" "SELECT count(*) FROM automations WHERE enabled")"
  queued="$(sqlite3 "$db" "SELECT count(*) FROM mono_messages WHERE delivered_at IS NULL AND status IN ('open', 'delivered')")"
  fx="$(sandbox_fx_first)"
  if [[ -n "$other" || "$automations" != 0 || "$fx" != "$PUPPET_BIN" ]]; then
    log "unsafe database, the app does not start: $queued queued turns, $automations automations;" \
      "sessions not on the puppet: ${other:-none}; fx resolves to ${fx:-nothing}"
    return 1
  fi
}

sandbox_home_ok() {
  local bad
  [[ -f "$SANDBOX_HOME_MARK" ]] || { log "sandbox start needs a fresh home: instance.sh home --fresh"; return 1; }
  bad="$(unsafe_home_link)" || { log "unsafe temp HOME link: $bad"; return 1; }
}

# One NAME=value per line: the whole environment the sandboxed app gets. PATH
# is the puppet's dir, a dir holding only a `node` link (for the puppet's
# `#!/usr/bin/env node`; node's own dir also holds real CLIs such as codex),
# and /usr/bin:/bin, which has git. Never the caller's. Both dirs are outside
# every path the profile lets the sandbox write.
sandbox_env() {
  mkdir -p "$EVAL_OUT/bin" && ln -sf "$(realpath "$(command -v node)")" "$EVAL_OUT/bin/node"
  local path="$(dirname "$PUPPET_BIN"):$EVAL_OUT/bin:/usr/bin:/bin"
  printf '%s\n' "HOME=$EVAL_HOME" "CFFIXED_USER_HOME=$EVAL_HOME" "USER=$(id -un)" "LOGNAME=$(id -un)" \
    "SHELL=/bin/zsh" "PATH=$path" "TMPDIR=$1/" "LANG=${LANG:-en_US.UTF-8}"
}

# sandbox_launch <profile> <env lines>: sets SANDBOX_LAUNCH, the command
# prefix of the preflight and of the app.
sandbox_launch() {
  local vars; IFS=$'\n' read -r -d '' -a vars <<<"$2" || true
  SANDBOX_LAUNCH=(env -i "${vars[@]}" sandbox-exec -f "$1")
}

sandbox_env_ok() {
  local secret; secret="$(grep -oE "$SANDBOX_SECRET" <<<"$1" | paste -sd' ' -)"
  [[ -z "$secret" ]] || { log "the sandboxed app would get: $secret"; return 1; }
}

sandbox_git() { xcrun --find git 2>/dev/null || echo /usr/bin/git; }

# sandbox_profile <app binary> <jail tmp> <out>: one rule per line.
sandbox_profile() {
  local app home bin jail; app="$(realpath "$1")"; home="$(realpath "$EVAL_HOME")"; bin="$home/.local/bin"
  jail="$(realpath "$2")"
  cat >"$3" <<SB
(version 1)
(allow default)
(deny process-exec)
(allow process-exec (literal "$app") (literal "$(realpath "$(command -v node)")") (literal "$bin/fx") (literal "/usr/bin/env") (literal "/usr/bin/git") (literal "$(sandbox_git)"))
(deny network-outbound)
(allow network-outbound (remote ip "localhost:*"))
(allow network-outbound (remote unix-socket (subpath "$home")) (remote unix-socket (subpath "$jail")))
(deny file-write*)
(allow file-write* (subpath "$home") (subpath "$jail") (subpath "/dev"))
(deny file-write* (subpath "$bin") (literal "$home/.local") (literal "$home") (subpath "${app%/Contents/MacOS/*}"))
SB
}

# sandbox_preflight <app binary> <jail tmp>: each probe runs under
# SANDBOX_LAUNCH; any rule that does not hold fails the start. The unix socket
# it must not reach is a test listener outside the jail, not a real one.
sandbox_preflight() {
  local node want got missing sockdir listener i; node="$(realpath "$(command -v node)")"
  sockdir="$(mktemp -d /private/tmp/giga-eval-sock.XXXXXX)"
  "$node" -e 'require("net").createServer((c) => c.end()).listen(process.argv[1])' "$sockdir/s" &
  listener=$!
  for i in $(seq 1 40); do [[ -S "$sockdir/s" ]] && break; sleep 0.05; done
  want="env= home=$EVAL_HOME connect=EPERM sock=EPERM sock_jail=ok real_home=EPERM tmp=EPERM user_tmp=EPERM
puppet=EPERM app=EPERM rename_local=EPERM rename_home=EPERM zsh=EPERM jail=ok write_home=ok exec_node=ok"
  got="$("${SANDBOX_LAUNCH[@]}" "$node" -e '
    const [real, utmp, puppet, app, jail, sock] = process.argv.slice(1);
    const fs = require("fs"), net = require("net"), cp = require("child_process");
    const home = process.env.HOME;
    const out = (k, v) => console.log(`${k}=${v}`);
    const write = (p, keep) => { try { fs.appendFileSync(p, ""); if (!keep) fs.unlinkSync(p); return "ok"; } catch (e) { return e.code; } };
    const rename = (p, to) => { try { fs.renameSync(p, to); fs.renameSync(to, p); return "ok"; } catch (e) { return e.code; } };
    out("env", Object.keys(process.env).filter((k) => /^(MONOCODE_|SSH_AUTH_SOCK$)|TOKEN|SECRET|API_KEY|PASSWORD/.test(k)).join(","));
    out("home", home);
    out("real_home", write(`${real}/.giga-eval-preflight-${process.pid}`));
    out("tmp", write(`/private/tmp/giga-eval-preflight-${process.pid}`));
    out("user_tmp", write(`${utmp}/giga-eval-preflight-${process.pid}`));
    out("puppet", write(puppet, true));
    out("app", write(app, true));
    out("rename_local", rename(`${home}/.local`, `${home}/.local.moved`));
    out("rename_home", rename(home, `${jail}/home`));
    out("jail", write(`${jail}/preflight`));
    out("write_home", write(`${home}/.giga-eval-preflight`));
    const pids = [process.pid];
    const run = (f, a) => { const r = cp.spawnSync(f, a); pids.push(r.pid); return r.error ? r.error.code : "ok"; };
    out("zsh", run("/bin/zsh", ["-c", "true"]));
    out("exec_node", run(process.execPath, ["-e", "0"]));
    out("pids", pids.filter(Boolean).join(","));
    const connect = (k, to) => new Promise((done) => {
      const s = net.connect(to);
      const t = setTimeout(() => { out(k, "timeout"); s.destroy(); done(); }, 1500);
      s.on("connect", () => { clearTimeout(t); out(k, "ok"); s.destroy(); done(); });
      s.on("error", (e) => { clearTimeout(t); out(k, e.code); done(); });
    });
    const own = net.createServer((c) => c.end()).listen(`${jail}/preflight.sock`, async () => {
      await Promise.all([connect("connect", { host: "192.0.2.1", port: 9 }), connect("sock", sock),
        connect("sock_jail", `${jail}/preflight.sock`)]);
      own.close();
    });
  ' "$REAL_HOME" "$(getconf DARWIN_USER_TEMP_DIR)" "$PUPPET_BIN" "$1" "$2" "$sockdir/s" 2>&1 | sort)"
  kill "$listener" 2>/dev/null; wait "$listener" 2>/dev/null; rm -rf "$sockdir"
  grep '^pids=' <<<"$got" | cut -d= -f2 | tr , '\n' >>"$EVAL_OUT/preflight.pids"
  missing="$(comm -23 <(tr ' ' '\n' <<<"$want" | sort) <(echo "$got"))"
  [[ -z "$missing" ]] || { log "sandbox preflight failed: wanted $(paste -sd' ' - <<<"$missing"); got $(paste -sd' ' - <<<"$got")"; return 1; }
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
# processes since `$1` (a `log show --start` date), with counts, leaving out
# the preflight's own probes (since the last fresh home).
sandbox_denied() {
  local probes; probes="$(sed 's/.*/ node(&)/' "$EVAL_OUT/preflight.pids" 2>/dev/null)"
  /usr/bin/log show --style compact --start "$1" \
    --predicate 'sender == "Sandbox" AND eventMessage CONTAINS "deny(1)"' 2>/dev/null \
    | grep -v -e 'duplicate report' -e giga-eval-preflight | grep -vF "${probes:-(none)}" \
    | grep -oE ' (monocode|node|env|git)\([0-9]+\) deny\(1\) (process-exec|network-outbound|file-write)[a-z*-]* [^ ]+' \
    | awk '{print $3 " " $4}' | sort | uniq -c \
    | awk '{printf "%s%s %s(%s)", sep, $2, $3, $1; sep = ", "}'
}
