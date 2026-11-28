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
if [ '__CHECK_ONLY__' = yes ]; then
  existing_updater_launcher || exit 10
  if ! updater_tracks_channel; then
    if [ "$updater_entrypoint" != "$updater_launcher" ]; then
      digest=$("$updater_entrypoint" updater-digest </dev/null 2>/dev/null) || exit 10
    else
      digest=$(updater_hash "$updater_entrypoint")
    fi
    test "$digest" = '__UPDATER_SHA256__' || exit 10
  fi
  require_checkout_layout "$updater_entrypoint"
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
# A channel may have been selected since the upload preflight.
if existing_updater_launcher && updater_tracks_channel; then
  require_checkout_layout "$updater_entrypoint"
else
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
fi
printf 'ctlup-bootstrap-v1\n'
