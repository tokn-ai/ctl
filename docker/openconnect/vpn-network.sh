#!/bin/sh
set -eu

mkdir -p /run/openconnect
case "${reason:-}" in
  pre-init|connect|disconnect|attempt-reconnect)
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
    touch /run/openconnect/vpn-ready
    ;;
esac
