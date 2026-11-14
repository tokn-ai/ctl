#!/bin/sh
set -eu
test -r README.md
printf 'Orbit workspace\n'
printf 'Release notes: %s lines, readable on this host\n' "$(wc -l < README.md | tr -d ' ')"
printf '\nPreview the release notes:\n'
# Absolute OSC 8 target works even before cwd observation has caught up.
printf '\033]8;;file://%s/README.md\033\\README.md\033]8;;\033\\\n' "$PWD"
