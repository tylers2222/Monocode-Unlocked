#!/usr/bin/env bash
# Download one omp release asset for CI, retrying a transient failure.
#
#   scripts/fetch-omp.sh <tag> <asset> <dest>
#
# Anonymous release downloads (curl on github.com/.../releases/download) returned 504
# for hours: tyler/main run 42580086493, job 129036771058 (browser 2/6) failed after
# curl's 4 tries in 90 s. The cache in front of this step is the main fix; on a miss,
# this spreads OMP_TRIES tries (default 6) OMP_RETRY_DELAY seconds apart (default 60),
# about 5 minutes, so a CDN outage that short no longer fails setup. gh uses GH_TOKEN,
# but its asset download ends on the same release CDN, so the token is not the fix.
# A 5xx, a 429 or a network error is retried. Another 4xx, or gh's "release not found"
# or "no assets match", fails at once: a wrong tag or asset name never heals.
# The caller checks the sha256. Runs on bash 3.2 (macOS runner). No -e: the loop reads
# each gh exit status itself.
set -uo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: fetch-omp.sh <tag> <asset> <dest>" >&2
  exit 2
fi
tag=$1
asset=$2
dest=$3
tries=${OMP_TRIES:-6}
delay=${OMP_RETRY_DELAY:-60}
err=$(mktemp) || exit 1
trap 'rm -f "$err"' EXIT

i=1
while :; do
  echo "omp: download $asset $tag, try $i of $tries"
  if gh release download "$tag" -R can1357/oh-my-pi -p "$asset" -O "$dest" --clobber 2>"$err"; then
    echo "omp: downloaded on try $i"
    exit 0
  fi
  cat "$err" >&2
  if grep -Eq 'release not found|no assets match' "$err" ||
    { grep -Eq 'HTTP 4[0-9][0-9]' "$err" && ! grep -q 'HTTP 429' "$err"; }; then
    echo "omp: client error, not retried" >&2
    exit 1
  fi
  if [ "$i" -ge "$tries" ]; then
    echo "omp: still failing after $tries tries" >&2
    exit 1
  fi
  i=$((i + 1))
  sleep "$delay"
done
