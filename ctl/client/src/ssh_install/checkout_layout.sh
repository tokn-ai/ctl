require_checkout_layout() {
  checkout_root="$HOME/.tokn/ctl"
  if [ ! -e "$checkout_root/development.json" ] && [ ! -L "$checkout_root/development.json" ] &&
     [ ! -e "$checkout_root/.development-transaction.json" ] && [ ! -L "$checkout_root/.development-transaction.json" ]; then
    return 0
  fi
  checkout_version=$("$1" installation-layout-version </dev/null 2>/dev/null) || checkout_version=''
  if [ "$checkout_version" -ge __LAYOUT_VERSION__ ] 2>/dev/null; then return 0; fi
  printf '%s\n' 'The selected ctlup cannot preserve checkout installations. Select a build with a compatible updater.' >&2
  exit 1
}
