# Remote agent bundles

CI and `pnpm bundles:sync` stage a validated `bundle-set.json` plus
`ctl-agent-bundle-<bundle-id>-<target>.tar.gz` archives and their `.sha256`
files here, together with each target's standalone `ctlup` executable,
checksum and `ctlup-<target>.json` manifest. Builds for Windows x64 also include
`bundle-set-windows.json`, a runtime archive and a standalone `ctlup` with an
`.exe` suffix. Release bundle IDs equal the app version. Nightly and PR bundle
IDs include the source revision so each build has an immutable remote install
directory.

Generated bundle files are ignored. A normal development start performs only a
local preflight; it never downloads artifacts or dispatches CI implicitly.
`pnpm bundles:sync` only stages these verified packaging resources. It does not
import an account cache, select an upload profile, install commands or activate
a runtime. Deployment caches are populated on demand by `ctlup` or the desktop.
