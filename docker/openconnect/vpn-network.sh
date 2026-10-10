#!/bin/sh
set -eu

mkdir -p /run/openconnect
case "${reason:-}" in
  pre-init|connect|reconnect|disconnect|attempt-reconnect)
    rm -f /run/openconnect/vpn-ready
    ;;
esac

/usr/share/vpnc-scripts/vpnc-script

case "${reason:-}" in
  connect|reconnect)
    if [ -z "$(ip -o link show dev "$TUNDEV" up)" ] \
      || [ -z "$(ip -o addr show dev "$TUNDEV" scope global)" ]; then
      printf '%s\n' 'VPN interface is not up with an assigned address.' >&2
      exit 1
    fi
    # A reconnect can succeed momentarily while the server keeps closing TLS.
    # Record monotonic time so health requires a sustained established tunnel.
    IFS=' ' read -r vpn_uptime remainder < /proc/uptime
    printf '%s\n' "${vpn_uptime%%.*}" > /run/openconnect/vpn-ready.tmp
    mv /run/openconnect/vpn-ready.tmp /run/openconnect/vpn-ready
    ;;
esac
