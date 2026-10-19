# Windows support and CI

Windows client support and Windows SSH target support are separate: the client
platform determines local IPC and process handling; `--remote-platform windows`
selects the command understood by a Windows SSH server. The default remote
platform is Unix, even when the client runs on Windows.

## Supported paths

| Path | Windows behavior | Native CI coverage |
| --- | --- | --- |
| Local `ctl exec` | Runs a program with inherited streams and preserves its exit status | Binary stdin/stdout/stderr and nonzero exit status |
| `ctl ssh` / `ctl scp` | Uses system OpenSSH and its configuration | Real `ssh -G` checks alias settings and explicit overrides |
| Remote `ctl exec` | Uses system SSH; `--remote-platform windows` selects cmd.exe command construction | Authenticated loopback stdin/stdout/stderr and exit status |
| Local `ctl ctmux` / `ctmux` | Named-pipe transport and ConPTY sessions | Auto-start, attachment, reconnect, resize, final output and exit status |
| Local `ctl task` | Named-pipe transport, background Job Objects and managed ConPTY tasks | Auto-start, lifecycle, logs, recovery and descendant cleanup |
| Local `ctld` | Owner-only named pipe, daemon lifecycle and available local VPN services | Detached startup, status without a container engine, cooperative restart/stop and pipe reuse |
| Local SSH forwards | ctld owns loopback listeners and native OpenSSH connections | Authenticated binary transfers, configuration aliases and proxies, enable/update/disable, failure isolation and owner cleanup |
| Remote ctmux | `ctl-agent.exe connect` routes to `ctmuxd.exe` | Authenticated loopback auto-start, reconnect and both destination selectors |
| Remote tasks | `ctl-agent.exe connect --service task` routes to `ctl-taskd.exe` | Authenticated loopback lifecycle, log streams, working directory and persistence across channels |

`--host` / `-H` selects a saved ctl host by name or ID. `--destination` selects an
IP address, DNS name or OpenSSH `Host` alias directly without reading or changing
the ctl catalog. Both retain OpenSSH configuration; when an alias is selected, an
explicit hostname override changes `HostName` while retaining the alias's
identity, user, port and proxy settings.
The Windows SSH fixture covers saved hosts, temporary aliases, colliding names,
and a corrupt catalog. A missing saved `--host` is rejected before SSH starts.

Windows clients use direct SSH for remote connections. `ctld` runs as a local
owner on Windows and serves daemon lifecycle and local VPN operations as their
dependencies permit. Local forwarding uses ctld-owned listeners and native SSH
streams. OpenSSH control-master connection management and remote-listener
discovery return an explicit platform diagnostic; ordinary direct SSH commands
remain available. macOS Keychain operations remain macOS-only.
The desktop backend and `ctld` participate in the whole-workspace Windows checks.

## Native CI gate

The [CI workflow](../.github/workflows/ci.yml) runs the same whole-workspace Rust
checks on native platforms through the shared
[check entry point](../scripts/ci/check.mts):

```sh
node scripts/ci/check.mts quality
node scripts/ci/check.mts rust
```

The quality command checks workspace formatting and runs Clippy over every
workspace target with warnings denied on Linux and Windows. The test command
links and executes workspace tests on Linux, macOS and `windows-2025`, collecting
failures across all test binaries. Windows uses the MSVC Rust toolchain and Node
24. New workspace crates enter these checks automatically; there is no package
allowlist or separate Windows test command.

Native dependencies and adapters are gated at their actual platform boundary.
The macOS Keychain framework and notification adapter remain macOS-only, while
its session revocation/resume state machine runs in the shared test suite.
Shell/FIFO and Unix socket tests stay Unix-only; Windows runs its native named
pipe, Job Object and ConPTY integration tests alongside the shared suites.
Compilation alone does not establish runtime support, and WSL does not exercise
these Windows paths.

A separate Windows job builds executables and runs
[ctld lifecycle](../scripts/ci/windows-ctld.ps1),
[task](../scripts/ci/windows-tasks.ps1),
[terminal](../scripts/ci/windows-ctmux.ps1) and
authenticated [SSH](../scripts/ci/windows-ssh.ps1) fixtures using PowerShell.
The SSH test requiring server setup is explicitly invoked with `--ignored` by
its fixture, rather than counted as covered by ordinary `cargo test`.

## Windows local ctld owner

