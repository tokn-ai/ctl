#!/bin/sh
set -eu
case "$0" in
  */*) bin=${0%/*} ;;
  *) printf 'ctld: could not resolve its installation\n' >&2; exit 1 ;;
esac
case "$bin" in /*) ;; *) bin="./$bin" ;; esac
# Resolve current once, preserving trailing newlines in a valid account path.
installation=$(CDPATH= cd -P "$bin/../current" && printf '%s.' "$PWD")
installation=${installation%.}
if [ -f "$installation/ctld.app/Contents/MacOS/ctld" ]; then
  exec "$installation/ctld.app/Contents/MacOS/ctld" "$@"
fi
exec "$installation/ctld" "$@"
