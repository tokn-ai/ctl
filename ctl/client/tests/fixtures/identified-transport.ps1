$inputStream = [Console]::OpenStandardInput()
$outputStream = [Console]::OpenStandardOutput()
$startup = [Text.Encoding]::UTF8.GetBytes([string]$env:CTL_TEST_STARTUP_NOISE)
$outputStream.Write($startup, 0, $startup.Length)
if ($env:CTL_TEST_AUTHENTICATION_MARKER) {
  $authenticated = [Text.Encoding]::ASCII.GetBytes("ctl-ssh-auth-v1`n")
  $outputStream.Write($authenticated, 0, $authenticated.Length)
}
$marker = if ($env:CTL_TEST_IDENTITY_MARKER) { $env:CTL_TEST_IDENTITY_MARKER } else { 'ctl-ssh-v3' }
$preface = [Text.Encoding]::ASCII.GetBytes("$marker`n")
$outputStream.Write($preface, 0, $preface.Length)
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
