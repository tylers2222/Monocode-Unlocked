#!/usr/bin/env bash
# Host side of the make e2e targets (see `make help`).
#   e2e.sh start <suite> <rev> <cpus> <memory>   build the image for <rev>, start the run detached, print its id
#   e2e.sh status <run-id>                      running, or copy the results back and summarise them
#   e2e.sh logs <run-id>                        the run's output so far
# Every docker call follows DOCKER_CONTEXT, so a remote daemon needs no change
# here. A run remembers its context in e2e-results/<run-id>/run.env.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
RESULTS=e2e-results
IMAGE=monocode-e2e

die() { echo "e2e: $*" >&2; exit 2; }

start() {
  local suite=$1 rev cpus=$3 memory=$4
  bash docker/e2e-run.sh --check "$suite"
  rev=$(git rev-parse --verify "$2^{commit}")
  if [[ $2 == HEAD && -n "$(git status --porcelain)" ]]; then
    echo "e2e: uncommitted changes are not in the run; it uses commit ${rev:0:12}" >&2
  fi
  local tag="$IMAGE:${rev:0:12}" id
  git archive --format=tar "$rev" | docker build --file docker/e2e.Dockerfile --tag "$tag" - >&2
  id="$(date -u +%Y%m%d-%H%M%S)-${rev:0:8}-$suite"
  mkdir -p "$RESULTS/$id"
  printf 'context=%s\ncommit=%s\nsuite=%s\nimage=%s\n' "$(docker context show)" "$rev" "$suite" "$tag" >"$RESULTS/$id/run.env"
  docker run --detach --init --name "$IMAGE-$id" --cpus "$cpus" --memory "$memory" --shm-size 1g \
    --env GIGA_EVAL_REV="$rev" "$tag" "$suite" >/dev/null
  echo "$id"
}

open_run() {
  RUN=${1:?RUN=<run-id> is required}
  DIR="$RESULTS/$RUN"
  CONTAINER="$IMAGE-$RUN"
  [[ -f "$DIR/run.env" ]] || die "no run $RUN in $RESULTS/"
  export DOCKER_CONTEXT="${DOCKER_CONTEXT:-$(sed -n 's/^context=//p' "$DIR/run.env")}"
}

collect() {
  docker cp "$CONTAINER:/results/." "$DIR/"
  docker logs "$CONTAINER" >"$DIR/run.log" 2>&1
  [[ -f "$DIR/done" ]] || docker inspect --format '{{.State.ExitCode}}' "$CONTAINER" >"$DIR/done"
  docker rm "$CONTAINER" >/dev/null
}

status() {
  open_run "$1"
  if [[ ! -f "$DIR/done" ]]; then
    local state
    state=$(docker inspect --format '{{.State.Status}}' "$CONTAINER")
    if [[ $state != exited ]]; then
      echo "$RUN: $state on $DOCKER_CONTEXT"
      docker logs --tail 3 "$CONTAINER" 2>&1
      return
    fi
    collect
  fi
  echo "$RUN: done, exit $(cat "$DIR/done"), results in $DIR/"
  [[ -f "$DIR/results.tsv" ]] || return 0
  awk -F'\t' 'NR > 1 { n[$1 "\t" $3]++ } END { for (k in n) print k "\t" n[k] }' "$DIR/results.tsv" | sort
  awk -F'\t' '$3 == "failed" { print "FAILED  " $2 "  " $5 }' "$DIR/results.tsv"
}

logs() {
  open_run "$1"
  if [[ -f "$DIR/run.log" ]]; then cat "$DIR/run.log"; else docker logs "$CONTAINER"; fi
}

cmd=${1:-}
shift || true
case "$cmd" in
  start | status | logs) "$cmd" "$@" ;;
  *) sed -n '2,5p' "$0"; exit 2 ;;
esac
