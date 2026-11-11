#!/usr/bin/env bash
set -euo pipefail

# Hosted runners include unrelated third-party repositories. CI can select the
# distribution's own source file without editing the machine's apt config.
APT_SOURCES=()
if [ "$#" -gt 0 ]; then
  if [ "$#" -ne 2 ] || [ "$1" != "--sources" ] || [ ! -f "$2" ]; then
    echo "Usage: $0 [--sources EXISTING_SOURCE_FILE]" >&2
    exit 1
  fi
  APT_SOURCES=(-o "Dir::Etc::sourcelist=$2" -o "Dir::Etc::sourceparts=-")
fi

if ! command -v apt-get >/dev/null 2>&1; then
  echo "This helper currently supports Ubuntu/Debian systems with apt-get." >&2
  exit 1
fi

if [ "$(id -u)" -eq 0 ]; then
  SUDO=()
elif command -v sudo >/dev/null 2>&1; then
  SUDO=(sudo)
else
  echo "sudo is required when not running as root." >&2
  exit 1
fi

# Bound mirror retries so a stalled download cannot consume the whole CI job.
APT_NETWORK=(-o Acquire::Retries=2 -o Acquire::http::Timeout=20 -o Acquire::https::Timeout=20)

# apt-get update only warns when it cannot resolve a mirror and exits 0, and the
# install then spends about 20 s per package on the dead name (PR #638's gate run
# hung 36 min). Stop at the first command whose output shows the distribution
# mirrors cannot be resolved. Third-party hosts are ignored: the install works
# without them. The error names DNS and apt so the runner-fault rerun rule applies.
# Each apt command also gets APT_COMMAND_SECONDS; CI steps add timeout-minutes on top.
MIRROR_DNS_FAULT="(Temporary failure resolving|Could not resolve) '[^']*(ubuntu\.com|debian\.org)'"
APT_COMMAND_SECONDS=300
TIMEOUT=()
if command -v timeout >/dev/null 2>&1; then TIMEOUT=(timeout "$APT_COMMAND_SECONDS"); fi
run_apt() {
  local log status=0
  log=$(mktemp)
  ${TIMEOUT[@]+"${TIMEOUT[@]}"} "$@" 2>&1 | tee "$log" || status=$?
  if grep -Eq "$MIRROR_DNS_FAULT" "$log"; then
    rm -f "$log"
    echo "::error::apt could not resolve the Ubuntu/Debian package mirrors (Temporary failure resolving). This is a runner DNS/network fault, not a code failure: rerun the job."
    exit 1
  fi
  rm -f "$log"
  if [ "$status" -eq 124 ]; then
    echo "::error::apt ran over ${APT_COMMAND_SECONDS} s, so the package mirrors are unreachable (runner DNS/network fault, not a code failure): rerun the job."
  fi
  return "$status"
}

# The conditional expansions also support empty arrays with nounset on Bash 3.
run_apt ${SUDO[@]+"${SUDO[@]}"} apt-get "${APT_NETWORK[@]}" ${APT_SOURCES[@]+"${APT_SOURCES[@]}"} update
run_apt ${SUDO[@]+"${SUDO[@]}"} env DEBIAN_FRONTEND=noninteractive apt-get "${APT_NETWORK[@]}" ${APT_SOURCES[@]+"${APT_SOURCES[@]}"} install -y \
  build-essential \
  curl \
  file \
  libayatana-appindicator3-dev \
  libgtk-3-dev \
  libssl-dev \
  libwebkit2gtk-4.1-dev \
  libxdo-dev \
  librsvg2-dev \
  patchelf \
  wget
