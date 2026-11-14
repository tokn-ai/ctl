#!/bin/sh
set -eu
cd "$(dirname "$0")"
# Keep personal startup files, prompt hooks, and inherited secrets out of the shell.
# Use only system programs so the sample works on macOS and Linux hosts.
exec env -i PATH=/usr/bin:/bin TERM=xterm-256color BASH_SILENCE_DEPRECATION_WARNING=1 /bin/bash --noprofile --rcfile ./shell.rc -i
