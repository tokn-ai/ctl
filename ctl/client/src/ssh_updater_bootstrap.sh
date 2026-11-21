set -eu
umask 077
fail() {
  printf 'ctlup bootstrap: %s\n' "$1" >&2
  exit 1
}
__CHECKOUT_LAYOUT_GUARD__
__UPDATER_STAGING__
updater_hash() {
  if command -v sha256sum >/dev/null 2>&1; then hash=$(sha256sum "$1"); else hash=$(shasum -a 256 "$1"); fi
  printf '%s\n' "${hash%% *}"
}
base="$HOME/.tokn/ctl"
if [ '__CHECK_ONLY__' = yes ]; then
  for directory in "$HOME" "$HOME/.tokn" "$base" "$base/bin"; do
    test ! -L "$directory" || fail 'installation directory is a symbolic link'
    test -e "$directory" || exit 10
    test -d "$directory" || fail 'installation path is not a directory'
    require_private_owner_mode "$directory"
  done
  destination="$base/bin/ctlup"
  if [ -L "$destination" ]; then
    test "$(readlink "$destination")" = .ctl-launcher || fail 'updater entry point has an unexpected link target'
    launcher="$base/bin/.ctl-launcher"
    test ! -L "$launcher" || fail 'updater launcher is a symbolic link'
    test -e "$launcher" || exit 10
    test -f "$launcher" || fail 'updater launcher is not a regular file'
    require_private_owner_mode "$launcher"
    test -x "$launcher" || exit 10
    digest=$("$destination" updater-digest </dev/null 2>/dev/null) || exit 10
  else
    test -e "$destination" || exit 10
    test -f "$destination" || fail 'updater entry point is not a regular file'
    require_private_owner_mode "$destination"
    test -x "$destination" || exit 10
    digest=$(updater_hash "$destination")
  fi
  test "$digest" = '__UPDATER_SHA256__' || exit 10
  require_checkout_layout "$destination"
  printf 'ctlup-bootstrap-v1\n'
  exit 0
fi
prepare_updater_staging
temporary="$updater_staging/ctlup-__TOKEN__"
test ! -e "$temporary" && test ! -L "$temporary" || fail 'bootstrap staging path already exists'
trap 'rm -f "$temporary"' EXIT HUP INT TERM
# noclobber prevents accepting a raced-in link or other existing path.
set -C
cat > "$temporary"
set +C
received=$(wc -c < "$temporary" | tr -d '[:space:]')
test "$received" -eq __UPDATER_BYTES__ || fail 'incomplete updater upload'
test "$(updater_hash "$temporary")" = '__UPDATER_SHA256__' || fail 'updater checksum mismatch'
chmod 700 "$temporary"
# A wrong architecture or invalid executable must not replace a working updater.
"$temporary" --component-info >/dev/null
version=$("$temporary" --version)
case "$version" in
  'ctlup '*) ;;
  *) fail 'uploaded executable is not ctlup' ;;
esac
require_checkout_layout "$temporary"
"$temporary" install-manager </dev/null
printf 'ctlup-bootstrap-v1\n'
