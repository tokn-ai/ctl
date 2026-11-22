require_checkout_layout() {
  checkout_version=$("$1" installation-layout-version </dev/null 2>/dev/null) || checkout_version=''
  if [ "$checkout_version" -ge __LAYOUT_VERSION__ ] 2>/dev/null; then return 0; fi
  printf '%s\n' 'The selected ctlup cannot use this installation layout. Select a build with a compatible updater.' >&2
  exit 1
}
