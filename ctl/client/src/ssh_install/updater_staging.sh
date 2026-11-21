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

prepare_updater_staging() {
  updater_staging="$HOME/.tokn/ctl/ctlup/staging"
  for directory in "$HOME" "$HOME/.tokn" "$HOME/.tokn/ctl" "$HOME/.tokn/ctl/ctlup" "$updater_staging"; do
    test ! -L "$directory" || fail 'installation directory is a symbolic link'
    mkdir -p "$directory"
    test -d "$directory" || fail 'installation path is not a directory'
    require_private_owner_mode "$directory"
  done
}
