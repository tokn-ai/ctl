#!/bin/sh
set -eu

test -f /run/openconnect/vpn-ready
IFS= read -r ready_since < /run/openconnect/vpn-ready
case "$ready_since" in
  ''|*[!0-9]*) exit 1 ;;
esac
IFS=' ' read -r vpn_uptime remainder < /proc/uptime
now=${vpn_uptime%%.*}
test "$((now - ready_since))" -ge 5
test -n "$(ip -o link show dev vpn0 up)"
test -n "$(ip -o addr show dev vpn0 scope global)"
test -n "$(ss -H -ltn 'sport = :1080')"
