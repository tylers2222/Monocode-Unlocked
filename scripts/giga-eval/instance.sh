#!/usr/bin/env bash
# Build, launch, check and stop an isolated "MonoCode GigaEval" release instance.
#
#   instance.sh build <checkout> [label]   copy the checkout's working tree, build a release .app
#                                          (Linux: a release binary) with cargo features
#                                          $GIGA_EVAL_FEATURES (default fake-clock,test-db-key); a checkout
#                                          without .git (the E2E image) needs $GIGA_EVAL_REV
#   instance.sh home [--fresh]             create the temp HOME; refuse one where a link
#                                          resolves into the real HOME
#   instance.sh start [--fresh-home]       launch the last build under the temp HOME
#                                          (Linux: on the X server in $DISPLAY, e.g. Xvfb)
#   instance.sh lsof-check                 prove no open file under the real app data or ~/MonoCode
#   instance.sh stop                       quit the instance and every process it started
#   instance.sh status
#   instance.sh scrub <model>             on the instance's database: every session on the fx puppet,
#                                          no queued turn (for a copy of the real database)
#   instance.sh guard [db]                 fail when a sandboxed start would refuse <db>
#   instance.sh denied <since>             what the sandbox refused since a `log show` date
#
# GIGA_EVAL_LOGINS=1 (macOS, for a run on a real model): `home` copies LOGINS
# from the real HOME. Without it the temp HOME holds no login at all.
#
# GIGA_EVAL_SANDBOX=1 (see sandbox.sh, macOS only): `home` copies no logins, and
# `start` needs a fresh home, refuses an unsafe database, starts the app from
# `env -i` under sandbox.sh's profile after a preflight, and watches its
# process tree (GIGA_EVAL_WATCH_NOTIFY: a pid to get USR1 on a stray).
source "$(dirname "$0")/lib.sh"
source "$(dirname "$0")/sandbox.sh"

SRC_ROOT="$EVAL_OUT/src"
TARGET_DIR="$EVAL_OUT/target"
APP_DATA="$EVAL_APP_DATA"
# The files the harnesses log in with: omp's credentials and settings, Codex's
# auth, git's identity. Copied, never linked, so every write the instance makes
# (token refreshes too) stays in the temp HOME. Claude Code keeps its login in
# the macOS keychain, which no temp HOME shares, so it has none here.
LOGINS=(.omp/agent/agent.db .omp/agent/config.yml .codex/auth.json .gitconfig)
# Where they come from. The home-isolation scenario seeds one under /tmp.
LOGIN_HOME="${GIGA_EVAL_LOGIN_HOME:-$REAL_HOME}"

# What differs per OS. install_build <label> <src> copies the build to
# apps/<label>, records it in instance.app and checks its identifier.
# has_window <pid> is true once the process shows a window. launch <bin>
# [jail...] execs the app in place of the calling subshell.
if [[ "$EVAL_OS" == macos ]]; then
  BUNDLE=(--bundles app)
  LEAK_DIRS=("$REAL_HOME/Library/WebKit/$EVAL_ID" "$REAL_HOME/Library/Application Support/$EVAL_ID" "$REAL_HOME/Library/Caches/$EVAL_ID")
  install_build() {
    local built="$TARGET_DIR/release/bundle/macos/$EVAL_PRODUCT.app" dest="$EVAL_OUT/apps/$1/$EVAL_PRODUCT.app"
    [[ -d "$built" ]] || die "no bundle at $built"
    mkdir -p "$(dirname "$dest")"; rm -rf "$dest"; cp -a "$built" "$dest"
    echo "$dest" >"$EVAL_OUT/instance.app"
    local bid; bid="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$dest/Contents/Info.plist")"
    [[ "$bid" == "$EVAL_ID" ]] || die "bundle identifier is $bid, expected $EVAL_ID"
    local sig; sig="$(codesign -dv "$dest" 2>&1)"
    [[ "$sig" == *"Signature=adhoc"* ]] || die "bundle is not ad-hoc signed"
    log "built $dest (identifier $bid, ad-hoc, rev $(cut -c1-10 "$2/.giga-eval-rev"))"
  }
  has_window() { osascript -l JavaScript "$EVAL_DIR/ax.js" windowid "$1" >/dev/null 2>&1; }
  # Opened as a plain child, not through LaunchServices, so HOME and the
  # persistence flags reach the process.
  launch() {
    local bin="$1"; shift
    HOME="$EVAL_HOME" CFFIXED_USER_HOME="$EVAL_HOME" PATH="$PATH" \
      exec nohup "$@" "$bin" -ApplePersistence NO -NSQuitAlwaysKeepsWindows NO
  }
