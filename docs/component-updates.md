# Components and updates

Open **About ctmux** to inspect components on this computer and saved remote
hosts. **Running** identifies the existing process; **On disk** identifies the
selected executable. Build IDs distinguish development binaries that share a
product version. Different builds do not establish which one is newer.

Hosts start collapsed with a summary of outdated builds, protocol mismatches,
and required restarts. Expand a host to see its components. Matching verified
running and installed builds share one row; different or unverified builds show
separate **Running** and **On disk** rows, including their build identities and
protocols. Active-connection metadata remains labeled **Last observed**.

Each protocol has its own line and status: a green tick for the app's current
contract, a yellow tick for a different contract that shares an explicitly
supported version, and a red cross for no shared contract. Unreported protocols
and historical numeric protocols have a neutral question mark. The tooltip
shows the supported and required contracts; matching major versions alone do
not imply compatibility.

Refresh reuses existing authenticated SSH connections and checks service owners
without attaching a terminal, starting a daemon or VPN, or installing files.
An offline saved host stays visible as **Not checked**, with unknown running
versions. Saved identity metadata is never presented as a live process version.
A legacy numeric protocol is labeled **legacy**, independently of published
contract versions.

## Complete build bundles

All managed builds live under `~/.tokn/ctl/components`. A bundle contains
`ctl-agent`, `ctmuxd`, `ctl-taskd`, and `ctld` from the same build. It records its
source (CI, release, or local build), target, component contracts, and SHA-256
checksums. ctl checks both client compatibility and the contracts between the
components. Foreign-target binaries are never executed during import.

```
~/.tokn/ctl/components/
  bundles/<target>/<content-id>/bundle.json
  bundles/<target>/<content-id>/<component files>
  selected/local-<platform>.json
  selected/upload-<target>.json
```

One complete bundle is selected for local services. The local profile uses the
native target on macOS and the architecture’s portable musl target on Linux,
so GNU and musl clients share the same selection. Each remote upload target
has its own complete selection. **About → Bundles** lists verified stored builds
and offers **Use locally** and **Use for uploads** where supported. Changing a
selection verifies the complete build and atomically updates its profile.
It preserves running services and sessions. New service launches and explicit
remote updates use that selection; restart and reconnect remain separate actions.

Selections never change because another build appears in the store or because
a connection is opened. The first remote Update can import and select a compatible
packaged or cached bundle when no upload selection exists. Subsequent updates
keep that selection until an explicit Sync or choice in About. A damaged selection
reports an error instead of silently choosing another build.

Import a downloaded CI/release bundle-set directory explicitly:

```sh
ctl components sync --from /path/to/bundle-set --target x86_64-unknown-linux-musl --purpose upload --source ci
ctl components list --target x86_64-unknown-linux-musl --json
ctl components select <content-id> --target x86_64-unknown-linux-musl --purpose upload
```

`pnpm agents:sync` also imports and selects all four CI upload targets in this
shared store, after copying the verified resources needed to package the desktop.
Use `--source release` when importing a release bundle-set. Schema-2 manifests
must advertise all four components; legacy schema-1 artifacts cannot become a
managed complete selection. Existing installations and helper caches are retained.

For a native local build:

```sh
cargo build --locked -p ctl-agent -p ctmuxd -p ctl-taskd -p ctld
ctl components sync --from target/debug --local-build --purpose local
```

For local use on macOS, supply `--ctld-package /path/to/provisioned-helper-package` as well.
That directory must contain the complete signed `ctld.app` and its
`installation.json` receipt from `ctl setup`. ctl verifies the package using
its existing signing and provisioning checks, retains the whole app, and rejects
mixing it with companions from a different build. Current CI agent archives
contain flat binaries and are available for uploads on macOS; they lack the
provisioned app required for local use. On GNU Linux, a matching static musl build
can also be selected locally; a musl host cannot assume GNU dynamic libraries.

Uploads use `~/.tokn/ctl/components/bundles/<target>/<content-id>` on the remote
account and atomically switch `~/.tokn/ctl/current`. Unchanged installed content
is reused. Archive checksum failure or changed same-ID content keeps the previous
activation. Concurrent remote sync attempts fail without changing it. If a remote
install shell is forcibly killed, its `.sync-lock` directory may need removal
after confirming no sync is running. Ordinary errors and cancellations clean it.

For a saved remote host:

1. Use **Check host** to authenticate through its preferred connection method
   and route, then refresh component status. This does not open a terminal.
2. Use **Update…** to install the verified remote bundle for this account. ctl
   verifies the saved account before uploading, activates an immutable bundle,
   and verifies that the installed agent reports the selected bundle afterward.
   A compatible cached development bundle may differ from the desktop's source
   revision; the result describes the bundle actually installed.
3. If **Restart required** appears, choose **Restart** on the terminal daemon.
   ctl prepares the installed replacement and reports the impact before asking
   for confirmation. Restart ends every terminal owned by that remote account,
   including terminals in other windows and clients. Canceling keeps sessions.

Installation preserves running daemons and sessions. Existing SSH channels keep
an older agent process until **Reconnect** applies the installed agent; remote
terminal sessions survive that reconnect. The inspection agent itself is an
on-demand process, not evidence that existing channels were updated.

For this computer, select a complete local build in Bundles, then use the separate
confirmed restart actions. With no selection, existing desktop/helper discovery
continues. Explicit executable environment overrides retain their diagnostic use. Remote broker and task-daemon
status is displayed alongside the terminal daemon; their remote restart actions
are not exposed. A legacy owner without cooperative restart support requires a
manual restart after its work can be ended.

If activation cannot be verified, ctl reports that installation may have
completed and asks you to check the host. It does not retry a destructive action
or stop a daemon automatically.
