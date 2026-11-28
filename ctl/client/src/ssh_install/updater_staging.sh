require_private_owner_mode() {
  case "$(uname -s)" in
    Linux) metadata=$(stat -c '%u %a' "$1") ;;
    Darwin) metadata=$(stat -f '%u %Lp' "$1") ;;
    *) fail 'unsupported installation platform' ;;
  esac
  test "${metadata%% *}" -eq "$(id -u)" || fail 'installation path is not owned by this account'
  mode=$((0${metadata#* }))
  test "$((mode & 022))" -eq 0 || fail 'installation path is writable by group or others'
}

existing_updater_launcher() {
  updater_bin="$HOME/.tokn/ctl/bin"
  for directory in "$HOME" "$HOME/.tokn" "$HOME/.tokn/ctl" "$updater_bin"; do
    test ! -L "$directory" || fail 'installation directory is a symbolic link'
    test -e "$directory" || return 1
    test -d "$directory" || fail 'installation path is not a directory'
    require_private_owner_mode "$directory"
  done
  updater_entrypoint="$updater_bin/ctlup"
  updater_launcher="$updater_entrypoint"
  if [ -L "$updater_entrypoint" ]; then
    test "$(readlink "$updater_entrypoint")" = .ctl-launcher || fail 'updater entry point has an unexpected link target'
    updater_launcher="$updater_bin/.ctl-launcher"
    test ! -L "$updater_launcher" || fail 'updater launcher is a symbolic link'
  fi
  test -e "$updater_launcher" || return 1
  test -f "$updater_launcher" || fail 'updater launcher is not a regular file'
  require_private_owner_mode "$updater_launcher"
  test -x "$updater_launcher"
}

updater_tracks_channel() {
  "$updater_entrypoint" updater-channel-tracked </dev/null >/dev/null 2>&1
}

prepare_updater_staging() {
  updater_staging="$HOME/.tokn/ctl/ctlup/staging"
  for directory in "$HOME" "$HOME/.tokn" "$HOME/.tokn/ctl" "$HOME/.tokn/ctl/ctlup" "$updater_staging"; do
    test ! -L "$directory" || fail 'installation directory is a symbolic link'
    mkdir -p "$directory"
    test -d "$directory" || fail 'installation path is not a directory'
    require_private_owner_mode "$directory"
  done
}