else
  BUNDLE=(--no-bundle)
  LEAK_DIRS=("$REAL_HOME/.local/share/$EVAL_ID" "$REAL_HOME/.cache/$EVAL_ID" "$REAL_HOME/.config/$EVAL_ID")
  # Linux runs in a container that holds no logins.
  LOGINS=()
  install_build() {
    local built="$TARGET_DIR/release/monocode" dest="$EVAL_OUT/apps/$1/monocode"
    [[ -x "$built" ]] || die "no binary at $built"
    mkdir -p "$(dirname "$dest")"; cp "$built" "$dest"
    echo "$dest" >"$EVAL_OUT/instance.app"
    # No Info.plist: the identifier is in the tauri config the binary embeds.
    grep -qaF "$EVAL_ID" "$dest" || die "binary does not embed identifier $EVAL_ID"
    log "built $dest (identifier $EVAL_ID, rev $(cut -c1-10 "$2/.giga-eval-rev"))"
  }
  has_window() { xdotool search --onlyvisible --pid "$1" >/dev/null 2>&1; }
  # The XDG folders follow HOME, so app_data_dir() is $EVAL_APP_DATA.
  launch() {
    HOME="$EVAL_HOME" XDG_DATA_HOME="$EVAL_HOME/.local/share" XDG_CONFIG_HOME="$EVAL_HOME/.config" \
      XDG_CACHE_HOME="$EVAL_HOME/.cache" PATH="$PATH" exec nohup "$1"
  }
fi

cmd_build() {
  local checkout="${1:?checkout path}" label="${2:-$(basename "$1")}"
  checkout="$(cd "$checkout" && pwd)"
  local src="$SRC_ROOT/$label" logf="$EVAL_OUT/build-$label.log"
  mkdir -p "$src" "$TARGET_DIR"
  local rev dirty
  if rev="$(git -C "$checkout" rev-parse HEAD 2>/dev/null)"; then
    dirty="$(git -C "$checkout" status --porcelain | wc -l | tr -d ' ') dirty paths"
  else
    rev="${GIGA_EVAL_REV:?$checkout is not a git checkout; set GIGA_EVAL_REV to its commit}" dirty="no .git"
  fi
  log "copying $checkout (${rev:0:10}, $dirty) -> $src"
  # Build from a copy so this script never writes into the checkout under test.
  rsync -a --delete --exclude node_modules --exclude target --exclude dist --exclude .git \
    "$checkout/" "$src/"
  echo "$rev" >"$src/.giga-eval-rev"
  local override="$src/tauri.gigaeval.json"
  printf '%s\n' "{\"productName\":\"$EVAL_PRODUCT\",\"identifier\":\"$EVAL_ID\",\"bundle\":{\"createUpdaterArtifacts\":false,\"macOS\":{\"signingIdentity\":\"-\",\"hardenedRuntime\":false}}}" >"$override"
  local lock_hash; lock_hash="$(shasum -a 256 "$src/package-lock.json" | cut -c1-16)"
  if [[ ! -f "$src/node_modules/.giga-eval-lock" || "$(cat "$src/node_modules/.giga-eval-lock")" != "$lock_hash" ]]; then
    log "npm ci ($logf)"
    (cd "$src" && npm ci --no-audit --no-fund) >"$logf" 2>&1 || die "npm ci failed, see $logf"
    echo "$lock_hash" >"$src/node_modules/.giga-eval-lock"
  fi
  local features="${GIGA_EVAL_FEATURES-fake-clock,test-db-key}"
  log "tauri build, features '$features' ($logf)"
  (cd "$src" && CARGO_TARGET_DIR="$TARGET_DIR" npx tauri build "${BUNDLE[@]}" --config tauri.gigaeval.json \
    ${features:+--features "$features"}) >>"$logf" 2>&1 \
    || die "tauri build failed, see $logf"
  install_build "$label" "$src"
}

