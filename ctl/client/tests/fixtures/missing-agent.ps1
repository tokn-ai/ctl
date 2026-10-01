$output_stream = [Console]::OpenStandardOutput()
$preface = [Text.Encoding]::ASCII.GetBytes("ctl-ssh-nf`n")
$output_stream.Write($preface, 0, $preface.Length)
$output_stream.Flush()
[Console]::Error.WriteLine('bash: ctl-agent: missing in a localized shell')
exit 127
