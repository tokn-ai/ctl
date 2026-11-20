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

wait_for() {
  local attempt
  for attempt in {1..100}; do
    if "$@" >/dev/null 2>&1; then return 0; fi
    sleep .1
  done
  return 1
}

window_manager_ready() {
  [[ $(xprop -root _NET_SUPPORTING_WM_CHECK) == *"window id # 0x"* ]]
}

listed_window_ready() {
  ctream windows "$1" --json | python3 -c \
    'import json, sys; sys.exit(not any(w["title"] == sys.argv[1] for w in json.load(sys.stdin)))' "$2"
}

Xvfb "$DISPLAY" -screen 0 1280x720x24 -nolisten tcp -ac +extension Composite >"$logs/xvfb" 2>&1 &
pids+=("$!")
wait_for xdpyinfo || { echo 'X11 display did not become ready' >&2; cat "$logs/xvfb"; exit 1; }
# Map the fixture before starting Openbox, avoiding a map request during the
# window manager's startup. Then wait for ctream to see the managed window.
xmessage -title 'Ctream X11 fixture' -geometry 480x260+100+100 'Ctream fixture' >"$logs/xmessage" 2>&1 &
pids+=("$!")
wait_for xdotool search --onlyvisible --name '^Ctream X11 fixture$' || {
  echo 'X11 fixture window did not become ready' >&2
  cat "$logs/xvfb" "$logs/xmessage"
  xwininfo -root -tree
  exit 1
}
openbox >"$logs/openbox" 2>&1 &
pids+=("$!")
if ! wait_for window_manager_ready || ! wait_for listed_window_ready "$DISPLAY" 'Ctream X11 fixture'; then
  echo 'X11 window manager did not become ready' >&2
  cat "$logs/openbox"
  xwininfo -root -tree
  ctream windows "$DISPLAY" --json
  exit 1
fi

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
if ! wait_for listed_window_ready "$WAYLAND_DISPLAY" 'Ctream Wayland fixture'; then
  echo 'Wayland fixture window did not become ready' >&2
  cat "$logs/sway" "$logs/gtk"
  exit 1
fi
timeout 90 "$1" --ignored --nocapture --test-threads=1 || {
  cat "$logs/sway" "$logs/gtk"
  exit 1
}
