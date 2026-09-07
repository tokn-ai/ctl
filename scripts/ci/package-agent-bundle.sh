#!/bin/sh
set -eu

if [ "$#" -ne 5 ]; then
  echo "usage: package-agent-bundle.sh TARGET APP_VERSION BUNDLE_ID GIT_REVISION OUTPUT_DIRECTORY" >&2
  exit 2
fi

target=$1
app_version=$2
bundle_id=$3
git_revision=$4
output_directory=$5

case "$target" in
  x86_64-unknown-linux-musl|aarch64-unknown-linux-musl|x86_64-apple-darwin|aarch64-apple-darwin) ;;
  *)
    echo "unsupported agent bundle target: $target" >&2
    exit 2
    ;;
esac

case "$app_version" in
  *[!a-zA-Z0-9._+-]*|'')
    echo "invalid app version: $app_version" >&2
    exit 2
    ;;
esac

case "$bundle_id" in
  *[!a-zA-Z0-9._+-]*|'')
    echo "invalid bundle id: $bundle_id" >&2
    exit 2
    ;;
esac

if [ "${#bundle_id}" -gt 128 ]; then
  echo "bundle id is too long" >&2
  exit 2
fi

case "$git_revision" in
  *[!0-9a-fA-F]*|'')
    echo "invalid git revision: $git_revision" >&2
    exit 2
    ;;
esac

if [ "${#git_revision}" -ne 40 ]; then
  echo "git revision must contain 40 hexadecimal characters" >&2
  exit 2
fi

target_directory=${CARGO_TARGET_DIR:-target}
binary_directory="$target_directory/$target/release"
staging_directory="$output_directory/staging-$target"
archive="ctl-agent-bundle-$bundle_id-$target.tar.gz"

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
  "  \"app_version\": \"$app_version\"," \
  "  \"bundle_id\": \"$bundle_id\"," \
  "  \"git_revision\": \"$git_revision\"," \
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
