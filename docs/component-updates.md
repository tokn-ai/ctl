# Components and updates

Open **About ctmux** to inspect components on this computer and saved remote
hosts. **Running** identifies the existing process; **On disk** identifies the
selected executable. Build IDs distinguish development binaries that share a
product version. Different builds do not establish which one is newer.

Local components stay expanded. Remote hosts start as a compact table with
summaries of outdated builds, protocol mismatches, and required restarts.
Expand one remote host at a time to see its components. Matching reported
running and installed builds share one row; different builds show separate
**Running** and **On disk** rows. Unknown or stopped running builds are hidden,
leaving the on-disk build and component status. Active-connection metadata
remains labeled **Last observed**. Shared SSH inspection failures appear once
on the host with a short message; hover it for the full diagnostic.

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
Connection status is separate from inspection support. A **Connected** host can
show **Update agent** when its installed agent lacks maintenance contract
`1.1.3`, which adds companion inspection. Select and upload a complete bundle
that supports this contract; reconnecting the same older agent cannot add it.
The installed agent's own build remains visible when it can be read. A connected
host without a verified saved identity asks for **Check host** to verify its account.
A legacy numeric protocol is labeled **legacy**, independently of published
contract versions.

## Complete build bundles

All managed builds live under `~/.tokn/ctl/components`. A bundle contains
`ctl-agent`, `ctmuxd`, `ctl-taskd`, and `ctld` from the same build. It records its
source (CI, release, or local build), target, component contracts, and SHA-256
checksums. ctl checks both client compatibility and the contracts between the
components. Foreign-target binaries are never executed during import.

## Update components

Use **Update components…** in the command palette, About, or a saved host's
settings. Context actions preselect the host; the dialog can include this
computer and several saved SSH hosts. Choose one of two packages:

- **ctl-agent only** installs the agent and keeps the installed daemons.
- **Full bundle** installs `ctl-agent`, `ctld`, `ctmuxd`, and `ctl-taskd` from one
  verified complete build.

Both choices take their binaries from a complete compatible build. The default
uses each target's pinned selection. **Provided build files** accepts a stored
bundle directory or archive, a CI/release bundle-set directory, or an explicitly
chosen native build directory. Publisher archives need their matching
`bundle-set.json` beside them. A bundle-set directory can supply different target
artifacts for a batch; a single-target build cannot be uploaded to another target.
Native builds must contain all four binaries; local macOS full updates also need
the signed helper package described below. Supplied upload builds do not change
the pinned upload selection. A local full update selects that build for future
local service launches.

The CLI uses the same package verification and installation policy:

```sh
# No host flags means this computer.
ctl components update
ctl components update --hosts work,jump-a --local --package ctl-agent
ctl --host work --method vpn components update --package full-bundle
ctl components update --hosts work,jump-a --from /path/to/bundle-set --json
ctl components update --local --from target/debug --local-build --ctld-package /path/to/ctld-package
```

Saved hosts use their preferred connection method and existing route. `--host`
also accepts an SSH alias or destination; `--method` applies to that one host.
The app reports progress and authentication prompts per host. A failed host
does not undo earlier successes or skip later hosts; the dialog can retry just
the failed hosts. CLI JSON contains each host's result, and any failure gives a
nonzero exit status. Stop/Ctrl-C skips remaining hosts. If activation raced with
cancellation, refresh status before retrying.

Updates preserve running services and sessions. **Restart** remains a separate
confirmed action, and **Reconnect** applies an installed agent to existing SSH
channels. An agent-only update cannot upgrade a daemon's contracts. Agent-only
installations live under `~/.tokn/ctl/components/agents/<target>/`, record their
complete source in `agent-source.json`, and link retained daemons to their
previous immutable locations. They do not claim to contain the complete bundle.

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
has its own complete selection. **About → Bundles** lists verified complete builds
included with the app alongside stored builds, without importing or selecting
anything during inspection. Identical included and stored builds share one row.
Separate local-service and remote-upload columns show supported choices and
explain unavailable uses, such as a missing signed macOS helper package.
Choosing an included build verifies and imports that exact content identity
before selecting it. Changing a selection verifies the complete build and
atomically updates its profile.
It preserves running services and sessions. New service launches and explicit
remote updates use that selection; restart and reconnect remain separate actions.