# Refuses a temp HOME where a link resolves into the real HOME or the login
# source: a write through it would land in the user's own files (ADE-112).
home_guard() {
  local link target home homes=()
  for home in "$REAL_HOME" "$LOGIN_HOME"; do
    target="$(resolve "$home")" || die "$home is a link loop"
    homes+=("$target")
  done
  while IFS= read -r -d '' link; do
    target="$(resolve "$link")" || die "$link is a link loop"
    for home in "${homes[@]}"; do
      [[ "$target/" != "$home/"* ]] \
        || die "$link resolves into $home ($target); make a fresh one: instance.sh home --fresh"
    done
  done < <(find "$EVAL_HOME" -type l -print0)
}

copy_logins() {
  local rel src dst copied=0
  for rel in ${LOGINS[@]+"${LOGINS[@]}"}; do
    src="$LOGIN_HOME/$rel" dst="$EVAL_HOME/$rel"
    [[ -f "$src" && ! -e "$dst" && ! -L "$dst" ]] || continue
    mkdir -p "$(dirname "$dst")"
    # A backup is a consistent copy of a live database, -wal included.
    if [[ "$rel" == *.db ]]; then sqlite3 "file:$src?mode=ro" ".backup '$dst'"; else cp "$src" "$dst"; fi
    chmod 600 "$dst"
    copied=$((copied + 1))
  done
  log "copied $copied login files from $LOGIN_HOME"
}

cmd_home() {
  if [[ "${1:-}" == "--fresh" ]]; then
    [[ -z "$(target_pid instance)" ]] || die "stop the instance before --fresh"
    rm -rf "$EVAL_HOME"   # rm -rf removes symlinks, never their targets
    mkdir -p "$EVAL_HOME"
    [[ "${GIGA_EVAL_SANDBOX:-}" != 1 ]] || { touch "$SANDBOX_HOME_MARK"; : >"$EVAL_OUT/preflight.pids"; }
  fi
  mkdir -p "$EVAL_HOME/Library"
  home_guard
  [[ "${GIGA_EVAL_LOGINS:-}" != 1 || "${GIGA_EVAL_SANDBOX:-}" == 1 ]] || copy_logins
  log "temp HOME $EVAL_HOME ready"
}


