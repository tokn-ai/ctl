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

case "$action" in
  build)
    [ "$#" -eq 0 ] || {
      printf 'Usage: %s build\n' "$0" >&2
      exit 2
    }
    docker build --tag "$image_name" "$script_dir"
    ;;
  start)
    if ! [ -f "$config_file" ]; then
      printf 'Copy %s/.env.example to %s and fill in your VPN settings.\n' "$script_dir" "$config_file" >&2
      exit 1
    fi
    chmod 0600 "$config_file"
    docker build --tag "$image_name" "$script_dir"
    cargo build --manifest-path "$repo_dir/Cargo.toml" \
      --target-dir "$repo_dir/target" -p ctl -p ctld
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
