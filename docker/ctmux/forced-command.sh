#!/bin/sh
set -eu

identity=false
authenticated=false
operation=connect
service=ctmux
managed_prefix='PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; command -v ctl-agent >/dev/null 2>&1 || { printf '\''ctl-ssh-nf\n'\''; exit 127; };'
authenticated_prefix='printf '\''ctl-ssh-auth-v1\n'\''; '"$managed_prefix"
case "${SSH_ORIGINAL_COMMAND:-}" in
  "exec ctl-agent vpn"|"$managed_prefix exec ctl-agent vpn") operation=vpn ;;
  "exec ctl-agent connect"|\
  'PATH="$HOME/.tokn/ctl/current:$PATH" exec ctl-agent connect'|\
  "$managed_prefix exec ctl-agent connect") service=ctmux ;;
  "exec ctl-agent connect --service task"|\
  'PATH="$HOME/.tokn/ctl/current:$PATH" exec ctl-agent connect --service task'|\
  "$managed_prefix exec ctl-agent connect --service task") service=task ;;
  "exec ctl-agent connect --identity"|\
  'PATH="$HOME/.tokn/ctl/current:$PATH" exec ctl-agent connect --identity'|\
  "$managed_prefix exec ctl-agent connect --identity") service=ctmux; identity=true ;;
  "$authenticated_prefix exec ctl-agent connect --identity") service=ctmux; identity=true; authenticated=true ;;
  "exec ctl-agent connect --service task --identity"|\
  'PATH="$HOME/.tokn/ctl/current:$PATH" exec ctl-agent connect --service task --identity'|\
  "$managed_prefix exec ctl-agent connect --service task --identity") service=task; identity=true ;;
  "$authenticated_prefix exec ctl-agent connect --service task --identity") service=task; identity=true; authenticated=true ;;
  *)
    echo "ctmux container: only fixed ctl-agent ctmux, task, or vpn commands are permitted" >&2
    exit 126
    ;;
esac

umask 077
export CTMUX_RUNTIME_DIR=/run/ctmux
export CTL_TASKD_RUNTIME_DIR=/run/ctl-taskd
export CTL_TASKD_DATA_DIR=/var/lib/ctl-taskd
export PATH=/usr/local/bin:/usr/bin:/bin

if [ "$authenticated" = true ]; then
  printf 'ctl-ssh-auth-v1\n'
fi
if [ ! -f /usr/local/bin/ctl-agent ] || [ ! -x /usr/local/bin/ctl-agent ]; then
  printf 'ctl-ssh-nf\n'
  exit 127
fi

set -- "$operation"
if [ "$service" = task ]; then
  set -- "$@" --service task
fi
if [ "$identity" = true ]; then
  set -- "$@" --identity
fi
exec /usr/local/bin/ctl-agent "$@"
