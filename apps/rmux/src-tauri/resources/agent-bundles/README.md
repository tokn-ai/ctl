# Remote agent bundles

CI and `pnpm agents:sync` stage a validated `bundle-set.json` plus
`ctl-agent-bundle-<bundle-id>-<target>.tar.gz` archives and their `.sha256`
files here. Release bundle IDs equal the app version. Development IDs append
the source revision so each build has an immutable remote install directory.

Generated bundle files are ignored. A normal development start performs only a
local preflight; it never downloads artifacts or dispatches CI implicitly.
