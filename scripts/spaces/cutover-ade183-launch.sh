#!/bin/bash
# Start cutover-ade183.sh DETACHED as a one-shot launchd job (ADE-183). Quitting MonoCode 2 ends
# every process the app started, nohup included; a launchd job survives that.
#
#   cutover-ade183-launch.sh LABEL=<com.tyler.cutover-ade183...> RUN_ROOT=<dir> HOME_DIR=<dir> \
#     APP1=<.app> ID1=<bundle id> DATA1=<dir> WEBKIT1=<dir> APP2=<.app> ID2=<bundle id> DATA2=<dir> \
#     WEBKIT2=<dir> CLAUDE2=<dir> [CODEX2=<dir>] NEW_APP=<.app> PYTHON=<python3 >= 3.10> REAL=<0|1> \
#     [POLL=<s>] [FAIL_AT=<step>]
#
# Every value is required (no default points at real data). It makes RUN_ROOT/<ts>/, copies the job
# script and scripts/spaces/merge-*.py there (launchd cannot read ~/Desktop: TCC), writes
# config.sh + the plist (~/Library/LaunchAgents/<LABEL>.plist) and bootstraps it into gui/<uid>.
# The job's env is only HOME USER LOGNAME SHELL PATH (EnvironmentVariables), so no agent var
# reaches it or the apps it opens. It waits until both apps exit; it never quits them.
# Prints the run folder.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
KEYS=(LABEL RUN_ROOT HOME_DIR APP1 ID1 DATA1 WEBKIT1 APP2 ID2 DATA2 WEBKIT2 CLAUDE2 NEW_APP PYTHON REAL)
OPTIONAL=(CODEX2 POLL FAIL_AT UP_TIMEOUT QUIT_TIMEOUT)
for kv in "$@"; do
  k=${kv%%=*}
  [[ " ${KEYS[*]} ${OPTIONAL[*]} " == *" $k "* ]] || { echo "unknown key: $k" >&2; exit 2; }
  printf -v "$k" '%s' "${kv#*=}"
done
for k in "${KEYS[@]}"; do [ -n "${!k:-}" ] || { echo "missing $k=" >&2; exit 2; }; done
case "$LABEL" in com.tyler.cutover-ade183*) ;; *) echo "LABEL must start with com.tyler.cutover-ade183" >&2; exit 2;; esac
launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1 && { echo "$LABEL is already loaded" >&2; exit 1; }

RUN="$RUN_ROOT/$(date +%Y%m%d-%H%M%S)"
# The importer takes the folder two levels up as the repo (and refuses outputs inside it): keep that shape.
TOOLS="$RUN/tools/scripts/spaces"
mkdir -p "$TOOLS"
cp "$HERE/cutover-ade183.sh" "$RUN/"
cp "$REPO/scripts/spaces/merge-import.py" "$REPO/scripts/spaces/merge-dry-run.py" "$TOOLS/"
{
  echo "# written by cutover-ade183-launch.sh $(date) from $(git -C "$REPO" rev-parse HEAD 2>/dev/null)"
  for k in RUN TOOLS "${KEYS[@]}" "${OPTIONAL[@]}"; do
    case $k in RUN) v=$RUN;; TOOLS) v=$TOOLS;; *) v=${!k:-};; esac
    printf '%s=%q\n' "$k" "$v"
  done
} > "$RUN/config.sh"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
mkdir -p "$(dirname "$PLIST")"
cat > "$PLIST" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>$LABEL</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>$RUN/cutover-ade183.sh</string><string>$RUN/config.sh</string></array>
<key>EnvironmentVariables</key><dict>
<key>HOME</key><string>$HOME</string><key>USER</key><string>$(id -un)</string><key>LOGNAME</key><string>$(id -un)</string>
<key>SHELL</key><string>/bin/zsh</string><key>PATH</key><string>/usr/bin:/bin:/usr/sbin:/sbin</string>
</dict>
<key>RunAtLoad</key><true/>
<key>AbandonProcessGroup</key><true/>
<key>StandardOutPath</key><string>$RUN/launchd.out</string>
<key>StandardErrorPath</key><string>$RUN/launchd.out</string>
</dict></plist>
PL
plutil -lint "$PLIST" >/dev/null
launchctl bootstrap "gui/$(id -u)" "$PLIST"
echo "$RUN"
