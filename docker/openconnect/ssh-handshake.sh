#!/bin/sh
set -eu

target_ip=${1:-}
if [ -z "$target_ip" ]; then
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      TARGET_IP=*) target_ip=${line#*=} ;;
    esac
  done < /run/secrets/openconnect.env
  unset line
fi
if [ -z "$target_ip" ]; then
  printf '%s\n' 'TARGET_IP omitted; skipping the optional SSH connectivity check.'
  exit 0
fi
case "$target_ip" in
  *[!0-9.]*|'')
    printf '%s\n' 'Supply an IPv4 target address.' >&2
    exit 1
    ;;
esac

route=$(ip -4 route get "$target_ip")
printf 'Connectivity check route: %s\n' "$route"

mkdir -p /run/openconnect
keys_file=$(mktemp /run/openconnect/ssh-host-keys.XXXXXX)
trap 'rm -f "$keys_file"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if ! ssh-keyscan -4 -T 10 -p 22 -t ed25519,ecdsa,rsa "$target_ip" > "$keys_file"; then
  printf 'SSH handshake failed for %s:22.\n' "$target_ip" >&2
  exit 1
fi
if ! [ -s "$keys_file" ]; then
  printf '%s\n' 'SSH server did not return a host key.' >&2
  exit 1
fi

printf 'SSH host-key handshake succeeded for %s:22.\n' "$target_ip"
ssh-keygen -lf "$keys_file"
mv "$keys_file" /run/openconnect/ssh_host_keys
