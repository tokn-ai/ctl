set -eu
umask 077
fail() {
  printf 'ctlup bootstrap: %s\n' "$1" >&2
  exit 1
}
__CHECKOUT_LAYOUT_GUARD__
__UPDATER_STAGING__
base="$HOME/.tokn/ctl"
if [ '__CHECK_ONLY__' = yes ]; then
  destination="$base/bin/ctlup"
  if [ -x "$destination" ] && digest=$("$destination" updater-digest </dev/null 2>/dev/null) &&
     [ "$digest" = '__UPDATER_SHA256__' ]; then
    require_checkout_layout "$destination"
    printf 'ctlup-bootstrap-v1\n'
    exit 0
  fi
  exit 10
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
require_checkout_layout "$temporary"
"$temporary" install-manager </dev/null
printf 'ctlup-bootstrap-v1\n'
