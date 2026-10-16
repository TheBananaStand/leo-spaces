#!/usr/bin/env bash
#
# Render assets/icon.svg to the PNG a hub can be handed.
#
#   bash scripts/render-icon.sh
#
# The hub rasterizes an uploaded SVG itself (rsvg-convert) and generates the
# whole rendition ladder — 16 through 1024 — from one square source, so this
# PNG is a convenience rather than a requirement: it is what to drag into
# Settings on a box that has no rasterizer, and what GitHub shows.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."

command -v rsvg-convert >/dev/null \
  || { echo "rsvg-convert not found (pacman -S librsvg / apt install librsvg2-bin)" >&2; exit 1; }

rsvg-convert -w 1024 -h 1024 assets/icon.svg -o assets/icon.png
echo "assets/icon.png — $(identify -format '%wx%h' assets/icon.png 2>/dev/null || echo 1024x1024)"
