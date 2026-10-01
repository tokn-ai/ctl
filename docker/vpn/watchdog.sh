#!/bin/sh
set -eu
umask 077

state=${CTLD_HEARTBEAT_DIR:-/run/ctl/heartbeat}
clock=${CTLD_HEARTBEAT_CLOCK_FILE:-/proc/uptime}
grace=${CTLD_HEARTBEAT_STARTUP_SECONDS:-15}
target=${1:?entrypoint PID is required}
case "$grace" in ''|*[!0-9]*) exit 1 ;; esac
[ "$grace" -gt 0 ] || exit 1
mkdir -p "$state/beats"
IFS=' ' read -r uptime ignored < "$clock"
now=${uptime%%.*}
case "$now" in ''|*[!0-9]*) exit 1 ;; esac
mkdir -p "$state/beats/$((now + grace))"

live_heartbeat() {
  # Conditional callers suppress set -e inside this function. A failed read
  # must not reuse the previous timestamp and keep expired interests alive.
  IFS=' ' read -r uptime ignored < "$clock" || return 1
  now=${uptime%%.*}
  case "$now" in ''|*[!0-9]*) return 1 ;; esac
  found=false
  for marker in "$state"/beats/*; do
    [ -d "$marker" ] || continue
    deadline=${marker##*/}
    case "$deadline" in ''|*[!0-9]*) continue ;; esac
    if [ "$deadline" -gt "$now" ]; then
      found=true
    else
      rmdir "$marker" 2>/dev/null || :
    fi
  done
  [ "$found" = true ]
}

while kill -0 "$target" 2>/dev/null; do
  if ! live_heartbeat; then
    if mkdir "$state/closing" 2>/dev/null; then
      # Serialize the expiry decision against renewal acknowledgement. A beat
      # published before this claim must be observed by the second scan.
      if live_heartbeat; then
        rmdir "$state/closing"
      else
        printf '%s\n' 'No ctld heartbeat remains; stopping the VPN and SOCKS5.' >&2
        kill -TERM "$target" 2>/dev/null || :
        exit 0
      fi
    else
      exit 0
    fi
  fi
  sleep 1
done
