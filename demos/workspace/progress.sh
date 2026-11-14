#!/bin/sh
# A real long-running process with predictable, fictional output.
set -eu
step=1
while [ "$step" -le 120 ]; do
  printf 'heartbeat %03d | session is running\n' "$step"
  step=$((step + 1))
  sleep 1
done
printf 'Heartbeat demo finished.\n'
