# Tests and CI

Use Node 24, pnpm 10, and the Rust toolchain pinned in `rust-toolchain.toml`.
On Linux, install the system dependencies listed in `.github/workflows/ci.yml`,
including `libxkbcommon-dev` for the ctream Wayland helpers.

On Linux or macOS, run the shared check entry point from the repository root:

```sh
node scripts/ci/check.mts all --allow-dirty
```

CI calls this same script with individual scopes: `scripts`, `quality`, `rust`, `frontend`, and `packages`.
Each scope stops at its first failure. Omit `--allow-dirty` for a clean checkout;
it only permits packaging uncommitted files and does not weaken verification.
`packages` packages and tests the extracted
published sources with all features; it is stronger than archive creation alone
and intentionally retains that separate gate. Formatting, Clippy, and workspace
tests use the pinned Rust 1.99.0 baseline. Published-source tests run on Rust
1.97.0, the minimum supported version, including outside the checkout. Rust
builds retain the checked-in lockfile.

Native tests cover the current operating system. Passing on macOS does not prove
Linux or Windows behavior. For platform-specific edits, also use a Linux runner
and the native Windows job. Cross-target Clippy/check can catch conditional-code
errors, but cannot prove subprocess, PTY, socket, or ConPTY behavior.

For timing-sensitive changes, run the affected integration suite repeatedly,
with its normal parallelism, and stop at the first failure. Do not retry a failed
suite until it turns green. A successful repetition supplements the ordinary
workspace and packaged-source checks; it does not replace them.

Test synchronization rules:

- PTY reads are arbitrary chunks. Wait for all expected menu content before
  inspecting it; a heading alone does not prove the options have arrived.
- Check focus through the live cursor and shell input when a temporary notice
  can replace the status text. Keep the process and reconnect barriers intact.
- Use deterministic clock tests for deadline policy. Real subprocess stream
  tests use real time for healthy completion; deliberate stall tests advance
  their test clock only after observing the phase being tested.
- Give each test private state. Serialize executable writes with child launches
  when parallel fork/exec could inherit an open writable descriptor.
- Prefer protocol readiness to socket/file existence. Preserve bounded waits
  and useful failure diagnostics rather than increasing timeouts globally.

## Executable fixtures

Fake Unix commands should use `ctl_core::test_fixtures::shell_command`. Enable
the `tokn-ctl-core` package's `test-fixtures` feature in **dev-dependencies**;
the workspace dependency alias is `ctl-core`. The command is a
symlink to the checked-in launcher; its script lives in `<command>.script` and
is read by `/bin/sh`. No test writes the executable inode, even when another
child inherits a writable script descriptor. `$0`, arguments, stdin, environment,
and exit status retain their usual command semantics. Never chmod this symlink,
use it to test canonical executable paths, or overwrite the shared launcher.

Tests that need actual executable files use the shared `ProcessGuard` instead:
`ProcessGuard::acquire().await` for async tests and `acquire_blocking()` for
synchronous tests. Acquire before writing or copying, and retain the guard until
children finish. Native copies use `guard.copy(source, destination)`. Other tests
that launch subprocesses in that same test binary must acquire the same guard,
even when launching an immutable system command: fork can inherit another test's
writable executable descriptor before exec. This coordinates one OS process;
separate Cargo integration-test executables do not share file descriptors.
Do not nest acquisitions or block a Tokio runtime with `acquire_blocking()`.

The fixture audit keeps real files for these reasons:

| Fixtures | Reason |
| --- | --- |
| CLI skills, agent sibling discovery, daemon proxy helper, desktop bundle discovery | Copy a native executable into an installation layout; test sibling discovery and selected paths. |
| Core executable inspection; ctl, ctmux, and task lifecycle; credential preflight; app-bundle Tailscale discovery | Test canonical paths, executable hashes, selected-file replacement, or bundle layout. |
| CLI remote repair/VPN, daemon lifecycle, local component import | Exercise real installed archive contents or executable selection. |
| Docker forced command | Change executable permissions to verify rejection. |

Ordinary SSH/SCP, remote VPN, agent restart, container-engine, and Tailscale
watchdog fake commands use immutable launchers. Data-only copies (registry JSON,
Keychain revision records, archive validation bytes) are not executable fixtures.
The bundle-download fake `gh` already uses an immutable `/bin/sh` launcher.

Core regressions force an open writable descriptor: the fake-command launcher
must still execute; a native launch must remain pending until the writer and
guard are released. On Linux, bypassing coordination must produce `ETXTBSY`.
These tests use an explicitly polled pending acquisition rather than sleeps to
establish ordering; a timeout only detects a launch that never resumes.

## Ctream drivers

Run the shared driver API, official driver, client, and agent tests with:

```sh
cargo test --locked -p ctream-driver -p ctream-driver-capture \
  -p ctream-driver-input -p ctream-client -p ctream-agent
```

`ctream/driver/tests/user_driver.rs` exercises the public API from a separate
integration-test crate. Its synthetic drivers cover a capture-only session,
custom and borrowed target types, an independent input capability, and capture
cancellation. They do not implement a real device backend. Official driver
process tests cover helper ownership and cleanup; platform-specific tests need
the matching operating system.

## Ctream Linux sessions

