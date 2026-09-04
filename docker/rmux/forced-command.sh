#!/bin/sh
set -eu

expected_command="exec ctl-agent connect"
if [ "${SSH_ORIGINAL_COMMAND:-}" != "$expected_command" ]; then
  echo "rmux container: only '$expected_command' is permitted" >&2
  exit 126
fi

umask 077
export RMUX_RUNTIME_DIR=/run/rmux
export PATH=/usr/local/bin:/usr/bin:/bin

exec /usr/local/bin/ctl-agent connect
