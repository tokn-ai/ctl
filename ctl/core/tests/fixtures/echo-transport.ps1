$ErrorActionPreference = 'Stop'
$output_stream = [Console]::OpenStandardOutput()
$preface = [Text.Encoding]::ASCII.GetBytes("ctl-ssh-v1`n")
$output_stream.Write($preface, 0, $preface.Length)
$output_stream.Flush()
[Console]::OpenStandardInput().CopyTo($output_stream)
