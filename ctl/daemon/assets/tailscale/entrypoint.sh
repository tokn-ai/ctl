#!/bin/sh
set -eu
umask 077

# Every interested daemon renews the same container heartbeat through exec.
# Persistent node identity lives only in /state.
daemon_pid=
login_pid=
heartbeat_pid=
cleanup() {
  trap - EXIT INT TERM
  for pid in "$login_pid" "$daemon_pid" "$heartbeat_pid"; do
    [ -z "$pid" ] || kill -TERM "$pid" 2>/dev/null || :
  done
  # Let tailscaled finish state writes, but keep shutdown bounded. The container
  # init reaps children. Never logout or delete persistent state.
  attempt=0
  while [ -n "$daemon_pid" ] && kill -0 "$daemon_pid" 2>/dev/null && [ "$attempt" -lt 20 ]; do
    attempt=$((attempt + 1))
    sleep 0.1
  done
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
/bin/sh /run/ctl/watchdog.sh "$$" &
heartbeat_pid=$!

mkdir -p /state /run/tailscale
chmod 700 /state
tailscaled --tun=userspace-networking --statedir=/state --state=/state/tailscaled.state \
  --socket=/run/tailscale/tailscaled.sock --socks5-server=0.0.0.0:1080 &
daemon_pid=$!
while [ ! -S /run/tailscale/tailscaled.sock ]; do
  kill -0 "$daemon_pid" 2>/dev/null || exit 1
  kill -0 "$heartbeat_pid" 2>/dev/null || exit 1
  sleep 0.2
done

start_login() {
  # No auth key, forced reauthentication, routes advertised, or exit node.
  # --reset keeps the profile's two supported preferences authoritative.
  tailscale --socket=/run/tailscale/tailscaled.sock up --reset \
    --accept-dns=true --accept-routes="$CTLD_ACCEPT_ROUTES" \
    --hostname="$CTLD_HOSTNAME" >/dev/null 2>&1 &
  login_pid=$!
}
start_login
while kill -0 "$daemon_pid" 2>/dev/null; do
  # The VPN must never survive without its independently expiring watchdog.
  kill -0 "$heartbeat_pid" 2>/dev/null || exit 1
  if ! kill -0 "$login_pid" 2>/dev/null; then
    wait "$login_pid" || :
    # An expired identity needs a new interactive URL while retaining ownership.
    # Raw status/log contents never leave this container through stdout.
    state=$(tailscale --socket=/run/tailscale/tailscaled.sock status --json --peers=false 2>/dev/null) || :
    if printf '%s' "$state" | grep -Eq '"BackendState"[[:space:]]*:[[:space:]]*"(NeedsLogin|Stopped)"'; then
      start_login
    fi
  fi
  sleep 2
done
exit 1
