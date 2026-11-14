#!/bin/sh
set -eu
umask 077

case "${script_type:-}" in
  route-pre-down)
    rm -f /run/openvpn/ready
    exit 0
    ;;
  route-up) ;;
  *) exit 1 ;;
esac

# OpenVPN exposes pushed DHCP options as environment variables on Linux.
# Interpret DNS addresses only. The SOCKS firewall enforces their tunnel route.
: > /run/openvpn/resolv.conf
index=1
while [ "$index" -le 64 ] && option=$(printenv "foreign_option_$index"); do
  case "$option" in
    'dhcp-option DNS '*|'dhcp-option DNS6 '*)
      address=${option##* }
      case "$address" in
        ''|*[!0-9a-fA-F.:]*) ;;
        *.*|*:*) printf 'nameserver %s\n' "$address" >> /run/openvpn/resolv.conf ;;
      esac
      ;;
  esac
  index=$((index + 1))
done
if [ -s /run/openvpn/resolv.conf ]; then
  if ! cat /run/openvpn/resolv.conf > /etc/resolv.conf; then
    : > /run/openvpn/dns-failed
    exit 1
  fi
fi

IFS=' ' read -r uptime remainder < /proc/uptime
printf '%s\n' "${uptime%%.*}" > /run/openvpn/ready.tmp
mv /run/openvpn/ready.tmp /run/openvpn/ready
