#!/bin/sh
set -eu
root=${0%/*}
case "$1" in
  run)
    printf '%s\n' "$@" > "$root/run.args"
    printf '%s\n' "$$" > "$root/run.pid"
    if [ -f "$root/failure" ]; then
      cat "$root/failure" >&2
      exit 1
    fi
    while IFS= read -r heartbeat; do
      printf '%s\n' "$heartbeat" >> "$root/heartbeats"
    done
    ;;
  inspect)
    touch "$root/inspected"
    [ -f "$root/ready" ] || exit 1
    printf '%s\n' '{"1080/tcp":[{"HostIp":"127.0.0.1","HostPort":"49152"}]}'
    ;;
  exec)
    touch "$root/healthchecked"
    [ -f "$root/ready" ]
    ;;
  rm)
    printf '%s\n' "$@" > "$root/remove.args"
    ;;
  *) exit 2 ;;
esac
