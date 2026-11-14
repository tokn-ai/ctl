#!/bin/bash
set -euo pipefail
umask 077

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

children=()
config=/run/secrets/client.ovpn
cleanup() {
  trap - EXIT INT TERM
  rm -f "$config" /run/openvpn/ready
  if [ "${#children[@]}" -gt 0 ]; then
    kill -TERM "${children[@]}" 2>/dev/null || :
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

[ "${CTLD_CONFIG_STDIN:-}" = 1 ] || fail 'VPN configuration was not received.'
payload=
IFS= read -r -t 15 -n 65537 payload || fail 'VPN configuration was not received.'
[ "${#payload}" -le 65536 ] || fail 'VPN configuration is too large.'
[ -n "$payload" ] || fail 'VPN configuration is empty.'
printf '%s' "$payload" | base64 --decode > "$config" || fail 'VPN configuration is invalid.'
unset payload
exec </dev/null

[ -c /dev/net/tun ] || fail 'This OpenVPN container requires a Linux TUN device.'

# The proxy's outgoing connections and DNS queries must leave through the
# tunnel. Replies to clients of the published SOCKS port still use the bridge.
# The OpenVPN daemon runs separately as root so it can reach its gateway.
nft -f - <<'RULES' || fail 'Could not isolate OpenVPN proxy traffic.'
table inet ctl_openvpn {
  chain output {
    type filter hook output priority filter; policy accept;
    meta skuid 65534 tcp sport 1080 ct direction reply accept
    meta skuid 65534 oifname "tun0" accept
    meta skuid 65534 reject
  }
}
RULES

/bin/sh /run/ctl/watchdog.sh "$$" &
watchdog_pid=$!
children+=("$watchdog_pid")

openvpn --pull-filter ignore setenv --pull-filter ignore verb --config "$config" \
  --dev tun0 --disable-dco --script-security 2 \
  --route-up /usr/local/bin/openvpn-network --route-pre-down /usr/local/bin/openvpn-network \
  --remap-usr1 SIGTERM --auth-retry none --verb 3 &
vpn_pid=$!
children+=("$vpn_pid")

# The repository-owned route hook signals successful negotiation and route
# setup. Do not accept proxy connections while the tunnel is starting.
while [ ! -f /run/openvpn/ready ]; do
  [ ! -f /run/openvpn/dns-failed ] || fail 'Could not install OpenVPN DNS settings.'
  kill -0 "$vpn_pid" 2>/dev/null || fail 'OpenVPN exited before connecting.'
  kill -0 "$watchdog_pid" 2>/dev/null || fail 'The VPN heartbeat watchdog stopped.'
  sleep 0.25
done

# Use the reserved nobody UID specifically for the firewall rules above.
setpriv --reuid 65534 --regid 65534 --clear-groups --no-new-privs \
  microsocks -q -i 0.0.0.0 -p 1080 &
proxy_pid=$!
children+=("$proxy_pid")

result=0
wait -n "$vpn_pid" "$proxy_pid" "$watchdog_pid" || result=$?
exit "$result"
