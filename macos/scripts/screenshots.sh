#!/usr/bin/env bash
# Real screenshots of the window and the menu bar panel for the README, taken
# from `--demo --capture` (a staged track and a synthetic groove, no Spotify).
# Liquid Glass is composited live, so this captures the screen: it needs
# Screen Recording permission for your terminal, and each surface floats on
# top for a moment. The room light images come from `make stills`.
set -euo pipefail
cd "$(dirname "$0")/.."

[ -x build/muse-box.app/Contents/MacOS/muse-box ] || ./scripts/build-app.sh >/dev/null
defaults delete com.atharvakanherkar.musebox.demo >/dev/null 2>&1 || true
mkdir -p docs

jpeg() { sips -s format jpeg -s formatOptions 88 "$1" --out "$2" >/dev/null && rm "$1" && echo "$2"; }

build/muse-box.app/Contents/MacOS/muse-box --demo --capture | while read -r kind id; do
  case "$kind" in
    window) screencapture -l"$id" -o -x build/window.png && jpeg build/window.png docs/player.jpg ;;
    panel) screencapture -l"$id" -o -x build/panel.png && jpeg build/panel.png docs/menubar.jpg ;;
  esac
done