Run `bash docker/ctream/run.sh` from the repository checkout to test the
current client, official drivers, and agent against disposable Xvfb and headless
Sway sessions. The script uses a local Docker-compatible engine, builds the
Linux binaries, and runs the driver, platform, client, and agent tests. It then
runs the opt-in `ctream-agent` integration test through local stdio IPC,
including accessible controls in a GTK fixture.

The fixture lives in `ctream/agent/tests/linux_session.rs`, so Cargo supplies
the current agent executable directly to the test. It uses `ctream-client`
as a development dependency and builds the CLI and Wayland helpers for the
session setup.

The image supplies a compositor with the foreign-toplevel capture protocols
used by the Wayland helper. The test covers display discovery, display and
window frames, input initialization, and capture cancellation. Geometry unit
tests cover the common overlay and pointer transform when the preview resizes.

## Ctream Windows capture experiment

Build the diagnostic on Windows with
`cargo build --locked -p ctream-driver-capture --example windows-probe --release`.
It reports the process and console sessions, window station, desktop, DXGI
adapters, and attached outputs. For each attached output it attempts Desktop
Duplication with a 1.5-second frame wait, reads the frame back to CPU memory,
and reports dimensions, a nonblack-pixel count, and a checksum. It saves no
captured pixels and sends no input.

The probe requests per-monitor v2 DPI awareness before enumerating outputs and
reports whether it took effect. Only a verified `physical_pixels` coordinate
space should be compared with capture dimensions, accounting for output rotation;
an `unverified` result can contain DPI-virtualized desktop rectangles. See
[DXGI output geometry](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgioutput-getdesc).

Run it directly in the context being investigated:

```powershell
./target/release/examples/windows-probe.exe
```

For an explicit comparison with the same account's existing interactive login,
use the runner:

```powershell
powershell.exe -NoProfile -File ctream/driver-capture/examples/windows_probe/run.ps1 `
  -ProbePath target/release/examples/windows-probe.exe -Interactive
```

When all scopes in `Get-ExecutionPolicy -List` are `Undefined`, Windows clients
default to blocking script files. For this reviewed script, add
`-ExecutionPolicy RemoteSigned` before `-File` to set the policy for that process
only. This does not persist a policy change or override Group Policy. See
[PowerShell execution policies](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_execution_policies?view=powershell-5.1).

The runner creates a temporary task with an interactive,
limited principal, waits up to 20 seconds for the report, and removes its task
and temporary result files. The task also has a 30-second execution limit.
This requires Task Scheduler permission and an interactive login for the
current account. It does not elevate the process or create a login session.

Compare `context.process_session_id`, `same_session_as_console`, and the
window-station visibility with each output's capture result. A usable frame
has `capture.status = "frame_acquired"` and `readback.status = "read"`.
HRESULTs identify failed enumeration, device creation, duplication, or readback.
A frame timeout means no new frame arrived within the wait; it is distinct
from an access error.

Windows services and interactive desktops occupy separate sessions. A capture
helper in the user's interactive session can communicate with an SSH-facing
agent through IPC. See Microsoft's [interactive services guidance](https://learn.microsoft.com/en-us/windows/win32/services/interactive-services)
and [Desktop Duplication API](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/desktop-dup-api).
This example evaluates that boundary; it does not yet add a Windows wire
target, viewer connection, or production capture driver.

The live experiment confirmed this distinction: the SSH process could create
D3D11 devices but DXGI output enumeration returned `0x887A0022`
(`DXGI_ERROR_NOT_CURRENTLY_AVAILABLE`). The same executable, launched by the
limited interactive task, acquired and read nonblack frames from the attached
outputs. The runner removed its task and result files after completion.
Enabling per-monitor v2 awareness also made the scaled output's desktop
rectangle agree with its capture dimensions. This must be preserved when
introducing input coordinates into a Windows driver.

### Launching through WSL

A Windows executable launched through WSL interop can reach a different Windows
session from one launched by Windows OpenSSH. The live WSL experiment reached
the interactive window station directly and captured the attached outputs with
physical-pixel geometry. No scheduled task was needed. Treat the reported
session and window station as evidence for that launch, rather than inferring
desktop access from the SSH endpoint's operating system.

Use `windows-probe --echo-stdio` to check the same launch path without capturing
the desktop. This portable diagnostic echoes raw bytes immediately, reports
readiness and EOF byte counts as JSON on stderr, and exits after at most 30
seconds. The timeout is a guard against orphaned test processes, not evidence
that a transport handles cancellation.

The Bun checker accepts the complete command that launches this mode. For
example, replacing the host and executable path with the test environment:

```sh
bun ctream/driver-capture/examples/windows_probe/check_stdio.ts \
  ssh -T wsl-host '/mnt/c/ctream-test/windows-probe.exe --echo-stdio'
```

It checks an 8 MiB deterministic binary payload in differently sized chunks,
requires replies before closing stdin, verifies clean EOF and the remote byte
count, then tests disconnecting a second command. Check the reported remote
PIDs independently immediately afterward; successful local command termination
does not prove remote process cleanup.

The live WSL checks passed byte-for-byte and both Windows processes were absent
before the watchdog deadline. Capture also succeeded in a separate SSH command.
WSL restart, Windows sign-out, and session switching remain untested. A future
production launcher needs explicit executable selection, binary stdio and
cancellation handling; this experiment adds none of those to the agent protocol.
