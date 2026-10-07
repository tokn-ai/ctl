set -eu
umask 077
fail() {
  printf 'ctl install: %s\n' "$1" >&2
  exit 1
}
base="$HOME/.tokn/ctl"
versions="$base/components/agents/__BUNDLE_TARGET__"
for directory in "$HOME" "$HOME/.tokn" "$base" "$base/components" "$base/components/agents" "$versions"; do
  test ! -L "$directory" || fail "storage path is a symbolic link: $directory"
  mkdir -p "$directory"
  test -d "$directory" || fail "storage path is not a directory: $directory"
  case "$(uname -s)" in
    Linux) metadata=$(stat -c '%u %a' "$directory") ;;
    Darwin) metadata=$(stat -f '%u %Lp' "$directory") ;;
    *) fail 'unsupported installation platform' ;;
  esac
  test "${metadata%% *}" -eq "$(id -u)" || fail "storage directory is not owned by this account: $directory"
  mode=$((0${metadata#* }))
  test "$((mode & 022))" -eq 0 || fail "storage directory is writable by group or others: $directory"
done
base=$(cd "$base" && pwd -P)
versions="$base/components/agents/__BUNDLE_TARGET__"
if ! mkdir "$base/components/.sync-lock" 2>/dev/null; then
  printf 'ctl install: another component sync is running; previous installation was kept\n' >&2
  exit 1
fi
temporary=""
link="$base/.current-$$"
receiver=""
trap 'if [ -n "$receiver" ]; then kill "$receiver" 2>/dev/null || :; fi; if [ -n "$temporary" ]; then rm -rf "$temporary"; fi; rm -f "$link"; rmdir "$base/components/.sync-lock"' EXIT HUP INT TERM
temporary=$(mktemp -d "$versions/.install-XXXXXX")
archive="$temporary/agent.tar.gz"
payload="$temporary/payload"
mkdir "$payload"
# The counter must see a file even before the background receiver runs.
: > "$archive"
printf 'ctl-install-progress-v1 receiving 0\n'
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
test "$received" -eq __ARCHIVE_BYTES__ || fail 'incomplete agent upload; previous installation was kept'
if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$archive"); else actual=$(shasum -a 256 "$archive"); fi
test "${actual%% *}" = '__ARCHIVE_SHA256__' || fail 'archive checksum mismatch; previous installation was kept'
printf 'ctl-install-progress-v1 receiving %s\n' "$received"
printf 'ctl-install-progress-v1 extracting\n'
tar -xzf "$archive" -C "$payload"
printf 'ctl-install-progress-v1 checking ctl-agent\n'
test -f "$payload/ctl-agent" && test ! -L "$payload/ctl-agent"
test -f "$payload/agent-source.json" && test ! -L "$payload/agent-source.json"
chmod 700 "$payload/ctl-agent"
chmod 600 "$payload/agent-source.json"
# Pin retained companions to the previous immutable installation. Never link
# through current, which would become a self-reference after activation.
previous=__COMPANION_DIRECTORY__
if [ -z "$previous" ] && { [ -e "$base/current" ] || [ -L "$base/current" ]; }; then
  test -L "$base/current"
  previous=$(cd "$base/current" && pwd -P)
  case "$previous" in "$base"/*) ;; *) printf 'ctl install: active components are outside the managed directory\n' >&2; exit 1 ;; esac
fi
if [ -n "$previous" ]; then
  for binary in ctmuxd ctl-taskd ctld; do
    companion="$previous/$binary"
    if [ "$binary" = ctld ] && [ -f "$previous/ctld.app/Contents/MacOS/ctld" ]; then
      companion="$previous/ctld.app/Contents/MacOS/ctld"
    fi
    if [ -e "$companion" ] || [ -L "$companion" ]; then
      test -f "$companion" && test -x "$companion"
      ln -s "$companion" "$payload/$binary"
    fi
  done
fi
destination="$versions/__STORE_ID__-${temporary##*.install-}"
test ! -e "$destination" && test ! -L "$destination"
mv "$payload" "$destination"
printf 'ctl-install-progress-v1 activating\n'
ln -s "$destination" "$link"
case "$(uname -s)" in
  Linux) mv -fT "$link" "$base/current" ;;
  Darwin) mv -fh "$link" "$base/current" ;;
  *) exit 1 ;;
esac
printf 'ctl-install-v1\n'
