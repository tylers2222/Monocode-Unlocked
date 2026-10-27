# Sourced by the giga-eval scripts. Paths and helpers shared by every script.
set -euo pipefail

EVAL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EVAL_OUT="${GIGA_EVAL_OUT:-/tmp/giga-eval}"
EVAL_HOME="${GIGA_EVAL_HOME:-/tmp/giga-eval-home}"
EVAL_ID="com.monocode.desktop.gigaeval"
EVAL_PRODUCT="MonoCode GigaEval"
INSTALLED_APP="/Applications/MonoCode.app"
# The instance's data folder is Tauri's app_data_dir() under HOME=$EVAL_HOME.
case "$(uname -s)" in
  Darwin)
    EVAL_OS=macos
    REAL_HOME="$(dscl . -read "/Users/$(id -un)" NFSHomeDirectory | awk '{print $2}')"
    REAL_APP_DATA="$REAL_HOME/Library/Application Support/com.monocode.desktop"
    EVAL_APP_DATA="$EVAL_HOME/Library/Application Support/$EVAL_ID" ;;
  Linux)
    EVAL_OS=linux
    REAL_HOME="$(getent passwd "$(id -un)" | cut -d: -f6)"
    REAL_APP_DATA="${XDG_DATA_HOME:-$REAL_HOME/.local/share}/com.monocode.desktop"
    EVAL_APP_DATA="$EVAL_HOME/.local/share/$EVAL_ID" ;;
  *) echo "giga-eval runs on macOS or Linux, not $(uname -s)" >&2; exit 2 ;;
esac
SCRATCH_NAME="giga-eval-scratch"

# Prints `path` as a write to it would resolve: links followed, dangling ones
# too, then its deepest existing folder through `cd -P` (symlinks and `..`),
# plus the part not made yet. Fails on a link loop.
resolve() {
  local path="$1" tail="" link hops=0
  while [[ ! -d "$path" ]]; do
    if [[ -L "$path" ]]; then
      ((++hops <= 40)) || return 1
      link="$(readlink "$path")"
      [[ "$link" == /* ]] || link="$(dirname "$path")/$link"
      path="$link"
    else
      tail="/$(basename "$path")$tail"
      path="$(dirname "$path")"
    fi
  done
  printf '%s\n' "$(cd -P "$path" && pwd)$tail"
}

# Prints `path` resolved when that is under /tmp, else fails. The part not
# made yet may hold no `..` because `cd -P` cannot see it. A relative path
# fails: the scripts `cd` before they use it.
under_tmp() {
  [[ "$1" == /* ]] || return 1
  local path; path="$(resolve "$1")" || return 1
  [[ "$path/" != *"/../"* ]] || return 1
  case "$path" in
    /tmp/?*|/private/tmp/?*) printf '%s\n' "$path" ;;
    *) return 1 ;;
  esac
}

# Before any write: every GigaEval script writes only under these two.
for dir in "$EVAL_OUT" "$EVAL_HOME"; do
  under_tmp "$dir" >/dev/null \
    || { echo "GIGA_EVAL_OUT and GIGA_EVAL_HOME must be absolute paths that resolve under /tmp, got $dir" >&2; exit 2; }
done
mkdir -p "$EVAL_OUT"

log() { printf '[giga-eval %s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
die() { log "ERROR: $*"; exit 2; }

# Target = which app and which HOME a case runs against.
#   instance  -> the isolated GigaEval build, HOME=$EVAL_HOME
#   installed -> /Applications/MonoCode.app, real HOME
target_home() {
  case "$1" in
    instance) echo "$EVAL_HOME" ;;
    installed) echo "$REAL_HOME" ;;
    *) die "unknown target: $1 (instance|installed)" ;;
  esac
}
# PID of the GUI process for a target; empty when not running.
target_pid() {
  case "$1" in
    instance)
      local f="$EVAL_OUT/instance.pid"
      [[ -f "$f" ]] && kill -0 "$(cat "$f")" 2>/dev/null && cat "$f" || true ;;
    installed) /bin/ps -axo pid=,command= | awk -v b="$INSTALLED_APP/Contents/MacOS/monocode" '$2 == b {print $1; exit}' ;;
  esac
}
# The built app in $EVAL_OUT/instance.app is a .app bundle on macOS and the
# executable itself on Linux (tauri build --no-bundle).
app_bin() {
  if [[ "$EVAL_OS" == macos ]]; then echo "$1/Contents/MacOS/monocode"; else echo "$1"; fi
}
target_bin() {
  case "$1" in
    instance) local app; app="$(cat "$EVAL_OUT/instance.app" 2>/dev/null)" && app_bin "$app" || true ;;
    installed) echo "$INSTALLED_APP/Contents/MacOS/monocode" ;;
  esac
}

# Case results: one TSV row per check, read by report.sh.
#   run <TAB> case <TAB> check <TAB> PASS|FAIL|INFO <TAB> evidence
RESULTS="$EVAL_OUT/results.tsv"
RUN_ID="${RUN_ID:-adhoc}"
record() {
  local case_id="$1" check="$2" verdict="$3" evidence="$4"
  printf '%s\t%s\t%s\t%s\t%s\n' "$RUN_ID" "$case_id" "$check" "$verdict" "${evidence//$'\t'/ }" >>"$RESULTS"
  printf '%-4s case %s  %s  (%s)\n' "$verdict" "$case_id" "$check" "$evidence"
}
# check <case> <name> <evidence> <command...>: PASS when the command exits 0.
# Always returns 0 so one failed check does not stop the case.
check() {
  local case_id="$1" name="$2" evidence="$3"; shift 3
  if "$@"; then record "$case_id" "$name" PASS "$evidence"; else record "$case_id" "$name" FAIL "$evidence"; fi
}

# All descendant pids of a pid, from one ps snapshot.
descendants() {
  /bin/ps -axo pid=,ppid= | awk -v root="$1" '
    { parent[$1] = $2; pids[NR] = $1 }
    END {
      for (i = 1; i <= NR; i++) {
        p = pids[i]; q = parent[p]
        while (q != "" && q != 0 && q != 1) { if (q == root) { print p; break } q = parent[q] }
      }
    }'
}
