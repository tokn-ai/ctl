#!/bin/sh
set -eu
root=$(dirname "$0")
case "$1" in
  run)
    printf '%s\n' "$@" > "$root/run.args"
    if [ -f "$root/late_create" ]; then
      for arg in "$@"; do
        case "$arg" in
          io.ctl.lease=*) printf '%s' "${arg#io.ctl.lease=}" > "$root/pending.lease" ;;
        esac
      done
      printf '%s' "$$" > "$root/run.pid"
      while IFS= read -r heartbeat; do
        printf '%s\n' "$heartbeat" >> "$root/heartbeats"
      done
      exit 0
    fi
    if [ -f "$root/separate_container" ]; then
      # Keep engine-client exit separate from actual container removal.
      [ ! -f "$root/container.running" ] || exit 1
      : > "$root/container.running"
    fi
    if [ -f "$root/competing" ]; then
      printf '%s' other-owner > "$root/lease"
      sleep 0.2
      exit 1
    fi
    for arg in "$@"; do
      case "$arg" in
        io.ctl.lease=*) printf '%s' "${arg#io.ctl.lease=}" > "$root/lease" ;;
      esac
    done
    printf '%s' "$$" > "$root/run.pid"
    while IFS= read -r heartbeat; do
      printf '%s\n' "$heartbeat" >> "$root/heartbeats"
    done
    ;;
  inspect)
    if [ -f "$root/late_create" ] && [ ! -f "$root/lease" ]; then
      : > "$root/first_inspect"
      if [ -f "$root/create_after_client_exit" ]; then
        cp "$root/pending.lease" "$root/lease"
        : > "$root/container.running"
      fi
      exit 1
    fi
    [ -f "$root/lease" ] || exit 1
    printf '{"id":"test-container-id","lease":"%s","ports":{"1080/tcp":[{"HostIp":"127.0.0.1","HostPort":"%s"}]}}\n' "$(cat "$root/lease")" "$(cat "$root/port")"
    ;;
  exec)
    [ ! -f "$root/no_status" ] || exit 1
    cat "$root/status.json"
    ;;
  stop|rm)
    printf '%s\n' "$@" > "$root/remove.args"
    if [ "$1" = rm ] || [ ! -f "$root/late_create" ]; then
      rm -f "$root/container.running"
    fi
    kill -TERM "$(cat "$root/run.pid")" 2>/dev/null || :
    ;;
  *) exit 1 ;;
esac
