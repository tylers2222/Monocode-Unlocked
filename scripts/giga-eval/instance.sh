#!/usr/bin/env bash
# Build, launch, check and stop an isolated "MonoCode GigaEval" release instance.
#
#   instance.sh build <checkout> [label]   copy the checkout's working tree, build a release .app
#                                          with cargo features $GIGA_EVAL_FEATURES (default fake-clock)
#   instance.sh home [--fresh]             create the temp HOME (symlinks to real logins, no copies)
#   instance.sh start [--fresh-home]       launch the last built .app under the temp HOME
#   instance.sh lsof-check                 prove no open file under the real app data or ~/MonoCode
#   instance.sh stop                       quit the instance and every process it started
#   instance.sh status
source "$(dirname "$0")/lib.sh"

SRC_ROOT="$EVAL_OUT/src"
TARGET_DIR="$EVAL_OUT/target"
APP_DATA="$EVAL_HOME/Library/Application Support/$EVAL_ID"
# Logins the harnesses read. Symlinked, never copied.
LINKS=(.omp .claude .claude.json .codex .config .gitconfig .ssh Library/Keychains)

guard_home() {
  case "$EVAL_HOME" in
    /tmp/?*|/private/tmp/?*) ;;
    *) die "GIGA_EVAL_HOME must be under /tmp, got $EVAL_HOME" ;;
  esac
  [[ "$EVAL_HOME" != "$REAL_HOME" ]] || die "temp HOME equals the real HOME"
}

cmd_build() {
  local checkout="${1:?checkout path}" label="${2:-$(basename "$1")}"
  checkout="$(cd "$checkout" && pwd)"
  local src="$SRC_ROOT/$label" logf="$EVAL_OUT/build-$label.log"
  mkdir -p "$src" "$TARGET_DIR"
  log "copying $checkout ($(git -C "$checkout" rev-parse --short HEAD), $(git -C "$checkout" status --porcelain | wc -l | tr -d ' ') dirty paths) -> $src"
  # Build from a copy so this script never writes into the checkout under test.
  rsync -a --delete --exclude node_modules --exclude target --exclude dist --exclude .git \
    "$checkout/" "$src/"
  git -C "$checkout" rev-parse HEAD >"$src/.giga-eval-rev"
  local override="$src/tauri.gigaeval.json"
  printf '%s\n' "{\"productName\":\"$EVAL_PRODUCT\",\"identifier\":\"$EVAL_ID\",\"bundle\":{\"createUpdaterArtifacts\":false,\"macOS\":{\"signingIdentity\":\"-\",\"hardenedRuntime\":false}}}" >"$override"
  local lock_hash; lock_hash="$(shasum -a 256 "$src/package-lock.json" | cut -c1-16)"
  if [[ ! -f "$src/node_modules/.giga-eval-lock" || "$(cat "$src/node_modules/.giga-eval-lock")" != "$lock_hash" ]]; then
    log "npm ci ($logf)"
    (cd "$src" && npm ci --no-audit --no-fund) >"$logf" 2>&1 || die "npm ci failed, see $logf"
    echo "$lock_hash" >"$src/node_modules/.giga-eval-lock"
  fi
  local features="${GIGA_EVAL_FEATURES-fake-clock}"
  log "tauri build, features '$features' ($logf)"
  (cd "$src" && CARGO_TARGET_DIR="$TARGET_DIR" npx tauri build --bundles app --config tauri.gigaeval.json \
    ${features:+--features "$features"}) >>"$logf" 2>&1 \
    || die "tauri build failed, see $logf"
  local built="$TARGET_DIR/release/bundle/macos/$EVAL_PRODUCT.app" dest="$EVAL_OUT/apps/$label/$EVAL_PRODUCT.app"
  [[ -d "$built" ]] || die "no bundle at $built"
  mkdir -p "$(dirname "$dest")"; rm -rf "$dest"; cp -a "$built" "$dest"
  echo "$dest" >"$EVAL_OUT/instance.app"
  local bid; bid="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$dest/Contents/Info.plist")"
  [[ "$bid" == "$EVAL_ID" ]] || die "bundle identifier is $bid, expected $EVAL_ID"
  local sig; sig="$(codesign -dv "$dest" 2>&1)"
  [[ "$sig" == *"Signature=adhoc"* ]] || die "bundle is not ad-hoc signed"
  log "built $dest (identifier $bid, ad-hoc, rev $(cut -c1-10 "$src/.giga-eval-rev"))"
}

cmd_home() {
  guard_home
  if [[ "${1:-}" == "--fresh" ]]; then
    [[ -z "$(target_pid instance)" ]] || die "stop the instance before --fresh"
    rm -rf "$EVAL_HOME"   # rm -rf removes symlinks, never their targets
  fi
  mkdir -p "$EVAL_HOME/Library"
  local rel
  for rel in "${LINKS[@]}"; do
    [[ -e "$REAL_HOME/$rel" ]] || continue
    if [[ ! -L "$EVAL_HOME/$rel" ]]; then
      [[ -e "$EVAL_HOME/$rel" ]] && die "$EVAL_HOME/$rel exists and is not a symlink"
      ln -s "$REAL_HOME/$rel" "$EVAL_HOME/$rel"
    fi
  done
  log "temp HOME $EVAL_HOME ready ($(find "$EVAL_HOME" -maxdepth 2 -type l | wc -l | tr -d ' ') login symlinks)"
}


cmd_start() {
  [[ -z "$(target_pid instance)" ]] || die "instance already running (pid $(target_pid instance))"
  if [[ "${1:-}" == "--fresh-home" ]]; then cmd_home --fresh; else cmd_home; fi
  local app; app="$(cat "$EVAL_OUT/instance.app" 2>/dev/null)" || die "build first"
  local bin="$app/Contents/MacOS/monocode"
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
  # Opened as a plain child, not through LaunchServices, so HOME and the
  # persistence flags reach the process.
  (cd "$scratch" && HOME="$EVAL_HOME" CFFIXED_USER_HOME="$EVAL_HOME" PATH="$PATH" \
    exec nohup "$bin" -ApplePersistence NO -NSQuitAlwaysKeepsWindows NO \
    >"$EVAL_OUT/instance.log" 2>&1 </dev/null) &
  local pid=$!
  disown "$pid" 2>/dev/null || true
  echo "$pid" >"$EVAL_OUT/instance.pid"
  local i
  for i in $(seq 1 60); do
    kill -0 "$pid" 2>/dev/null || die "instance exited, see $EVAL_OUT/instance.log"
    if osascript -l JavaScript "$EVAL_DIR/ax.js" windowid "$pid" >/dev/null 2>&1; then
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
  for d in "$REAL_HOME/Library/WebKit/$EVAL_ID" "$REAL_HOME/Library/Application Support/$EVAL_ID" "$REAL_HOME/Library/Caches/$EVAL_ID"; do
    [[ -e "$d" ]] && { echo "  instance path under the real HOME: $d"; leaked_webkit=1; }
  done
  [[ "$leaked_webkit" == 0 ]] && echo "  no $EVAL_ID dir under the real HOME (WebKit, Application Support, Caches)"
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
  *) sed -n '2,9p' "$0"; exit 2 ;;
esac
