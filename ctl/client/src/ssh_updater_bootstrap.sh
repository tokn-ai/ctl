set -eu
umask 077
fail() {
  printf 'ctlup bootstrap: %s\n' "$1" >&2
  exit 1
}
base="$HOME/.tokn/ctl"
for directory in "$HOME" "$HOME/.tokn" "$base" "$base/bin"; do
  test ! -L "$directory" || fail 'installation directory is a symbolic link'
  mkdir -p "$directory"
  test -d "$directory" || fail 'installation path is not a directory'
  case "$(uname -s)" in
    Linux) metadata=$(stat -c '%u %a' "$directory") ;;
    Darwin) metadata=$(stat -f '%u %Lp' "$directory") ;;
    *) fail 'unsupported installation platform' ;;
  esac
  test "${metadata%% *}" -eq "$(id -u)" || fail 'installation directory is not owned by this account'
  mode=$((0${metadata#* }))
  test "$((mode & 022))" -eq 0 || fail 'installation directory is writable by group or others'
done
destination="$base/bin/ctlup"
test ! -L "$destination" || fail 'updater entry point is a symbolic link'
if [ -e "$destination" ]; then test -f "$destination" || fail 'updater entry point is not a regular file'; fi
if [ '__CHECK_ONLY__' = yes ]; then
  if [ -f "$destination" ] && [ -x "$destination" ]; then
    if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$destination"); else actual=$(shasum -a 256 "$destination"); fi
    if [ "${actual%% *}" = '__UPDATER_SHA256__' ]; then
      printf 'ctlup-bootstrap-v1\n'
      exit 0
    fi
  fi
  exit 10
fi
temporary="$base/bin/.ctlup-__TOKEN__"
test ! -e "$temporary" && test ! -L "$temporary" || fail 'bootstrap staging path already exists'
trap 'rm -f "$temporary"' EXIT HUP INT TERM
# noclobber prevents accepting a raced-in link or other existing path.
set -C
cat > "$temporary"
set +C
received=$(wc -c < "$temporary" | tr -d '[:space:]')
test "$received" -eq __UPDATER_BYTES__ || fail 'incomplete updater upload'
if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$temporary"); else actual=$(shasum -a 256 "$temporary"); fi
test "${actual%% *}" = '__UPDATER_SHA256__' || fail 'updater checksum mismatch'
chmod 700 "$temporary"
# A wrong architecture or invalid executable must not replace a working updater.
"$temporary" --component-info >/dev/null
version=$("$temporary" --version)
case "$version" in
  'ctlup '*) ;;
  *) fail 'uploaded executable is not ctlup' ;;
esac
mv -f "$temporary" "$destination"
trap - EXIT HUP INT TERM
printf 'ctlup-bootstrap-v1\n'
