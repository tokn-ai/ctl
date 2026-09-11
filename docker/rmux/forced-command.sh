#!/bin/sh
set -eu

identity=false
case "${SSH_ORIGINAL_COMMAND:-}" in
  "exec ctl-agent connect"|\
  'PATH="$HOME/.tokn/ctl/current:$PATH" exec ctl-agent connect') service=rmux ;;
  "exec ctl-agent connect --service task"|\
  'PATH="$HOME/.tokn/ctl/current:$PATH" exec ctl-agent connect --service task') service=task ;;
  "exec ctl-agent connect --identity"|\
  'PATH="$HOME/.tokn/ctl/current:$PATH" exec ctl-agent connect --identity') service=rmux; identity=true ;;
  "exec ctl-agent connect --service task --identity"|\
  'PATH="$HOME/.tokn/ctl/current:$PATH" exec ctl-agent connect --service task --identity') service=task; identity=true ;;
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

set -- connect
if [ "$service" = task ]; then
  set -- "$@" --service task
fi
if [ "$identity" = true ]; then
  set -- "$@" --identity
fi
exec /usr/local/bin/ctl-agent "$@"
