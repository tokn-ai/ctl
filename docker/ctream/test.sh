#!/bin/bash
set -euo pipefail

# The checkout is mounted read-only. Cargo artifacts stay in the build volume.
cargo build --locked --features ctream-agent/test-fixtures -p ctream-cli -p ctream-agent -p ctream-wayland-input --bins
cargo test --locked --features ctream-agent/test-fixtures -p ctream-driver -p ctream-driver-capture -p ctream-driver-input \
  -p ctream-driver-codec -p ctream-platform -p ctream-client -p ctream-agent
test_binary=$(cargo test --locked --features ctream-agent/test-fixtures -p ctream-agent --test linux_session --no-run --message-format=json |
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
