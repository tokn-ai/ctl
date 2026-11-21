function Assert-CheckoutLayout($updater) {
  $preference = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  $version = $null | & $updater installation-layout-version 2>&1
  $code = $LASTEXITCODE
  $ErrorActionPreference = $preference
  [uint16]$parsed = 0
  if ($code -ne 0 -or ![uint16]::TryParse(($version -join "`n"), [ref]$parsed) -or $parsed -lt __LAYOUT_VERSION__) {
    throw 'The selected ctlup cannot use this installation layout. Select a build with a compatible updater.'
  }
}
