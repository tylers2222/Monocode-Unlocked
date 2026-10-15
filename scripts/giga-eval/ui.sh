#!/usr/bin/env bash
# Drive the target's window with System Events (ax.js) and cliclick.
#
#   ui.sh shot <png>                     screenshot of the target's main window
#   ui.sh dump [depth]                   accessibility tree, for writing new steps
#   ui.sh create-mono <name>             "New mono", type the name in its settings, close
#   ui.sh open-mono <name>               select the Mono in the sidebar
#   ui.sh open-settings <name>           open the Mono, then its settings dialog
#   ui.sh giga-on <name>                 open settings, turn the "Giga Mono" switch on, verify, close
#   ui.sh giga-off <name>                same, off
#   ui.sh wait-idle <name> [seconds]     wait until the sidebar row reads idle
#   ui.sh count <name> <prefix> [role]   elements in the Mono's chat whose text starts with <prefix>
#   ui.sh send <name> <prompt>           type a prompt in the Mono's chat and submit it
#   ui.sh hold <name> [seconds]          start a turn that sleeps, so cli.sh can call the app as that Mono
#   GIGA_EVAL_TARGET=instance|installed  (default instance)
source "$(dirname "$0")/lib.sh"

TARGET="${GIGA_EVAL_TARGET:-instance}"
command -v cliclick >/dev/null || brew install cliclick
pid="$(target_pid "$TARGET")"; [[ -n "$pid" ]] || die "$TARGET is not running"

ax() { osascript -l JavaScript "$EVAL_DIR/ax.js" "$1" "$pid" "${@:2}"; }
# Every click and keystroke lands on the frontmost app, so each one first
# proves the target is frontmost; the installed MonoCode has the same process name.
front() { ax front >/dev/null || die "cannot bring pid $pid to the front"; }
# click <label> [role]: cliclick at the centre of the element's frame.
click() {
  local f; f="$(ax find "$@")" || die "no element labelled '$1'"
  front
  cliclick "c:$(jq -r '.cx' <<<"$f"),$(jq -r '.cy' <<<"$f")"
  sleep 0.6
}
# Keyboard goes through System Events: cliclick's key events drop the last
# character and the Return when they reach the webview back to back.
keystroke() {
  front
  osascript -e 'on run argv' -e 'tell application "System Events" to keystroke (item 1 of argv)' -e 'end run' "$1"
  sleep 0.2
}
key() {
  local using=""
  [[ -z "${2:-}" ]] || using=" using {$2}"
  front
  osascript -e "tell application \"System Events\" to key code $1$using"
  sleep 0.3
}
RETURN=36
type_into() {
  click "$1" "${3:-}"
  key 0 "command down"   # select all (key code 0 = a)
  keystroke "$2"
}
# wait_for <label> [role] [seconds]: poll until the element exists.
wait_for() {
  local i
  for (( i = 0; i < ${3:-15}; i++ )); do
    ax find "$1" "${2:-}" >/dev/null 2>&1 && return 0
    sleep 1
  done
  die "'$1' did not appear"
}
close_dialog() { key 53; sleep 0.4; }   # Escape

cmd_shot() {
  local out="${1:?png path}"; mkdir -p "$(dirname "$out")"
  front
  screencapture -x -o -l "$(ax windowid)" "$out"
  [[ -s "$out" ]] || die "screenshot is empty"
  echo "$out $(sips -g pixelWidth -g pixelHeight "$out" | awk '/pixel/{printf "%s ", $2}')"
}

cmd_create_mono() {
  local name="${1:?name}"
  front
  # A home with no Mono shows the "Meet Monos" intro instead of the section's "+" button.
  if ax find "New mono" AXButton >/dev/null 2>&1; then click "New mono" AXButton; else click "Create mono" AXButton; fi
  type_into "Name" "$name" AXTextField
  key $RETURN
  sleep 0.5
  close_dialog
  ax find "$name, *" AXButton >/dev/null || die "no sidebar row for Mono '$name' after create"
  echo "created Mono '$name'"
}

cmd_open_mono() {
  # An open Mono settings dialog is modal and hides the sidebar from AX.
  if ax find "Settings sections" >/dev/null 2>&1; then close_dialog; fi
  click "${1:?name}, *" AXButton
  wait_for "Message *" AXTextArea
}

cmd_open_settings() {
  local name="${1:?name}"
  cmd_open_mono "$name"
  click "$name settings" AXButton
  wait_for "Giga Mono" AXCheckBox
}

# giga_set <name> 1|0: flip the "Giga Mono" switch to the wanted state and read it back.
giga_set() {
  local name="$1" want="$2"
  cmd_open_settings "$name"
  local before; before="$(ax checked "Giga Mono" AXCheckBox)"
  [[ "$before" == "$want" ]] || click "Giga Mono" AXCheckBox
  local after; after="$(ax checked "Giga Mono" AXCheckBox)"
  close_dialog
  [[ "$after" == "$want" ]] || die "Giga Mono switch reads '$after', wanted '$want'"
  echo "Giga Mono switch for '$name': $before -> $after"
}
cmd_giga_on() {
  giga_set "${1:?name}" 1
  ax find "Giga Monos" AXGroup >/dev/null || die "no Giga Monos sidebar section"
}
cmd_giga_off() { giga_set "${1:?name}" 0; }

# The sidebar row reads "<name>, idle, ..." or "<name>, working, ...".
cmd_wait_idle() {
  local name="${1:?name}" timeout="${2:-300}" i
  for (( i = 0; i < timeout; i += 3 )); do
    ax find "$name, idle*" AXButton >/dev/null 2>&1 && { echo "'$name' idle after ${i}s"; return 0; }
    sleep 3
  done
  die "'$name' not idle after ${timeout}s"
}

# Elements in the Mono's open chat whose text starts with <prefix>.
cmd_count() {
  cmd_open_mono "${1:?name}"
  ax count "${2:?prefix}*" "${3:-}"
}

cmd_send() {
  local name="${1:?name}" prompt="${2:?prompt}"
  cmd_open_mono "$name"
  type_into "Message *" "$prompt" AXTextArea
  key $RETURN
  echo "sent to '$name': ${prompt:0:80}"
}

cmd_hold() {
  local name="${1:?name}" secs="${2:-60}"
  cmd_send "$name" "Run exactly this shell command and nothing else: sleep $secs. When it finishes, reply with the single word held."
}

case "${1:-}" in
  shot) shift; cmd_shot "$@" ;;
  dump) shift; ax dump "${1:-25}" ;;
  create-mono) shift; cmd_create_mono "$@" ;;
  open-mono) shift; cmd_open_mono "$@" ;;
  open-settings) shift; cmd_open_settings "$@" ;;
  giga-on) shift; cmd_giga_on "$@" ;;
  giga-off) shift; cmd_giga_off "$@" ;;
  wait-idle) shift; cmd_wait_idle "$@" ;;
  count) shift; cmd_count "$@" ;;
  send) shift; cmd_send "$@" ;;
  hold) shift; cmd_hold "$@" ;;
  *) sed -n '2,15p' "$0"; exit 2 ;;
esac
