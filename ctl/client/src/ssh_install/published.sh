set -eu
umask 077
unset WGET_ASKPASS SSH_ASKPASS
__ARGUMENTS__
size=$1
sha=$2
url=$3
origin=$4
archive=$5
target=$6
manifest_bytes=$7
local_updater=$8
local_runtime=$9
fail() { printf '%s\n' "$1" >&2; exit 1; }
unavailable() { printf 'ctl-published-unavailable-v1\n'; exit 12; }
__ACCOUNT_GUARD__
printf 'ctl-published-stage-v1 checking-updater\n'
# A mode-0700 directory prevents a verified executable being replaced before use.
case "$(uname -s)" in
  Linux) home_stat=$(stat -c '%u %a' "$HOME");;
  Darwin) home_stat=$(stat -f '%u %Lp' "$HOME");;
  *) fail 'Unsupported updater bootstrap platform.';;
esac
test ! -L "$HOME" && test "${home_stat%% *}" = "$(id -u)" && test $((0${home_stat#* } & 022)) -eq 0 || fail 'Unsafe account home for ctlup staging.'
stage=$(mktemp -d "$HOME/.ctlup-fetch.XXXXXXXX")
file="$stage/ctlup"
download_pid=''
timer_pid=''
cleanup() {
  if [ -n "$download_pid" ]; then kill "$download_pid" 2>/dev/null || :; fi
  if [ -n "$timer_pid" ]; then kill "$timer_pid" 2>/dev/null || :; fi
  rm -f "$file"; rmdir "$stage"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
length() { wc -c < "$1" | tr -d '[:space:]'; }
digest() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi
}
verified() {
  test "$(length "$file")" -eq "$size" || return 1
  hash=$(digest "$file") || return 1
  test "${hash%% *}" = "$sha"
}
# File-size limits bound writes even for chunked responses or older clients.
blocks=$(( (size + 511) / 512 ))
managed="$HOME/.tokn/ctl/bin/ctlup"
if [ -f "$managed" ] && [ ! -L "$managed" ] && [ "$(length "$managed")" -eq "$size" ] &&
   (ulimit -f "$blocks" && cp "$managed" "$file") && verified; then
  printf 'ctl-published-stage-v1 using-installed-updater\n'
elif [ "$local_updater" = 1 ]; then
  printf 'ctl-published-stage-v1 uploading-updater\n'
  # dd reads no more than the requested prefix from a pipe. Short SSH reads
  # are permitted, so recompute the remaining length after each dd process.
  : > "$file"
  received=0
  while [ "$received" -lt "$size" ]; do
    left=$((size - received)); block=65536
    if [ "$left" -lt "$block" ]; then block=$left; fi
    dd bs="$block" count="$((left / block))" >> "$file" 2>/dev/null
    next=$(length "$file")
    [ "$next" -gt "$received" ] || fail 'Incomplete cached updater upload.'
    received=$next
  done
  verified || fail 'Uploaded ctlup size or checksum mismatch.'
else
  printf 'ctl-published-stage-v1 downloading-updater\n'
  fetched=no
  if command -v curl >/dev/null 2>&1; then
    if (ulimit -f "$blocks" && exec curl -q --globoff --location --max-redirs 0 --proto '=https' --fail --silent --show-error --connect-timeout 10 --max-time 120 --max-filesize "$size" --output "$file" -- "$url") </dev/null; then
      fetched=yes
    else
      case $? in 1|2|5|6|7|18|22|28|52|56) :;; *) fail 'Remote curl could not verify or write ctlup.';; esac
    fi
  fi
  if [ "$fetched" = no ] && command -v wget >/dev/null 2>&1; then
    # wget's read timeout bounds individual reads. Limit the full command too.
    (ulimit -f "$blocks" && exec wget --no-config --no-netrc --no-hsts --max-redirect=0 --timeout=10 --tries=1 -q -O "$file" -- "$url") </dev/null &
    download_pid=$!
    (
      sleep 120 & sleeper=$!
      trap 'kill "$sleeper" 2>/dev/null; exit 0' TERM
      wait "$sleeper" && kill -TERM "$download_pid" 2>/dev/null
    ) </dev/null >/dev/null 2>&1 &
    timer_pid=$!
    code=0
    wait "$download_pid" || code=$?
    download_pid=''
    kill "$timer_pid" 2>/dev/null || :
    wait "$timer_pid" 2>/dev/null || :
    timer_pid=''
    case "$code" in 0) fetched=yes;; 2|4|8|143) :;; *) fail 'Remote wget could not verify or write ctlup.';; esac
  fi
  [ "$fetched" = yes ] || unavailable
  verified || fail 'Downloaded ctlup size or checksum differs from the publication.'
fi
chmod 700 "$file"
__CHECKOUT_LAYOUT_GUARD__
require_checkout_layout "$file"
code=0
help=$("$file" install-publication --help </dev/null 2>&1) || code=$?
if [ "$code" -ne 0 ]; then
  case "$code:$help" in 2:*"unrecognized subcommand 'install-publication'"*) unavailable;; esac
  fail 'Remote ctlup capability check failed.'
fi
set -- install-publication --origin-hex "$origin" --archive-url "$archive" --target "$target" --manifest-bytes "$manifest_bytes"
if [ "$local_runtime" = 1 ]; then
  printf 'ctl-published-stage-v1 uploading-runtime\n'
  set -- "$@" --archive-stdin
else
  printf 'ctl-published-stage-v1 installing-runtime\n'
fi
"$file" "$@"
