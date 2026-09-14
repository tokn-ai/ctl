#!/bin/sh
set -eu

identity=false
managed_prefix='PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; command -v ctl-agent >/dev/null 2>&1 || { printf '\''ctl-ssh-nf\n'\''; exit 127; };'
authenticated_prefix='printf '\''ctl-ssh-auth-v1\n'\''; '"$managed_prefix"
case "${SSH_ORIGINAL_COMMAND:-}" in
  "exec ctl-agent connect"|\
  'PATH="$HOME/.tokn/ctl/current:$PATH" exec ctl-agent connect'|\
  "$managed_prefix exec ctl-agent connect") service=rmux ;;
  "exec ctl-agent connect --service task"|\
  'PATH="$HOME/.tokn/ctl/current:$PATH" exec ctl-agent connect --service task'|\
  "$managed_prefix exec ctl-agent connect --service task") service=task ;;
  "exec ctl-agent connect --identity"|\
  'PATH="$HOME/.tokn/ctl/current:$PATH" exec ctl-agent connect --identity'|\
  "$managed_prefix exec ctl-agent connect --identity"|\
  "$authenticated_prefix exec ctl-agent connect --identity") service=rmux; identity=true ;;
  "exec ctl-agent connect --service task --identity"|\
  'PATH="$HOME/.tokn/ctl/current:$PATH" exec ctl-agent connect --service task --identity'|\
  "$managed_prefix exec ctl-agent connect --service task --identity"|\
  "$authenticated_prefix exec ctl-agent connect --service task --identity") service=task; identity=true ;;
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
