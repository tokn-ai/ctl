$inputStream = [Console]::OpenStandardInput()
$outputStream = [Console]::OpenStandardOutput()
$startup = [Text.Encoding]::UTF8.GetBytes([string]$env:CTL_TEST_STARTUP_NOISE)
$outputStream.Write($startup, 0, $startup.Length)
if ($env:CTL_TEST_AUTHENTICATION_MARKER) {
  $authenticated = [Text.Encoding]::ASCII.GetBytes("ctl-ssh-auth-v1`n")
  $outputStream.Write($authenticated, 0, $authenticated.Length)
}
$marker = if ($env:CTL_TEST_IDENTITY_MARKER) { $env:CTL_TEST_IDENTITY_MARKER } else { 'ctl-ssh-identity' }
$preface = [Text.Encoding]::ASCII.GetBytes("$marker`n")
$outputStream.Write($preface, 0, $preface.Length)
if ($marker -eq 'ctl-ssh-identity') {
  $offer = [Text.Encoding]::UTF8.GetBytes('{"build":3,"version":"1.0.3","supported_versions":["1.0.3"]}')
  $offerSize = [BitConverter]::GetBytes([uint32]$offer.Length)
  if ([BitConverter]::IsLittleEndian) { [Array]::Reverse($offerSize) }
  $outputStream.Write($offerSize, 0, $offerSize.Length)
  $outputStream.Write($offer, 0, $offer.Length)
  $outputStream.Flush()
  $remaining = [int]$env:CTL_TEST_SELECTION_BYTES
  $selection = New-Object byte[] $remaining
  while ($remaining -gt 0) {
    $count = $inputStream.Read($selection, 0, $remaining)
    if ($count -eq 0) { exit 1 }
    $remaining -= $count
  }
  if ($env:CTL_TEST_DAEMON_FAILURE) {
    [Console]::Error.WriteLine('fixture companion daemon failed')
    exit 1
  }
}
$metadata = [Text.Encoding]::UTF8.GetBytes($env:CTL_TEST_IDENTITY_JSON)
$size = [BitConverter]::GetBytes([uint32]$metadata.Length)
if ([BitConverter]::IsLittleEndian) { [Array]::Reverse($size) }
$outputStream.Write($size, 0, $size.Length)
$outputStream.Write($metadata, 0, $metadata.Length)
$outputStream.Flush()
$buffer = New-Object byte[] 8192
while (($count = $inputStream.Read($buffer, 0, $buffer.Length)) -gt 0) {
  $outputStream.Write($buffer, 0, $count)
  $outputStream.Flush()
}
