#!/bin/bash
set -euo pipefail

# The checkout is mounted read-only. Cargo artifacts stay in the build volume.
cargo build --locked -p ctream -p ctream-wayland-input --bins
test_binary=$(cargo test --locked -p ctream --test linux_session --no-run --message-format=json |
  python3 -c '
import json, sys
executable = None
for line in sys.stdin:
  message = json.loads(line)
  if message.get("target", {}).get("name") == "linux_session" and message.get("executable"):
    executable = message["executable"]
if executable is None:
  raise SystemExit("Cargo did not produce the Linux session test")
print(executable)
')

exec runuser -u ctream-test -- dbus-run-session -- \
  bash /workspace/docker/ctream/session.sh "$test_binary"
