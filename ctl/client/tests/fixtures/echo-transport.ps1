$ErrorActionPreference = 'Stop'
$output_stream = [Console]::OpenStandardOutput()
$startup = [Text.Encoding]::UTF8.GetBytes([string]$env:CTL_TEST_STARTUP_NOISE)
$output_stream.Write($startup, 0, $startup.Length)
$marker = if ($env:CTL_TEST_TRANSPORT_MARKER) { $env:CTL_TEST_TRANSPORT_MARKER } else { 'ctl-ssh-v1' }
$preface = [Text.Encoding]::ASCII.GetBytes("$marker`n")
$output_stream.Write($preface, 0, $preface.Length)
$output_stream.Flush()
[Console]::OpenStandardInput().CopyTo($output_stream)
