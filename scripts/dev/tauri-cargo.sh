#!/bin/sh
set -eu

# Tauri invokes its runner for every native reload. Signed development hands
# successful builds to the app supervisor; unsigned runs keep Cargo ownership.
if [ "${1:-}" = run ]; then
  script_directory=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
  if [ -n "${RMUX_DEV_APP_SUPERVISOR:-}" ]; then
    exec node --experimental-strip-types --disable-warning=ExperimentalWarning \
      "$script_directory/signed-cargo.mts" "$@"
  fi
  node --experimental-strip-types --disable-warning=ExperimentalWarning \
    "$script_directory/prepare-daemons.mts" "$@"
fi
exec cargo "$@"
