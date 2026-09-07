#!/bin/sh
set -eu

if [ "$#" -ne 5 ]; then
  echo "usage: assemble-agent-bundle-set.sh APP_VERSION BUNDLE_ID GIT_REVISION INPUT_DIRECTORY OUTPUT_DIRECTORY" >&2
  exit 2
fi

app_version=$1
bundle_id=$2
git_revision=$3
input_directory=$4
output_directory=$5

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

checksum() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

validate_checksum() {
  archive=$1
  checksum_file=$2
  expected=$(awk 'NR == 1 { print $1 }' "$checksum_file")
  recorded_name=$(awk 'NR == 1 { print $2 }' "$checksum_file")
  case "$expected" in
    *[!0-9a-fA-F]*|'')
      echo "invalid checksum in $checksum_file" >&2
      exit 1
      ;;
  esac
  if [ "${#expected}" -ne 64 ]; then
    echo "invalid checksum length in $checksum_file" >&2
    exit 1
  fi
  if [ "$recorded_name" != "$(basename "$archive")" ]; then
    echo "invalid archive name in $checksum_file" >&2
    exit 1
  fi
  actual=$(checksum "$archive")
  if [ "$actual" != "$expected" ]; then
    echo "checksum mismatch for $archive" >&2
    exit 1
  fi
  printf '%s' "$expected"
}

mkdir -p "$output_directory"
targets='x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-apple-darwin aarch64-apple-darwin'

for target in $targets; do
  archive="ctl-agent-bundle-$bundle_id-$target.tar.gz"
  archive_path="$input_directory/$archive"
  checksum_path="$archive_path.sha256"
  if [ ! -f "$archive_path" ] || [ ! -f "$checksum_path" ]; then
    echo "missing bundle pair for $target" >&2
    exit 1
  fi
  validate_checksum "$archive_path" "$checksum_path" >/dev/null
  cp "$archive_path" "$checksum_path" "$output_directory/"
done

linux_x64_archive="ctl-agent-bundle-$bundle_id-x86_64-unknown-linux-musl.tar.gz"
linux_arm64_archive="ctl-agent-bundle-$bundle_id-aarch64-unknown-linux-musl.tar.gz"
macos_x64_archive="ctl-agent-bundle-$bundle_id-x86_64-apple-darwin.tar.gz"
macos_arm64_archive="ctl-agent-bundle-$bundle_id-aarch64-apple-darwin.tar.gz"
linux_x64_sha256=$(validate_checksum "$input_directory/$linux_x64_archive" "$input_directory/$linux_x64_archive.sha256")
linux_arm64_sha256=$(validate_checksum "$input_directory/$linux_arm64_archive" "$input_directory/$linux_arm64_archive.sha256")
macos_x64_sha256=$(validate_checksum "$input_directory/$macos_x64_archive" "$input_directory/$macos_x64_archive.sha256")
macos_arm64_sha256=$(validate_checksum "$input_directory/$macos_arm64_archive" "$input_directory/$macos_arm64_archive.sha256")

printf '%s\n' \
  '{' \
  '  "schema_version": 1,' \
  "  \"app_version\": \"$app_version\"," \
  "  \"bundle_id\": \"$bundle_id\"," \
  "  \"git_revision\": \"$git_revision\"," \
  '  "targets": {' \
  '    "x86_64-unknown-linux-musl": {' \
  "      \"archive\": \"$linux_x64_archive\"," \
  "      \"sha256\": \"$linux_x64_sha256\"" \
  '    },' \
  '    "aarch64-unknown-linux-musl": {' \
  "      \"archive\": \"$linux_arm64_archive\"," \
  "      \"sha256\": \"$linux_arm64_sha256\"" \
  '    },' \
  '    "x86_64-apple-darwin": {' \
  "      \"archive\": \"$macos_x64_archive\"," \
  "      \"sha256\": \"$macos_x64_sha256\"" \
  '    },' \
  '    "aarch64-apple-darwin": {' \
  "      \"archive\": \"$macos_arm64_archive\"," \
  "      \"sha256\": \"$macos_arm64_sha256\"" \
  '    }' \
  '  }' \
  '}' > "$output_directory/bundle-set.json"
