$inputStream = [Console]::OpenStandardInput()
$outputStream = [Console]::OpenStandardOutput()
$preface = [Text.Encoding]::ASCII.GetBytes("ctl-ssh-v2`n")
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