`ctld` listens on a local, owner-only named pipe derived from its runtime
namespace, and supports explicit `--socket` pipe names. Its lifecycle endpoint
supports inspection, cooperative restart and confirmed stop. Windows removes the endpoint when
handles close; startup uses an exclusive first pipe instance rather than
filesystem socket cleanup.

Optional local VPN dependencies are discovered when needed. Missing container
engines produce discovery diagnostics without preventing daemon startup or
status requests. The native daemon suite starts `ctld.exe` detached with an empty
program search path, checks repeated VPN status requests and platform-specific
SSH diagnostics, then restarts the owner and reuses its pipe name. Actual VPN
connectivity still requires a supported container engine and configuration.
A separate CLI smoke fixture verifies passive status/list operations, a restart
dry run, owner replacement through `ctl components restart ctld`, VPN
status after reconnecting to the successor, and graceful, idempotent
`ctl components stop ctld`. Native tests also check abandoned and expired stop
confirmations. The fixture isolates its helper executable, endpoint, host catalog
and VPN profile selections from other daemon owners.

## Windows local SSH forwards

`ctl --host HOST port add ...` and `ctl --destination ALIAS port add ...` use the
same ctld forwarding contract as Unix. Windows ctld owns the local TCP listener,
a native `ssh -N -T` authentication/health connection, and a separate native
`ssh -W host:port` stream for each accepted TCP connection. This supports local
forwarding without requiring OpenSSH control-master multiplexing.

Every SSH process reads the original SSH configuration, retaining `Host` alias
selection, hostname overrides, user, port, identity files, `ProxyCommand` and
`ProxyJump`; ctld does not reconstruct an `ssh -G` snapshot. Authentication is
noninteractive, using credentials already available to system OpenSSH. ctld
reserves `LocalCommand` for its internal post-authentication readiness nonce and
passes `ClearAllForwardings=yes` so unrelated configured local, remote or dynamic
forwards do not start. The owned listener passes bytes without
constructing a remote shell command. Local `port list` observes owned state and
does not begin authentication. Enabling, editing or explicitly retrying a
forward can start authentication; listing does not retry a stopped connection.

Each forward owns its SSH process tree through a Windows Job Object. Disabling a
forward or stopping ctld releases its listener and owned connections. If ctld
crashes, closing its job handles terminates SSH and proxy descendants. Updating
an existing forward ID replaces its previous definition, matching Unix behavior;
a failed replacement reports an error. Failures for another ID do not authorize
closing existing forwards or an unrelated listener occupying a requested port.

The authenticated [native forwarding fixture](../ctl/daemon/tests/windows_ssh_forwarding.rs)
runs under `windows-ssh.ps1`, which enables only local TCP forwarding on its
loopback server. It exchanges binary data with two TCP endpoints to verify
updates, checks idempotent enable, list, disable and CLI routing through both
selectors, and verifies failed authentication and occupied ports leave another
forward working. Resetting a data connection must leave passive forwarding
status active and allow another connection without an explicit retry. A dedicated SSH alias exercises `IdentityFile`, user, a
nondefault port, a hostname override and `ProxyCommand`. Its configured unrelated
`LocalForward` must stay inactive. A backend that reads a request to EOF and
immediately returns binary data verifies TCP half-close, final response draining
and removal of internal readiness bytes. The fixture records the actual SSH
anchor, data and proxy descendant process IDs with their creation times, then
requires those same process identities, active channels and listeners to vanish
after cooperative stop and abrupt ctld termination. This does not claim OpenSSH
connection multiplexing or automated coverage of every proxy topology.

A separate native test creates a saved SOCKS5 gateway route and launches ctld
from a path containing spaces, an apostrophe and a literal percent sign. It
exercises the generated helper `ProxyCommand` through CLI add/list/remove,
authenticated binary forwarding and cleanup of the SSH/helper descendants.

## Windows background tasks

`ctl-task-ipc`, `ctl-task-cli`, `ctl-taskd`, and `ctl` now cross-compile for Windows. The
native Windows job builds both executables, runs task integration tests, and
exercises `ctl task` auto-start, create, logs, restart, stop, remove, and list.

