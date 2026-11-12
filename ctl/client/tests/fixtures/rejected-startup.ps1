$inputStream = [Console]::OpenStandardInput()
$outputStream = [Console]::OpenStandardOutput()
$marker = [Text.Encoding]::ASCII.GetBytes("ctl-ssh-identity`n")
$outputStream.Write($marker, 0, $marker.Length)
$outputStream.Flush()
$buffer = New-Object byte[] 1024
while ($inputStream.Read($buffer, 0, $buffer.Length) -gt 0) {}
[Console]::Error.WriteLine('startup input closed')
