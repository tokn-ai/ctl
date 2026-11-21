function Get-UpdaterStaging {
  $path = $env:USERPROFILE
  foreach ($name in @('.tokn', 'ctl', 'ctlup', 'staging')) {
    $path = Join-Path $path $name
    if (![IO.Directory]::Exists($path)) {
      $null = [IO.Directory]::CreateDirectory($path)
    }
    if ([IO.File]::GetAttributes($path) -band [IO.FileAttributes]::ReparsePoint) {
      throw 'Installation staging path contains a reparse point.'
    }
  }
  return $path
}
