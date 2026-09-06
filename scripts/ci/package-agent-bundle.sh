#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
  echo "usage: package-agent-bundle.sh TARGET VERSION OUTPUT_DIRECTORY" >&2
  exit 2
fi

target=$1
version=$2
output_directory=$3

case "$target" in
  x86_64-unknown-linux-musl|aarch64-unknown-linux-musl|x86_64-apple-darwin|aarch64-apple-darwin) ;;
  *)
    echo "unsupported agent bundle target: $target" >&2
    exit 2
    ;;
esac

case "$version" in
  *[!a-zA-Z0-9._+-]*|'')
    echo "invalid agent bundle version: $version" >&2
    exit 2
    ;;
esac

target_directory=${CARGO_TARGET_DIR:-target}
binary_directory="$target_directory/$target/release"
staging_directory="$output_directory/staging-$target"
archive="ctl-agent-bundle-$version-$target.tar.gz"

mkdir -p "$staging_directory" "$output_directory"
for binary in ctl-agent rmuxd taskd; do
  if [ ! -x "$binary_directory/$binary" ]; then
    echo "missing release binary: $binary_directory/$binary" >&2
    exit 1
  fi
  cp "$binary_directory/$binary" "$staging_directory/$binary"
done

if command -v strip >/dev/null 2>&1; then
  strip "$staging_directory/ctl-agent" "$staging_directory/rmuxd" "$staging_directory/taskd"
fi

checksum() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

ctl_agent_sha256=$(checksum "$staging_directory/ctl-agent")
rmuxd_sha256=$(checksum "$staging_directory/rmuxd")
taskd_sha256=$(checksum "$staging_directory/taskd")
printf '%s\n' \
  '{' \
  '  "schema_version": 1,' \
  "  \"version\": \"$version\"," \
  "  \"target_triple\": \"$target\"," \
  '  "files": {' \
  "    \"ctl-agent\": \"$ctl_agent_sha256\"," \
  "    \"rmuxd\": \"$rmuxd_sha256\"," \
  "    \"taskd\": \"$taskd_sha256\"" \
  '  }' \
  '}' > "$staging_directory/manifest.json"

tar -czf "$output_directory/$archive" -C "$staging_directory" \
  ctl-agent rmuxd taskd manifest.json
archive_sha256=$(checksum "$output_directory/$archive")
printf '%s  %s\n' "$archive_sha256" "$archive" > "$output_directory/$archive.sha256"
rm -rf "$staging_directory"
