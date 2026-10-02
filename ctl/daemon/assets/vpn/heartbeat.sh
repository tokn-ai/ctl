#!/bin/sh
set -eu
umask 077

state=${CTLD_HEARTBEAT_DIR:-/run/ctl/heartbeat}
clock=${CTLD_HEARTBEAT_CLOCK_FILE:-/proc/uptime}
ttl=${CTLD_HEARTBEAT_TIMEOUT_SECONDS:-15}
case "$ttl" in ''|*[!0-9]*) exit 1 ;; esac
[ "$ttl" -gt 0 ] || exit 1
[ ! -d "$state/closing" ] || exit 1
IFS=' ' read -r uptime ignored < "$clock"
now=${uptime%%.*}
case "$now" in ''|*[!0-9]*) exit 1 ;; esac
deadline=$((now + ttl))
mkdir -p "$state/beats/$deadline"
# Publishing a marker is atomic and cannot overwrite a newer sender's deadline.
# A shutdown claim wins only if its rescan finds no acknowledged future marker.
[ ! -d "$state/closing" ]