cmd_start() {
  [[ -z "$(target_pid instance)" ]] || die "instance already running (pid $(target_pid instance))"
  [[ "$EVAL_OS" == macos || "${GIGA_EVAL_SANDBOX:-}" != 1 ]] \
    || die "GIGA_EVAL_SANDBOX=1 is macOS only (sandbox-exec); on Linux the container is the isolation"
  [[ "$EVAL_OS" == macos || -n "${DISPLAY:-}" ]] || die "set DISPLAY to an X server (for example Xvfb :99)"
  if [[ "${1:-}" == "--fresh-home" ]]; then cmd_home --fresh; else cmd_home; fi
  local app; app="$(cat "$EVAL_OUT/instance.app" 2>/dev/null)" || die "build first"
  local bin jail=(); bin="$(app_bin "$app")"
  if [[ "${GIGA_EVAL_SANDBOX:-}" == 1 ]]; then
    sandbox_home_ok || exit 1
    if [[ -f "$APP_DATA/monocode.db" ]]; then
      sandbox_checkpoint "$APP_DATA/monocode.db"
      sandbox_guard "$APP_DATA/monocode.db" || exit 1
    fi
    local jailtmp envs profile="$EVAL_OUT/instance.sb"
    jailtmp="$(cat "$EVAL_OUT/instance.jailtmp" 2>/dev/null || true)"
    [[ "$jailtmp" != /private/tmp/giga-eval-jail.* ]] || rm -rf "$jailtmp"
    jailtmp="$(mktemp -d /private/tmp/giga-eval-jail.XXXXXX)"
    echo "$jailtmp" >"$EVAL_OUT/instance.jailtmp"
    envs="$(sandbox_env "$jailtmp")"
    sandbox_env_ok "$envs" || exit 1
    sandbox_profile "$bin" "$jailtmp" "$profile"
    sandbox_launch "$profile" "$envs"
    sandbox_preflight "$bin" "$jailtmp" || exit 1
    jail=("${SANDBOX_LAUNCH[@]}")
  fi
  touch "$EVAL_OUT/instance.marker"
  # A fake-clock build stops at launch without its clock file. This one holds
  # still; scripts/scenarios/app.sh writes its own and moves it.
  local clock="$APP_DATA/fake-clock.json" now; now="$(date +%s)000"
  [[ -f "$clock" ]] || { mkdir -p "$APP_DATA"; printf '{"uptime_ns":0,"monotonic_ns":0,"wall_ms":%s,"boot_wall_ms":%s}\n' "$now" "$now" >"$clock"; }
  # The launch cwd becomes a MonoCode project; case 4 uses it as the scratch project.
  local scratch="$EVAL_HOME/$SCRATCH_NAME"
  if [[ ! -d "$scratch/.git" ]]; then
    mkdir -p "$scratch"
    git -C "$scratch" init -q -b main
    echo "# giga-eval scratch project" >"$scratch/README.md"
    git -C "$scratch" add README.md
    git -C "$scratch" -c user.name=giga-eval -c user.email=giga-eval@local commit -qm "scratch: initial"
  fi
  (cd "$scratch" && launch "$bin" ${jail[@]+"${jail[@]}"} >"$EVAL_OUT/instance.log" 2>&1 </dev/null) &
  local pid=$!
  disown "$pid" 2>/dev/null || true
  echo "$pid" >"$EVAL_OUT/instance.pid"
  if [[ "${GIGA_EVAL_SANDBOX:-}" == 1 ]]; then
    sandbox_watch "$pid" "$(realpath "$bin")" </dev/null >/dev/null 2>&1 &
    disown $! 2>/dev/null || true
  fi
  local i
  for i in $(seq 1 60); do
    kill -0 "$pid" 2>/dev/null || die "instance exited, see $EVAL_OUT/instance.log"
    if has_window "$pid"; then
      log "instance pid $pid has a window after ${i}s"
      sleep 3   # let the webview load and the app open its stores
      return 0
    fi
    sleep 1
  done
  die "no window after 60s"
}

