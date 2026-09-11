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
for binary in ctl-agent rmuxd taskd; do
  printf 'ctl-install-progress-v1 checking %s\n' "$binary"
  test -f "$payload/$binary"
  chmod 700 "$payload/$binary"
done
printf 'ctl-install-progress-v1 activating\n'
if [ ! -e "$destination" ]; then
  mv "$payload" "$destination"
fi
test -x "$destination/ctl-agent"
test -x "$destination/rmuxd"
test -x "$destination/taskd"
ln -s "versions/__BUNDLE_ID__" "$link"
mv -f "$link" "$base/current"
rm -rf "$temporary"
trap - EXIT HUP INT TERM
printf 'ctl-install-v1\n'
