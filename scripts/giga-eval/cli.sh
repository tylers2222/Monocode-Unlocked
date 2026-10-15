#!/usr/bin/env bash
# Run the target's own `monocode app ...` CLI as one Mono.
#
#   cli.sh <mono name|id> <action> [json] [--request-id ID]
#   GIGA_EVAL_TARGET=instance|installed   (default instance)
#
# The credentials come from the environment of that Mono's agent process
# (MONOCODE_APP_ENDPOINT, MONOCODE_APP_TOKEN). They go to a 0600 temp file,
# reach the CLI through its environment only, and are never printed.
# The app accepts the token only while that Mono runs a turn; hold one first
# with `ui.sh hold <mono>`. Exit 3 means no Mono process has an active turn.
source "$(dirname "$0")/lib.sh"

TARGET="${GIGA_EVAL_TARGET:-instance}"
mono="${1:?mono name or id}"; action="${2:?action}"; json="${3:-}"; shift $(( $# < 3 ? $# : 3 ))
[[ -n "$json" ]] || json='{}'
extra=("$@")

pid="$(target_pid "$TARGET")"; [[ -n "$pid" ]] || die "$TARGET is not running"
bin="$(target_bin "$TARGET")"
creds_dir="$(mktemp -d "${TMPDIR:-/tmp}/giga-eval-cli.XXXXXX")"
chmod 700 "$creds_dir"
trap 'rm -rf "$creds_dir"' EXIT


# Runs the CLI with one credentials file. Stdout is the CLI's JSON line.
run_with() {
  local file="$1"; shift
  (
    set -a; source "$file"; set +a
    "$bin" app "$@"
  )
}

# A cached pid for this Mono goes first; it was proven by `self` earlier.
cache="$EVAL_OUT/cli-pid-$TARGET-$(printf '%s' "$mono" | shasum | cut -c1-12)"
candidates="$( { [[ -f "$cache" ]] && cat "$cache"; descendants "$pid"; } | awk '!seen[$0]++')"
inactive=0
for p in $candidates; do
  file="$creds_dir/$p.env"
  ( umask 077; python3 "$EVAL_DIR/procenv.py" "$p" MONOCODE_APP_ENDPOINT MONOCODE_APP_TOKEN >"$file" ) || continue
  grep -q '^MONOCODE_APP_TOKEN=' "$file" && grep -q '^MONOCODE_APP_ENDPOINT=' "$file" || continue
  listing="$(run_with "$file" monos.list 2>/dev/null || true)"
  if ! jq -e '.ok == true' >/dev/null 2>&1 <<<"$listing"; then inactive=$((inactive + 1)); continue; fi
  # The listing marks the caller with self:true; that is how a pid maps to a Mono.
  if jq -e --arg m "$mono" '[.. | objects | select(.self? == true and (.id == $m or .name == $m))] | length > 0' >/dev/null <<<"$listing"; then
    echo "$p" >"$cache"
    if [[ "$action" == "monos.list" && ${#extra[@]} -eq 0 ]]; then
      printf '%s\n' "$listing"; exit 0
    fi
    run_with "$file" "$action" --json "$json" ${extra[@]+"${extra[@]}"}
    exit $?
  fi
done
log "no active turn for Mono '$mono' among $(echo "$candidates" | grep -c . || true) processes ($inactive with an app token but no active turn)"
exit 3
