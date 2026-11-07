#!/bin/sh
set -eu

# Tauri invokes its runner for every native reload. Signed development hands
# successful builds to the app supervisor; every Unix run prepares through ctlup.
if [ "${1:-}" = run ]; then
  script_directory=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
  exec node --experimental-strip-types --disable-warning=ExperimentalWarning \
    "$script_directory/signed-cargo.mts" "$@"
fi
exec cargo "$@"
