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

Keep client and companion versions aligned. Helpers normally resolve beside
their client, with supported fallback locations; `CTLD_BIN`, `CTMUXD_BIN`, and
`CTL_TASKD_BIN` select explicit daemon executables. An override names an executable,
not a directory. Remote companions must belong to the SSH account and be
available together through the managed installation or noninteractive PATH.

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
- **Missing remote helper or protocol mismatch:** check the SSH account's
  noninteractive PATH and align components. The CLI has no remote installation
  subcommand. A manually run `ctl-agent connect` waits for protocol input; it
  is not a finite health-check command.
- **Old daemon:** `ctl taskd restart` is local-only and refuses active tasks.
  Restarting `ctmuxd` terminates its terminals; inspect sessions before considering
  that disruptive recovery. Do not use an unrelated daemon restart to fix a
  catalog or authentication error.

For a Windows SSH service target, use `ctl -H HOST --remote-platform windows
ctmux ...` or `task ...`. Install the `.exe` companions together on the remote
PATH. The supported server shell is cmd.exe; custom shells and PowerShell are
not covered by this transport. Host connect/disconnect, managed ports, and
managed VPNs currently require a Unix client.
