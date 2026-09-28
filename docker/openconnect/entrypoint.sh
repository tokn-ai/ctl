#!/bin/bash
set -euo pipefail

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

child_pids=()
cleanup() {
  trap - EXIT INT TERM
  rm -f /run/openconnect/vpn-ready
  if [ "${#child_pids[@]}" -eq 0 ]; then
    return
  fi
  kill -TERM "${child_pids[@]}" 2>/dev/null || :
  deadline=$((SECONDS + 3))
  while [ "$SECONDS" -lt "$deadline" ]; do
    children_running=false
    for pid in "${child_pids[@]}"; do
      if kill -0 "$pid" 2>/dev/null; then
        children_running=true
        break
      fi
    done
    [ "$children_running" = true ] || break
    sleep 0.1
  done
  for pid in "${child_pids[@]}"; do
    if kill -0 "$pid" 2>/dev/null; then
      kill -KILL "$pid" 2>/dev/null || :
    else
      wait "$pid" 2>/dev/null || :
    fi
  done
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# ctld owns this stream. Preserve it before OpenConnect receives its password.
exec 3<&0
entrypoint_pid=$$
(
  while IFS= read -r -t 15 heartbeat <&3; do
    [ "$heartbeat" = ping ] || break
  done
  printf '%s\n' 'ctld heartbeat ended; stopping the VPN and SOCKS5.' >&2
  kill -TERM "$entrypoint_pid" 2>/dev/null || :
) &
heartbeat_pid=$!
child_pids+=("$heartbeat_pid")

config_file=/run/secrets/openconnect.env
[ -r "$config_file" ] || fail "Mount the VPN .env file at $config_file."

# Parse literal values without sourcing shell code or exporting the password.
TARGET_IP=
VPN_URL=
VPN_USERNAME=
VPN_PASSWORD=
VPN_AUTH_METHOD=
while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in
    ''|'#'*) ;;
    TARGET_IP=*) TARGET_IP=${line#*=} ;;
    VPN_URL=*) VPN_URL=${line#*=} ;;
    VPN_USERNAME=*) VPN_USERNAME=${line#*=} ;;
    VPN_PASSWORD=*) VPN_PASSWORD=${line#*=} ;;
    VPN_AUTH_METHOD=*) VPN_AUTH_METHOD=${line#*=} ;;
    *) fail 'Unknown setting in .env; use VPN_URL, VPN_USERNAME, VPN_PASSWORD, VPN_AUTH_METHOD, or TARGET_IP.' ;;
  esac
done < "$config_file"
unset line

[ -n "$VPN_URL" ] || fail 'Set VPN_URL in .env.'
[ -n "$VPN_USERNAME" ] || fail 'Set VPN_USERNAME in .env.'
[ -n "$VPN_PASSWORD" ] || fail 'Set VPN_PASSWORD in .env.'
case "$VPN_URL" in
  https://*) ;;
  *://*) fail 'VPN_URL must use HTTPS.' ;;
  *) VPN_URL="https://$VPN_URL" ;;
esac
[ -c /dev/net/tun ] || fail 'Pass --device /dev/net/tun to the container.'

mkdir -p /run/openconnect
rm -f /run/openconnect/vpn-ready

printf '%s\n' "$VPN_PASSWORD" | openconnect \
  --protocol=array \
  --user="$VPN_USERNAME" \
  --form-entry="form:method=$VPN_AUTH_METHOD" \
  --passwd-on-stdin \
  --non-inter \
  --interface=vpn0 \
  --script=/usr/local/bin/vpn-network \
  "$VPN_URL" &
vpn_pid=$!
child_pids+=("$vpn_pid")
unset VPN_PASSWORD

attempt=0
while [ "$attempt" -lt 60 ]; do
  kill -0 "$vpn_pid" 2>/dev/null || fail 'OpenConnect exited before VPN setup completed.'
  [ ! -f /run/openconnect/vpn-ready ] || break
  attempt=$((attempt + 1))
  sleep 1
done
[ "$attempt" -lt 60 ] || fail 'VPN routes and DNS were not ready within 60 seconds.'

# Do not bind outgoing connections to an interface: use the kernel's routes.
microsocks -q -i 0.0.0.0 -p 1080 &
socks_pid=$!
child_pids+=("$socks_pid")

attempt=0
until /usr/local/bin/vpn-healthcheck; do
  kill -0 "$vpn_pid" 2>/dev/null || fail 'OpenConnect exited during proxy startup.'
  kill -0 "$socks_pid" 2>/dev/null || fail 'SOCKS5 proxy failed to start.'
  attempt=$((attempt + 1))
  [ "$attempt" -lt 5 ] || fail 'SOCKS5 listener was not ready within 5 seconds.'
  sleep 1
done
printf '%s\n' 'SOCKS5 ready on container port 1080; outgoing traffic follows the container routes.'

if [ -n "$TARGET_IP" ]; then
  (
    if ! /usr/local/bin/ssh-handshake "$TARGET_IP"; then
      printf '%s\n' 'Optional SSH connectivity check failed; VPN and SOCKS5 remain available.' >&2
    fi
  ) &
  child_pids+=("$!")
else
  printf '%s\n' 'TARGET_IP omitted; skipping the optional SSH connectivity check.'
fi

service_status=0
wait -n "$vpn_pid" "$socks_pid" "$heartbeat_pid" || service_status=$?
printf '%s\n' 'VPN, SOCKS5, or heartbeat monitor exited; stopping the container.' >&2
[ "$service_status" -ne 0 ] || service_status=1
exit "$service_status"
