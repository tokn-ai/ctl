#!/bin/sh
set -eu

case "${SSH_ORIGINAL_COMMAND:-}" in
  "exec ctl-agent connect"|\
  'PATH="${XDG_DATA_HOME:-$HOME/.local/share}/ctl/current:$PATH" exec ctl-agent connect') service=rmux ;;
  "exec ctl-agent connect --service task"|\
  'PATH="${XDG_DATA_HOME:-$HOME/.local/share}/ctl/current:$PATH" exec ctl-agent connect --service task') service=task ;;
  *)
    echo "rmux container: only the fixed ctl-agent rmux or task command is permitted" >&2
    exit 126
    ;;
esac

umask 077
export RMUX_RUNTIME_DIR=/run/rmux
export TASKD_RUNTIME_DIR=/run/taskd
export TASKD_DATA_DIR=/var/lib/taskd
export PATH=/usr/local/bin:/usr/bin:/bin

if [ "$service" = task ]; then
  exec /usr/local/bin/ctl-agent connect --service task
fi
exec /usr/local/bin/ctl-agent connect