ctl-taskd uses local named pipes via `interprocess`. The default name is a stable
UUID derived from `%LOCALAPPDATA%\ctl-taskd`, or `CTL_TASKD_RUNTIME_DIR` when
set. On Windows that override is a namespace seed, not a socket directory.
`ctl-taskd --socket` accepts an explicit `\\.\pipe\...` name. First-instance
creation prevents competing listeners; Windows removes the endpoint when its
handles close. The pipe rejects remote clients and uses an owner-only DACL
(`D:P(A;;GA;;;OW)`), rather than the Windows default pipe ACL, which includes
read access for Everyone. See [Microsoft's named-pipe security documentation](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights).

Background processes use `process-wrap` Job Objects with kill-on-close.
Creation is suspended until job assignment, preventing a process from launching
children before ownership is established. Stop terminates the whole job.
Completion follows the root process and terminates remaining descendants,
then drains stdout/stderr before publishing the final run state. Taskd waits
on the root using Tokio's cancellation-safe wait; it does not cancel the
wrapper's blocking job-completion wait. See [process-wrap's Job Object API](https://docs.rs/process-wrap/10.0.0/process_wrap/tokio/struct.JobObject.html).

State defaults to `%USERPROFILE%\.tokn\ctl\taskd` and inherits directory ACLs.
`CTL_TASKD_DATA_DIR` and `--data-directory` overrides must point to a private user
directory. An exclusive lifetime file lock prevents concurrent state writers.
The temporary state file is replaced with `std::fs::rename`, which supports
replacing an existing file on Windows. See [Rust's rename documentation](https://doc.rust-lang.org/std/fs/fn.rename.html).
State is not fsynced; sudden power-loss durability remains outside this slice.

On daemon restart, previously active runs are marked failed/stopped; there is
no automatic restart or adoption. Windows closes job handles on daemon crash,
terminating descendants. Native tests verify this by holding a file exclusively
in a descendant and checking that stop/crash releases it. Other tests verify
stdout/stderr tail output, completion, and metadata recovery after restart.

`ctl` locates sibling `ctl-taskd.exe`, starts it detached, and records the caller's
working directory when creating a definition. Background console programs use
`CREATE_NO_WINDOW`. Commands are passed as a program plus arguments; ctl-taskd does
not insert a shell. Invoke `cmd.exe` or PowerShell explicitly when needed.

## Windows local terminal sessions

`ctmuxd` now uses `portable-pty`'s ConPTY backend, with the same journal,
checkpoints, leases, flow control, and reconnect protocol as Unix. The local
`ctmux` CLI and `ctl ctmux` can create, list, attach to, inspect, and terminate
sessions. The daemon auto-start path locates `ctmuxd.exe` and detaches it from
the invoking console.

The data and owner-only maintenance endpoints are separate local named pipes,
both restricted by an owner-only DACL and rejecting remote clients. The
maintenance name adds `.control` to the data name. Its exclusive first instance
arbitrates concurrent startup; the data endpoint is published last, so a data
connection also implies maintenance support is available. `CTMUX_RUNTIME_DIR`
is a namespace seed on Windows, not a filesystem socket directory.

The daemon answers ConPTY's initial cursor-position query at the canonical
origin before publishing a new session. The bounded startup reader handles
fragmented queries and removes that transport handshake from replayed output;
a detached session does not need a renderer to finish starting.

A child waiter closes the pseudoconsole before joining the independent output
reader. Session kill also closes ConPTY on a separate thread. This keeps output
draining while `ClosePseudoConsole` runs, avoiding the shutdown deadlock on older
Windows versions. See [Microsoft's ClosePseudoConsole documentation](https://learn.microsoft.com/en-us/windows/console/closepseudoconsole).
PTY handles remain owned by ctmuxd throughout, including managed interactive tasks.

Attachment reads retain partial protocol frames across output and resize events;
Windows named-pipe delivery exposed a cancellation bug in the original reader.
A byte-by-byte cancellation regression test now runs on every platform.

Native CI exercises real ConPTY input/output, attachment resumption after a
connection loss, resize, final output and exit status, and owner-only daemon
restart. A separate smoke test checks `ctmuxd.exe` discovery, auto-start, and
local `ctl ctmux` routing. The existing Unix integration suite remains enabled
on Linux/macOS; its shell/FIFO-specific fixtures are not passed off as Windows
coverage. Manual Windows Terminal keyboard/rendering checks and cross-account
ACL tests remain useful follow-up validation.

Windows has no native process inspection or Bash/Zsh FIFO reporter in this
slice, so unavailable shell metadata stays unknown. Terminal output and
terminal-derived state continue to work.

## Windows SSH client routing

`ctl --host HOST ctmux ...` and `ctl --destination ALIAS ctmux ...` use the same
connector on Windows and Unix.
`ctl-client` local transports use the shared `ctmux-ipc::Stream`, so Windows local
commands retain named-pipe discovery and daemon auto-start. Remote connections
use the system `ssh` executable with binary stdin/stdout pipes. OpenSSH still
owns authentication and host verification. The agent gateway exposes its service
data endpoint; local SSH forwarding remains owned by the client's ctld.

The default remote platform remains Unix (a fixed managed-directory `PATH`
prefix followed by `exec ctl-agent connect`). CLI clients
can select `--remote-platform windows` to use the fixed `ctl-agent.exe connect`
command with the Windows OpenSSH server's default cmd.exe shell. The platform
is independent of the client OS; arbitrary shell commands remain disallowed.
The desktop currently selects Unix remote commands.

[Proposal 0006](proposals/0006-remote-tasks.md) extends this convention to tasks:
`ctl --host HOST --remote-platform windows task ...` selects
`ctl-agent.exe connect --service task`. The companion is `ctl-taskd.exe`, while
interactive attachment opens the ordinary ctmux channel on the same target.
The native SSH fixture exercises both services and both destination selectors.

Windows SSH targets need `ctl-agent.exe` and the relevant companion executables
installed on the server's `PATH`. Daemon auto-start is supported after installation;
automatic remote component installation and repair currently target Unix hosts.

`ctl-agent` uses the shared IPC stream and only the data endpoint. Its installed
companion is `ctmuxd.exe` on Windows. Startup requests both `DETACHED_PROCESS`
and `CREATE_BREAKAWAY_FROM_JOB`: Windows OpenSSH permits breakaway so the
persistent daemon survives the disposable channel. If the enclosing job
disallows breakaway, startup reports an error rather than creating a daemon
that silently dies on disconnect. Standard streams are null on the daemon.
See [OpenSSH job breakaway support](https://github.com/PowerShell/Win32-OpenSSH/issues/1032).

Native CI runs portable gateway tests over owner-restricted named pipes,
including byte transparency and endpoint survival. `windows-ssh.ps1` configures
an authenticated loopback OpenSSH server on the ephemeral Windows runner using
ephemeral client/host keys, an explicitly pinned host key, local TCP forwarding
only, and the default cmd.exe shell. Password authentication is disabled except
for a fixture-created local user with a random password, used to verify
caller-owned password channels; the user is removed during cleanup. It installs companion executables, then tests
daemon auto-start, ConPTY creation, attachment, SSH disconnect/reconnect,
resize, final output and exit status, and CLI routing. It also exercises remote
exec streams and task create/show/start/logs/restart/stop/remove/list, including a
working directory containing spaces. Caller-owned authentication tests verify an
explicit key, rejection of an incorrect key despite a valid configured identity,
and Bun-driven password login with stdin streaming, exit status and plain shell
behavior. Passwords enter that fixture only through stdin, rather than argv or
environment. The task daemon has isolated state and is
prestarted on the default user endpoint, because SSH sessions do not inherit the
fixture environment; task daemon auto-start is covered by the local task fixture.
The SSH fixture restores configuration and removes its processes, binaries, and
keys afterward. The script refuses to
run outside GitHub Windows runners. See Microsoft's
[Windows OpenSSH server configuration](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh-server-configuration).

PowerShell/custom SSH shells and connections between different machines are
not covered by the loopback fixture.

## Remaining Windows work

- Remote interactive-task attachment over SSH. Local managed ConPTY tasks and
  remote ctmux attachment are covered separately.
- Desktop remote-platform selection.
- Native process inspection and Windows desktop bundle installation checks.
- Cross-account ACL tests and filesystem ACL hardening for custom data paths.
- Persistent logs, restart policies, workspace task integration, and other
  unfinished task features shared with Unix.

Whole-workspace Rust checks cover the desktop backend; producing and installing
a Windows desktop bundle remains separate validation.

## Interactive managed tasks

The ctl-taskd integration suite also runs against real ConPTY sessions on Windows.
It verifies task start/stop/restart, normal ctmux attachment and input/output,
recovery after ctl-taskd restart, retained exit outcomes, idempotent creation, and
failure of old runs after replacing ctmuxd. Interactive runs use ctl-taskd lifecycle
records and ctmuxd process ownership; their output is not a ctl-taskd log stream.
