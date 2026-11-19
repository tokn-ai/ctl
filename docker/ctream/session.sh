#!/bin/bash
set -euo pipefail

export PATH="$CARGO_TARGET_DIR/debug:$PATH"
export CTREAM_TEST_X11=:98 DISPLAY=:98 XDG_RUNTIME_DIR=/run/user/1000
logs=$(mktemp -d)
pids=()
cleanup() {
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
}
trap cleanup EXIT

Xvfb "$DISPLAY" -screen 0 1280x720x24 -nolisten tcp -ac +extension Composite >"$logs/xvfb" 2>&1 &
pids+=("$!")
for attempt in {1..100}; do
  xdpyinfo >/dev/null 2>&1 && break
  sleep .1
done
xdpyinfo >/dev/null 2>&1 || { cat "$logs/xvfb"; exit 1; }
openbox >"$logs/openbox" 2>&1 &
pids+=("$!")
xmessage -title 'Ctream X11 fixture' -geometry 480x260+100+100 'Ctream fixture' >"$logs/xmessage" 2>&1 &
pids+=("$!")
for attempt in {1..100}; do
  xdotool search --onlyvisible --name '^Ctream X11 fixture$' >/dev/null 2>&1 && break
  sleep .1
done
xdotool search --onlyvisible --name '^Ctream X11 fixture$' >/dev/null 2>&1 || {
  cat "$logs/xmessage"
  exit 1
}

WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_HEADLESS_OUTPUTS=1 \
  sway --unsupported-gpu -c /workspace/docker/ctream/sway.conf >"$logs/sway" 2>&1 &
pids+=("$!")
for attempt in {1..100}; do
  for socket in "$XDG_RUNTIME_DIR"/wayland-*; do
    if test -S "$socket"; then export WAYLAND_DISPLAY="${socket##*/}"; break 2; fi
  done
  sleep .1
done
test -S "$XDG_RUNTIME_DIR/${WAYLAND_DISPLAY:-missing}" || { cat "$logs/sway"; exit 1; }

export GDK_BACKEND=wayland GTK_MODULES=gail:atk-bridge NO_AT_BRIDGE=0
python3 /workspace/docker/ctream/window.py >"$logs/gtk" 2>&1 &
pids+=("$!")
window_ready=false
for attempt in {1..100}; do
  if ctream windows "$WAYLAND_DISPLAY" --json | python3 -c \
    'import json, sys; sys.exit(not any(w["title"] == "Ctream Wayland fixture" for w in json.load(sys.stdin)))'; then
    window_ready=true
    break
  fi
  sleep .1
done
if [[ "$window_ready" != true ]]; then
  cat "$logs/sway" "$logs/gtk"
  exit 1
fi
timeout 90 "$1" --ignored --nocapture --test-threads=1 || {
  cat "$logs/sway" "$logs/gtk"
  exit 1
}
