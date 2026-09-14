$output_stream = [Console]::OpenStandardOutput()
$preface = [Text.Encoding]::ASCII.GetBytes("ctl-ssh-auth-v1`n")
$output_stream.Write($preface, 0, $preface.Length)
$output_stream.Flush()
[Console]::Error.WriteLine('ctl-agent: not found')
exit 127
