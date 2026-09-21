#!/bin/sh
set -eu

# Tauri invokes its runner for every native reload. Prepare helpers before
# handing this PID to Cargo so Tauri can stop the app normally on the next one.
if [ "${1:-}" = run ]; then
  script_directory=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
  node --experimental-strip-types --disable-warning=ExperimentalWarning \
    "$script_directory/prepare-daemons.mts" "$@"
fi
exec cargo "$@"
