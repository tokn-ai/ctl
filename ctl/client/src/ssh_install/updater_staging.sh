prepare_updater_staging() {
  updater_staging="$HOME/.tokn/ctl/ctlup/staging"
  for directory in "$HOME" "$HOME/.tokn" "$HOME/.tokn/ctl" "$HOME/.tokn/ctl/ctlup" "$updater_staging"; do
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
}
