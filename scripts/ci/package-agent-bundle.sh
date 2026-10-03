#!/bin/sh
set -eu
exec node "$(dirname "$0")/package-agent-bundle.mts" "$@"