Selections never change because another build appears in the store or because
a connection is opened. The first remote Update can import and select a compatible
packaged or cached bundle when no upload selection exists. Subsequent updates
keep that selection until an explicit Sync or choice in About. A damaged selection
reports an error instead of silently choosing another build.

Import a downloaded CI/release bundle-set directory explicitly:

```sh
ctl components list
ctl components sync --from /path/to/bundle-set --target x86_64-unknown-linux-musl --purpose upload --source ci
ctl components list --target x86_64-unknown-linux-musl --json
ctl components select <content-id> --target x86_64-unknown-linux-musl --purpose upload
```

`ctl components list` includes packaged builds and imported builds across all
supported targets. It labels included/stored availability and local/upload
selections, deduplicates identical builds, and reports a clear message when none
are available. `--target` filters the inventory. `--json` retains complete
manifests in `bundles` and adds `availability`, per-target `selections`, and
inspection `errors`; `target_triple` is null for an unfiltered inventory.
Listing is read-only. Selecting a listed included build imports its exact
verified content before changing the requested selection.

`pnpm bundles:sync` also imports and selects all four CI upload targets in this
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
2. Use **Update…** to open the shared updater for this account. ctl
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

For this computer, use **Update components…** or select a complete local build
in Bundles, then use the separate confirmed restart actions. With no selection, existing desktop/helper discovery
continues. Explicit executable environment overrides retain their diagnostic use. Remote broker and task-daemon
status is displayed alongside the terminal daemon; their remote restart actions
are not exposed. A legacy owner without cooperative restart support requires a
manual restart after its work can be ended.

If activation cannot be verified, ctl reports that installation may have
completed and asks you to check the host. It does not retry a destructive action
or stop a daemon automatically.

## Inspect and restart from the CLI

```sh
ctl components status
ctl components status --json
ctl components restart ctld --dry-run
ctl components restart ctld
ctl components restart ctmuxd
ctl components restart ctl-taskd
ctl -H work components status
ctl -H work components restart ctmuxd
```

`status` compares each running owner's full build identity and protocol advertisements
with the installed replacement selected for this CLI. Release versions alone do not
identify builds or protocol compatibility. Human output shows a short source fingerprint;
`--json` preserves complete build and protocol metadata. Unknown or legacy metadata is
not treated as an up-to-date build. Inspection failures are reported for each component,
with a nonzero exit status, while successful rows remain visible.

Local status only probes existing service endpoints and runs bounded `--component-info`
queries. It does not install embedded payloads or start a daemon. Remote status can
establish the saved host's SSH/VPN route, verifies its machine identity, and inspects
companions without starting those remote services. The remote agent is on demand;
its inspection process does not describe every active terminal transport.

`restart` verifies the selected replacement and pins the existing owner before showing
its impact and asking for confirmation. Use `--dry-run` to inspect the plan without
mutation, or `--yes` to explicitly approve it in scripts. `--json` does not imply consent.
Confirmation expires after 20 seconds; an expired plan must be prepared again. Restarts
use cooperative shutdown and verify the successor, without force-killing or automatically
retrying an uncertain mutation. An absent owner is not started by this command.

Restarting **ctmuxd ends all of its sessions and panes**, including other clients and
interactive tasks. Restarting ctld interrupts its clients and VPN connections. Taskd
refuses restart while tasks are active. Each local command addresses the CLI's selected
endpoint (`CTLD_SOCKET_PATH`, `CTMUX_RUNTIME_DIR`, or `CTL_TASKD_RUNTIME_DIR`), rather than
every daemon process on the machine. Remote restart currently supports only ctmuxd;
ctld and ctl-taskd are rejected before connecting.

Sync/import and update/install continue to preserve running owners. Restart is a separate,
explicit step after selecting and installing a build. These CLI operations use existing
lifecycle and remote maintenance contracts; they add no protocol version or build changes.
