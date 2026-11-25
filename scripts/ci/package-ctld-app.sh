#!/bin/sh
set -eu

exec /bin/sh "$(dirname "$0")/../../ctl/updater/src/updater/sign/ctld/package-ctld-app.sh" "$@"
