#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/../.." && pwd)
image_name=localhost/ctl-openconnect:local
config_file=$repo_dir/.env
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
  mkdir -p "$vpn_build_context/docker/openconnect" "$vpn_build_context/docker/vpn"
  for script in Dockerfile entrypoint.sh ssh-handshake.sh vpn-network.sh healthcheck.sh; do
    cp "$script_dir/$script" "$vpn_build_context/docker/openconnect/$script"
  done
  cp "$repo_dir/docker/vpn/heartbeat.sh" "$repo_dir/docker/vpn/watchdog.sh" "$vpn_build_context/docker/vpn/"
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
  start)
    if ! [ -f "$config_file" ]; then
      printf 'Copy %s/.env.example to %s and fill in your VPN settings.\n' "$script_dir" "$config_file" >&2
      exit 1
    fi
    chmod 0600 "$config_file"
    build_image
    cargo build --manifest-path "$repo_dir/Cargo.toml" \
      --target-dir "$repo_dir/target" -p ctl-cli -p ctld
    exec "$ctl_binary" vpn start --env-file "$config_file" "$@"
    ;;
  status|stop)
    if ! [ -x "$ctl_binary" ] || ! [ -x "$CTLD_BIN" ]; then
      printf 'Build ctl and ctld with %s start before managing the VPN.\n' "$0" >&2
      exit 1
    fi
    exec "$ctl_binary" vpn "$action" "$@"
    ;;
  *)
    printf 'Usage: %s [build|start|status|stop]\n' "$0" >&2
    exit 2
    ;;
esac