cmd_lsof_check() {
  local pid; pid="$(target_pid instance)"; [[ -n "$pid" ]] || die "instance not running"
  local pids; pids="$(printf '%s\n' "$pid" $(descendants "$pid") | paste -sd, -)"
  local out="$EVAL_OUT/lsof-$(date +%Y%m%d-%H%M%S).txt"
  lsof -n -P -p "$pids" >"$out" 2>/dev/null || true
  local real_data real_ws temp_data temp_home
  real_data="$(grep -cF "$REAL_APP_DATA/" "$out" || true)"
  real_ws="$(grep -cE "$REAL_HOME/MonoCode(/| |\$)" "$out" || true)"
  temp_data="$(grep -cF "$APP_DATA/" "$out" || true)"
  temp_home="$(grep -cE "(/private)?$EVAL_HOME/" "$out" || true)"
  echo "lsof over pids $pids -> $out"
  echo "  open files under real $REAL_APP_DATA/: $real_data"
  echo "  open files under real $REAL_HOME/MonoCode: $real_ws"
  echo "  open files under temp $APP_DATA/ (positive control): $temp_data"
  echo "  open files under temp HOME $EVAL_HOME/: $temp_home"
  [[ -d "$APP_DATA" ]] && echo "  instance data dir exists: $APP_DATA ($(find "$APP_DATA" | wc -l | tr -d ' ') entries)"
  # Files the real app keeps open: who holds them. The instance's pids must not.
  local holders; holders="$(lsof -n -P -t +D "$REAL_APP_DATA" 2>/dev/null | sort -u | paste -sd, - || true)"
  echo "  pids holding files under real app data: ${holders:-none} ($(for h in ${holders//,/ }; do /bin/ps -o comm= -p "$h"; done | paste -sd, -))"
  local held_by_us=0 h
  for h in ${holders//,/ }; do [[ ",$pids," == *",$h,"* ]] && held_by_us=1; done
  local leaked_webkit=0 d
  for d in "${LEAK_DIRS[@]}"; do
    [[ -e "$d" ]] && { echo "  instance path under the real HOME: $d"; leaked_webkit=1; }
  done
  [[ "$leaked_webkit" == 0 ]] && echo "  no $EVAL_ID dir under the real HOME (${LEAK_DIRS[*]#"$REAL_HOME/"})"
  if [[ "$real_data" == 0 && "$real_ws" == 0 && "$held_by_us" == 0 && "$leaked_webkit" == 0 && "$temp_data" -gt 0 ]]; then
    echo "PASS isolation (no real app data, no real ~/MonoCode, $temp_data files under the temp app data)"
  else
    echo "FAIL isolation"; return 1
  fi
}

cmd_stop() {
  local pid; pid="$(target_pid instance)"
  if [[ -z "$pid" ]]; then log "instance not running"; rm -f "$EVAL_OUT/instance.pid"; return 0; fi
  local kids; kids="$(descendants "$pid")"
  kill -TERM "$pid" 2>/dev/null || true
  local i
  for i in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
  kill -0 "$pid" 2>/dev/null && kill -KILL "$pid" 2>/dev/null || true
  # Harness processes run in their own process groups and can outlive the app.
  local k
  for k in $kids; do kill -TERM "$k" 2>/dev/null || true; done
  sleep 1
  for k in $kids; do kill -KILL "$k" 2>/dev/null || true; done
  rm -f "$EVAL_OUT/instance.pid"
  local jailtmp; jailtmp="$(cat "$EVAL_OUT/instance.jailtmp" 2>/dev/null || true)"
  [[ "$jailtmp" != /private/tmp/giga-eval-jail.* ]] || rm -rf "$jailtmp"
  log "stopped pid $pid and $(echo "$kids" | grep -c . || true) descendants"
}

cmd_status() {
  local pid; pid="$(target_pid instance)"
  echo "app: $(cat "$EVAL_OUT/instance.app" 2>/dev/null || echo none)"
  echo "pid: ${pid:-not running}"
  echo "home: $EVAL_HOME"
}

case "${1:-}" in
  build) shift; cmd_build "$@" ;;
  home) shift; cmd_home "$@" ;;
  start) shift; cmd_start "$@" ;;
  lsof-check) cmd_lsof_check ;;
  stop) cmd_stop ;;
  status) cmd_status ;;
  scrub) sandbox_scrub "$APP_DATA/monocode.db" "${2:?usage: instance.sh scrub <model>}" ;;
  guard) sandbox_guard "${2:-$APP_DATA/monocode.db}" && log "safe: ${2:-$APP_DATA/monocode.db}" ;;
  denied) sandbox_denied "${2:?usage: instance.sh denied <since>}" ;;
  *) sed -n '2,26p' "$0"; exit 2 ;;
esac
