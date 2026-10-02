# Setup and troubleshooting

Use this reference when the executable, companion daemons, SSH authentication,
or a service protocol prevents the selected workflow.

## Locate the right components

Start with the installed `ctl` and its current help. If it is absent, report the
missing executable or use a known checkout build path. Build/install only when
the user's request includes setup or the work requires an authorized setup.

| Operation | Client-side companions | Controlled host |
| --- | --- | --- |
| Local exec/plain shell | None | Local program |
| Local persistent sessions | `ctmuxd` | Local machine |
| Local background tasks | `ctl-taskd` | Local machine |
| Local interactive tasks | `ctl-taskd`, `ctmuxd` | Local machine |
| Unix remote native commands / saved connections | `ctld` | SSH service |
| Remote sessions/tasks | `ctld` on Unix client | `ctl-agent`, `ctmuxd` and/or `ctl-taskd` |
| Managed ports / VPN | `ctld` on Unix client | SSH service for ports; local container engine for VPN |

Keep terminal/task companion versions aligned with the client. `CTLD_BIN`,
`CTMUXD_BIN`, and `CTL_TASKD_BIN` select explicit daemon executables; an override names an
executable, not a directory. On macOS, a standalone CLI otherwise prefers a
verified compatible shared `ctld.app`, then its own bundled helper, then beside
the client or on PATH, with a nearby desktop bundle taking precedence over loose
executables. The desktop continues to prefer its own bundled helper.
Terminal/task helpers normally resolve beside the client. Remote
companions must belong to the SSH account and be available together through
the managed installation or noninteractive PATH.

For an authorized published CLI setup on macOS:

```sh
cargo install --locked ctl-cli ctmuxd ctl-taskd
ctl setup
```

`ctl setup` is local and macOS-only. It installs the signed, notarized `ctld.app`
from the published GitHub release matching the installed CLI's version and
architecture. It cannot install a draft or select a different/latest release.
The bundle lives under
`~/.tokn/ctl/components/ctld/versions/<version>-<target>/ctld.app`; the component's
`current` symlink selects the release independently from the remote agent
installation, and an architecture/API selection selects the shared CLI helper.
The archive, Apple identity/profile, notarization, and helper build/APIs are
verified before selection. No `sudo`, daemon start, or automatic restart occurs.
Existing connections keep their running daemon, and `CTLD_BIN` remains an
override. `ctl setup --json` reports the version/path and whether setup reused an
existing installation. On other Unix platforms, install `ctld` from Cargo
alongside the CLI. Cargo compilation alone does not provide Apple's signing
identity or the macOS Keychain entitlement.

Shared discovery requires the native architecture, ctld protocol 12, lifecycle
protocol 1, and one-shot helper API 1. It verifies the app's build identity against
its own installation manifest; its release version and source fingerprint need
not match the CLI's. Signed development apps retain provisioning expiry and
certificate checks, use `development/<archive-sha256>/`, and update
`selected/<target>-ctld12-lifecycle1-helper1` without changing release `current`.
All standalone CLI builds, including ordinary Cargo builds, can reuse them.

Official macOS CLI downloads embed their matching signed helper. They prepare
it locally when no compatible shared app is selected and a command starts a
daemon or creates a fresh SOCKS/VPN proxy route; `ctl setup` uses the embedded
payload too. No helper download occurs. Existing
daemon/master reuse and passive status reads do not install a helper. Ordinary
Cargo builds contain no embedded helper and use shared discovery or explicit setup.

When working from the ctl source checkout, a local CLI/service build is:

```sh
cargo build -p ctl-cli -p ctld -p ctmuxd -p ctl-taskd -p ctl-agent
./target/debug/ctl --help
```

This builds artifacts; it does not install anything on a remote host. `ctl host
connect HOST` authenticates SSH without installing components.

## Diagnose by the failing boundary

- **Terminal required:** `ctl shell` needs terminal stdin and stdout. Use `exec`
  for a one-off command, detached session creation for a PTY, or a background
  task for lifecycle/logs. Attachment is an interactive stream, not a finite log
  query.
- **SSH authentication required:** prompts use the controlling terminal rather
  than command/file-transfer stdin. Unattended use needs working key/agent/
  certificate authentication, or an authenticated connection established in a
  terminal. Do not bypass host-key verification to make a retry succeed.
- **Unavailable method, VPN, or device:** inspect the saved host and chosen
  method. Saved Tailscale bindings resolve current discovery. Fix that selected
  route rather than substituting a destination or assuming an old address works.
- **Remote identity mismatch:** stop the service request and investigate the
  host/account/route. Do not erase a pinned identity as routine recovery.
- **Missing remote helper or protocol mismatch:** an interactive Unix service
  connection offers repair for a missing agent or recognized old `ctl-ssh-v2`
  protocol, checks any saved machine ID, installs verified matching components,
  and retries once. Confirm the repair only for the intended SSH account/route.
  Piped commands, unknown protocol markers, and later reconnects do not offer
  installation. A clean source build needs exact-revision bundles: run `pnpm
  agents:sync` from `apps/desktop`, or set `CTL_REMOTE_BUNDLES_DIR` to that bundle
  set. The CLI may download an existing verified exact-revision bundle artifact
  through `gh`; it never dispatches a workflow. A manually run `ctl-agent
  connect` waits for protocol input; it is not a finite health-check command.
- **Missing signed macOS helper:** for authorized local setup, run `ctl setup`.
  A missing release artifact requires the publisher to publish the matching
  signed release. Discovery can reuse another verified compatible installation;
  do not substitute an untrusted download or unsigned binary to bypass
  signature/profile/notarization failure.
- **Old daemon:** `ctl taskd restart` is local-only and refuses active tasks.
  Restarting `ctmuxd` terminates its terminals; inspect sessions before considering
  that disruptive recovery. Do not use an unrelated daemon restart to fix a
  catalog or authentication error.

For a Windows SSH service target, use `ctl -H HOST --remote-platform windows
ctmux ...` or `task ...`. Install the `.exe` companions together on the remote
PATH. The supported server shell is cmd.exe; custom shells and PowerShell are
not covered by this transport. Host connect/disconnect, managed ports, and
managed VPNs currently require a Unix client.
