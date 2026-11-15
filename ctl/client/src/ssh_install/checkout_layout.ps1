function Assert-CheckoutLayout($updater) {
  $root = Join-Path $env:USERPROFILE '.tokn\ctl'
  $paths = @((Join-Path $root 'development.json'), (Join-Path $root '.development-transaction.json'))
  if (!(Test-Path -LiteralPath $paths | Where-Object { $_ })) { return }
  $preference = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  $version = $null | & $updater installation-layout-version 2>&1
  $code = $LASTEXITCODE
  $ErrorActionPreference = $preference
  [uint16]$parsed = 0
  if ($code -ne 0 -or ![uint16]::TryParse(($version -join "`n"), [ref]$parsed) -or $parsed -lt __LAYOUT_VERSION__) {
    throw 'The selected ctlup cannot preserve checkout installations. Select a build with a compatible updater.'
  }
}
