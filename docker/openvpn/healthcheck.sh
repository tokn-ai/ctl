#!/bin/sh
set -eu

IFS= read -r ready_since < /run/openvpn/ready
case "$ready_since" in
  ''|*[!0-9]*) exit 1 ;;
esac
IFS=' ' read -r uptime remainder < /proc/uptime
test "$((${uptime%%.*} - ready_since))" -ge 5
test -n "$(ip -o link show dev tun0 up)"
test -n "$(ip -o addr show dev tun0 scope global)"
test -n "$(ss -H -ltn 'sport = :1080')"
