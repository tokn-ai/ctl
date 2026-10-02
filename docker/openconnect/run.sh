#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/../.." && pwd)
image_name=localhost/ctl-openconnect:local
ctl_binary=$repo_dir/target/debug/ctl
export CTLD_BIN="$repo_dir/target/debug/ctld"
action=${1:-start}
if [ "$#" -gt 0 ]; then
  shift
fi

build_image() {
  # Both Docker and Podman receive only the files needed by this image.
  vpn_build_context=$(mktemp -d "${TMPDIR:-/tmp}/ctl-openconnect-build.XXXXXX")
  trap 'rm -rf -- "$vpn_build_context"' EXIT
  mkdir -p "$vpn_build_context/docker/openconnect" "$vpn_build_context/ctl/daemon/assets/vpn"
  for script in Dockerfile entrypoint.sh ssh-handshake.sh vpn-network.sh healthcheck.sh; do
    cp "$script_dir/$script" "$vpn_build_context/docker/openconnect/$script"
  done
  cp "$repo_dir/ctl/daemon/assets/vpn/heartbeat.sh" "$repo_dir/ctl/daemon/assets/vpn/watchdog.sh" "$vpn_build_context/ctl/daemon/assets/vpn/"
  docker build --file "$vpn_build_context/docker/openconnect/Dockerfile" --tag "$image_name" "$vpn_build_context"
  rm -rf -- "$vpn_build_context"
  trap - EXIT
}

case "$action" in
  build)
    [ "$#" -eq 0 ] || {
      printf 'Usage: %s build\n' "$0" >&2
      exit 2
    }
    build_image
    ;;
  create)
    cargo build --manifest-path "$repo_dir/Cargo.toml" \
      --target-dir "$repo_dir/target" -p ctl-cli
    exec "$ctl_binary" vpn create "$@"
    ;;
  start)
    build_image
    cargo build --manifest-path "$repo_dir/Cargo.toml" \
      --target-dir "$repo_dir/target" -p ctl-cli -p ctld
    exec "$ctl_binary" vpn start "$@"
    ;;
  list|stop|remove)
    if ! [ -x "$ctl_binary" ]; then
      cargo build --manifest-path "$repo_dir/Cargo.toml" \
        --target-dir "$repo_dir/target" -p ctl-cli
    fi
    exec "$ctl_binary" vpn "$action" "$@"
    ;;
  *)
    printf 'Usage: %s [build|create|start|list|stop|remove] [NAME_OR_ID]\n' "$0" >&2
    exit 2
    ;;
esac
