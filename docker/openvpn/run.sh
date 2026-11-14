#!/bin/sh
set -eu

if [ "$#" -ne 1 ] || [ "$1" != build ]; then
  printf 'Usage: %s build\n' "$0" >&2
  exit 2
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/../.." && pwd)

# Podman doesn't use Dockerfile-specific .dockerignore files. Give both engines
# the same small context regardless of which engine the caller has installed.
context=$(mktemp -d "${TMPDIR:-/tmp}/ctl-openvpn-build.XXXXXX")
trap 'rm -rf -- "$context"' EXIT
mkdir -p "$context/docker/openvpn" "$context/ctl/daemon/assets/vpn"
for file in Dockerfile entrypoint.sh vpn-network.sh healthcheck.sh; do
  cp "$script_dir/$file" "$context/docker/openvpn/$file"
done
cp "$repo_dir/ctl/daemon/assets/vpn/heartbeat.sh" "$repo_dir/ctl/daemon/assets/vpn/watchdog.sh" \
  "$context/ctl/daemon/assets/vpn/"

if command -v docker >/dev/null 2>&1; then
  engine=docker
else
  engine=podman
fi

# Podman may also be installed under the docker name. Its default OCI image
# format discards Dockerfile health checks.
set -- --file "$context/docker/openvpn/Dockerfile" --tag localhost/ctl-openvpn:local
if "$engine" build --help | grep -q BUILDAH_FORMAT; then
  set -- --format=docker "$@"
fi
"$engine" build "$@" "$context"
