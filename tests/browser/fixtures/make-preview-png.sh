#!/usr/bin/env bash
# Writes preview.png, the image tab next to the video in video-preview.spec.ts.
set -euo pipefail
cd "$(dirname "$0")"
ffmpeg -hide_banner -loglevel error -y -f lavfi -i color=c=teal:size=32x32 -frames:v 1 -fflags +bitexact preview.png
