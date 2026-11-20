#!/bin/bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd -P)
docker build --tag ctream-test:local "$repo_root/docker/ctream"
exec docker run --rm --init \
  --mount "type=bind,source=$repo_root,target=/workspace,readonly" \
  --mount type=volume,source=ctream-test-build,target=/build \
  --mount type=volume,source=ctream-test-registry,target=/root/.cargo/registry \
  ctream-test:local
