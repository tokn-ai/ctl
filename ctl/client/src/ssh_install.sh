set -eu
umask 077
base="$HOME/.tokn/ctl"
versions="$base/versions"
destination="$versions/__BUNDLE_ID__"
temporary="$versions/.install-__BUNDLE_ID__-$$"
link="$base/.current-$$"
receiver=""
mkdir -p "$versions"
test ! -e "$temporary"
mkdir "$temporary"
trap 'if [ -n "$receiver" ]; then kill "$receiver" 2>/dev/null || :; fi; rm -rf "$temporary" "$link"' EXIT HUP INT TERM
archive="$temporary/bundle.tar.gz"
payload="$temporary/payload"
mkdir "$payload"
: > "$archive"
printf 'ctl-install-progress-v1 receiving 0\n'
# Preserve stdin explicitly: a background command otherwise receives /dev/null
# in a non-interactive POSIX shell. Count bytes at the receiver, not SSH's pipe.
exec 3<&0
cat <&3 > "$archive" &
receiver=$!
exec 3<&-
while kill -0 "$receiver" 2>/dev/null; do
  received=$(wc -c < "$archive" | tr -d '[:space:]')
  printf 'ctl-install-progress-v1 receiving %s\n' "$received"
  sleep 1
done
wait "$receiver"
receiver=""
received=$(wc -c < "$archive" | tr -d '[:space:]')
test "$received" -eq __ARCHIVE_BYTES__
printf 'ctl-install-progress-v1 receiving %s\n' "$received"
printf 'ctl-install-progress-v1 extracting\n'
tar -xzf "$archive" -C "$payload"
for binary in ctl-agent ctmuxd ctl-taskd ctld; do
  printf 'ctl-install-progress-v1 checking %s\n' "$binary"
  test -f "$payload/$binary"
  test ! -L "$payload/$binary"
  chmod 700 "$payload/$binary"
done
if [ -e "$payload/manifest.json" ] || [ -L "$payload/manifest.json" ]; then
  test -f "$payload/manifest.json"
  test ! -L "$payload/manifest.json"
fi
reject_existing_bundle() {
  printf 'ctl install: bundle ID __BUNDLE_ID__ already exists with different %s; existing components were kept\n' "$1" >&2
  exit 1
}
if [ -e "$destination" ] || [ -L "$destination" ]; then
  if [ ! -d "$destination" ] || [ -L "$destination" ]; then
    reject_existing_bundle 'installation type'
  fi
  for binary in ctl-agent ctmuxd ctl-taskd ctld; do
    if [ ! -f "$destination/$binary" ] || [ -L "$destination/$binary" ] || \
      [ ! -x "$destination/$binary" ] || ! cmp -s "$payload/$binary" "$destination/$binary"; then
      reject_existing_bundle "$binary"
    fi
  done
  if [ -e "$payload/manifest.json" ] || [ -e "$destination/manifest.json" ] || \
    [ -L "$destination/manifest.json" ]; then
    if [ ! -f "$payload/manifest.json" ] || [ ! -f "$destination/manifest.json" ] || \
      [ -L "$destination/manifest.json" ] || \
      ! cmp -s "$payload/manifest.json" "$destination/manifest.json"; then
      reject_existing_bundle 'manifest.json'
    fi
  fi
else
  mv "$payload" "$destination"
fi
test -x "$destination/ctl-agent"
test -x "$destination/ctmuxd"
test -x "$destination/ctl-taskd"
test -x "$destination/ctld"
printf 'ctl-install-progress-v1 activating\n'
ln -s "versions/__BUNDLE_ID__" "$link"
case "$(uname -s)" in
  Linux) mv -fT "$link" "$base/current" ;;
  Darwin) mv -fh "$link" "$base/current" ;;
  *) printf 'ctl install does not support this platform\n' >&2; exit 1 ;;
esac
rm -rf "$temporary"
trap - EXIT HUP INT TERM
printf 'ctl-install-v1\n'
