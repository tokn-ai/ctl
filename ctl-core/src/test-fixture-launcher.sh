#!/bin/sh
# Execute the separate script with the original command path as $0.
exec /bin/sh -c '. "$0.script"' "$0" "$@"
