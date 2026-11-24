# Tests and CI

Use Node 24 LTS (24.15 or later) or Node 26 or newer. Use pnpm 10 and the
Rust toolchain pinned in `rust-toolchain.toml`.
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

## Ctream welcome and driver selection

Run the protocol, client, agent, and viewer checks for backend selection and
welcome-window changes:

```sh
cargo test --locked -p ctream-proto -p ctream-client -p ctream-agent -p ctream-preview
```

The welcome window uses the local agent when launched without arguments.
Existing endpoint arguments configure a remote agent; a display, `--windows`,
or `--window ID` preselects a source, and `--capture-driver ID` preselects its
capture provider. All launches first show a view-only preview. **Start** requires
a live frame, keeps that capture, hides setup, and opens supported input in a
separate native viewer window. Setup stays on the active source while the
viewer is open. The viewer's native close control releases input, restores and
activates setup, and retains the stream as a view-only preview. There is no
Back button. Closing setup or quitting the application cancels all work.

Protocol changes: none. The window lifecycle keeps `ctream_agent` at 1.0.8
(build 8), including its existing capture/input operations and handshakes.

Check these boundaries with a recording transport and deliberately delayed
responses before running a live desktop test:

| Case | Required observation |
| --- | --- |
| Welcome discovery and live preview | Queries and capture may open; neither `input` nor `input-window` opens before Start. Preview pointer, button, wheel, and keyboard events send no remote input. |
| Start and native viewer close | Start keeps the selected capture and opens input once in a separate native viewport. Native viewer close releases held keys/buttons and restores setup while capture continues. Starting again opens a new input channel for the same selection. |
| Window ownership and source lock | Setup and viewer have different native window identities. Start hides setup and preserves its dimensions. Source/driver refresh and selection cannot change the active source while viewing. Viewer close restores and activates setup without opening another capture. |
| Independent viewer redraw | With setup hidden, successive capture frames wake and update the viewer. A capture ending while the viewer is minimized still closes a finished window session and restores setup. |
| Driver, display, or window change | After returning to setup, changing a source cancels the previous capture, clears its image, and waits for a frame from the new choice before enabling Start. |
| Late asynchronous completion | Complete an old discovery, catalog, window query, or capture connection after a newer selection. Its data, frames, and errors must not replace the new selection; a late capture must be dropped and cancelled. |
| Failure or application close | Closing setup or quitting cancels capture, input, and pending requests. Capture EOF in the viewer closes input, including when a key is held. A connection failure cannot enable Start using a stale frame. |
| Close during input initialization | Close the native viewer while its input handshake is pending, then complete that handshake late. Input must be cancelled without interrupting the retained setup capture or reopening the viewer. |
| Viewport input isolation | Setup events never send remote input or reset viewer-held input. Only the focused viewer forwards events; focus loss releases held state. Its own dimensions govern pointer mapping after resize. |
| Driver-first choice | All advertised providers appear before a source is chosen, with system and capability tags. Choosing a single-scope driver sets its scope; incompatible sources are excluded. Drivers without accessible matching sessions remain visible with a No sources tag. |
| Explicit driver choice | Unknown IDs and unsupported sources fail without opening a different provider. The actual-driver label follows the successful handshake. |
| Independent catalog lifetime | Changing a driver or source preserves the full catalog. Refresh cancels the old catalog; a late result cannot replace the current list. Custom driver IDs use system/scope metadata rather than naming conventions. |
| Legacy agent | Auto uses retained capture operations and their original frame format. No explicit driver is reported as honored when the catalog or new handshake is unavailable. |

The `1.0.7` protocol tests should verify catalog fields and scope filtering,
bounded and malformed handshakes, reported driver/format validation, and
rejection of incompatible or unknown IDs. Verify that a source which fails
during lazy initialization never reports a successful handshake. Existing
`1.0.1`–`1.0.6` requests must retain their response shapes and frame prefixes.
Test with slow or failed operations as well as immediate responses: successful
frame rendering alone does not verify cancellation or stale-result handling.

For a live check, use a disposable Linux session or the owned Windows fixture
below. Select a driver and then its source, observe the welcome preview, switch
sources while capture is opening, and resize the welcome window. Verify that
clicking or typing in that preview does not reach the fixture. Then choose
Start and verify a distinct native viewer appears while setup hides. Resize
the viewer through wide and tall aspect ratios and verify fixture clicks.
Close it with the native window control and verify setup returns with its
original dimensions, input stops, and frames continue on the same capture.
Reopen the viewer, close it during input initialization, and verify the late
handshake cannot restore control. Finally close setup or quit the application.
Choose explicit valid and incompatible providers to check the reported backend
and failure path. Close during connection and during streaming, then verify
that temporary capture/input processes are gone.

For a local native Wayland viewer, winit 0.30.13's visibility and focus methods
are no-ops, so record automatic hide/activation as unsupported rather than a
successful window-management check. A remote Wayland capture target is a
separate concern and does not impose that limitation on a viewer running on
another platform.

The separate-window step passed all 24 preview tests and all-target preview
Clippy. Headless viewport tests cover distinct viewport events, root visibility
commands, held-key release, focus loss, closing during input initialization,
source locking, and pointer mapping after resize.

A native macOS wgpu harness also verified distinct native window identities:
setup retained its 860×620 size and hid while the viewer was visible and focused.
Closing the viewer through its native window control restored and activated
the same setup window, closed input, and retained capture. Reopening reused
that capture and opened fresh input. Closing the application with the viewer
open cleaned both channels. A held key survived hidden-root repaints, and
clicks in wide and tall viewer layouts mapped to the expected source pixel.
Owned-window screenshots confirmed fitted frames, restored setup, and no Back
button. The harness used a local synthetic source and exited cleanly; this step
did not repeat remote capture or input tests.

### Historical single-window validation

For the capture-driver and welcome-window step, 73 host tests passed, including
13 preview tests and six welcome lifecycle tests. The welcome tests cover
view-only interactions, retaining capture across Start/Back, continued frames
after Back, cancellation of pending input, stale source requests, explicit
driver failures, and legacy Auto behavior. The disposable Linux Docker suite
also passed its new and retained capture/input contracts.

The native Windows fixture on a secondary display passed Win32 discovery,
automatic and explicit WGC capture, growth, shrinkage, and full occlusion by
its owned cover window. The input portion of this rerun did not complete:
`SetCursorPos` returned access denied during fixture activation, before the
agent input channel opened.

A local native wgpu lifecycle harness observed zero input channels during
setup, one input channel after Start with the same capture, input closure
with capture continuing after Back, and capture EOF after app close. Native
Metal screenshots verified setup at 860×620 and 600×460 and the viewer at
1100×720: source controls and Start remain visible, and the preview retains
its aspect ratio inside its pane. The real welcome window also received live
WGC frames through the WSL endpoint; its owned remote processes showed one
capture channel and zero input channels during setup. Temporary preview and
fixture processes were closed after the check.

The driver-first catalog tests additionally cover the `1.0.8` agent-wide
catalog, retention of the `1.0.7` targeted response shape, custom identifiers,
catalog refresh races, metadata-driven scope switching, and tagged cards
before source selection. Conflicting window/driver preselection never widens
to display capture without an explicit card click, and pending display
discovery cannot start a stale source query. A `1.0.7` fallback combines the catalogs of discovered
targets without claiming to discover providers for absent systems. The Linux
Docker check verifies all four compiled providers before session discovery.
These tests and the Windows/Linux cross-target Clippy checks passed. Native
wgpu checks also verified the Windows and Linux driver lists at 860×620 and
600×460, including No sources tags, scrollable source controls, and unchanged
view-only setup and Start/Back input lifecycles.

Use this command to open the native viewer controls over a synthetic frame.

```sh
cargo run --locked -p ctream-preview --example controls_fixture
```

Hover at the top edge, move the window across display edges, and resize it. The
checkerboard and its size label make any unexpected canvas shift visible. The
Debug button shows whether the pointer is over the in-view controls. Press
Escape to close the fixture.
Maximize the viewer and drag its title bar: it should return to its previous
size under the pointer and continue moving. A title-bar click should leave
it maximized.

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

The AV1 frame-rate regression requests 30 FPS and then 8 FPS through the normal
client for both X11 display and window capture. An animated fixture paints frame
IDs, and five-second samples report delivered frames and fresh marker changes
separately. Delivery must reach 27–33 FPS at 30 and 5–9 FPS at 8. At least 80% of
delivered frames must show a new marker in each mode. That allows for AV1
distortion of some markers and still detects an 8 FPS source repeating frames
to fill a 30 FPS stream.

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
This probe evaluates that boundary. Use the native agent check below to exercise
the Windows drivers and agent protocol.

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
WSL restart, Windows sign-out, and session switching remain untested. The native
client now uses explicit executable selection and the same binary stdio and
cancellation behavior; the diagnostic probe itself has no agent protocol impact.

## Ctream native Windows agent check

Build the Windows agent and opt-in integration example on Windows:

```powershell
cargo build --locked --release -p ctream-agent --bin ctream-agent --example windows-session-check
./target/release/examples/windows-session-check.exe ./target/release/ctream-agent.exe windows-2
```

Replace `windows-2` with an idle test display from discovery. An explicit Windows
display is required; the fixture never defaults to the primary display. Windows
shares one pointer and keyboard across monitors, so the short input check still
needs an interval when moving the pointer and focusing its test window is acceptable.
Run it in the interactive desktop session, directly or through WSL interop. The
agent argument must be a Windows path when the example itself runs on Windows;
WSL's `wslpath -w` can translate a mounted executable path before invoking it.
This test opens its own temporary Win32 window and sends input only after
verifying that its click points and foreground window belong to the fixture.
It verifies changing fixture colors in captured pixels, capture cancellation,
click locations through wide and tall preview geometries, key delivery, and
release of a held key when the input channel closes. It closes its window and
agent channels and restores the pointer when it remains at the test position.

An overall deadline cancels blocked channels; a final watchdog bounds the
example's lifetime. Check that the test processes are absent after a failure.
The example is not run by ordinary `cargo test`; its pure rotation, coordinate,
input-state, and protocol checks run without a Windows desktop. Use
`bash docker/ctream/run.sh` to verify the retained Linux session behavior.

In the earlier Windows display milestone, the local wgpu preview received
frames over the WSL SSH launcher. The isolated native fixture then passed on
a secondary display: discovery and capture agreed
on physical dimensions, capture colors and both preview aspect ratios matched,
and keyboard delivery and key release on EOF succeeded. Temporary processes and
test files were removed. Display discovery does not imply permission to control
elevated applications or a secure desktop.

For individual windows, build and run the separate bounded fixture:

```powershell
cargo build --locked --release -p ctream-agent --bin ctream-agent --example windows-window-check
./target/release/examples/windows-window-check.exe ./target/release/ctream-agent.exe windows-2
```

It requires an explicitly selected idle display and creates its own window and
opaque cover. The check compares captured pixels and physical DWM bounds before
and after growth, shrinkage, and covering; then checks pointer mapping through
wide and tall preview areas, keyboard delivery, capture/input EOF, and target
closure while a key is held. Input starts only at the explicit input stage and
is restricted to the owned windows. The same shared-cursor constraint applies.
The fixture has a deadline, cancels agent channels, closes its windows, and
conditionally restores the pointer.

Append `--serve 60` to leave the fixture visible for a bounded preview check
without automatic input. It prints the discovered window ID, which can be
passed to `ctream-preview --host SSH_HOST DISPLAY --window WINDOW_ID` with the
same agent/SSH launcher options. The maximum serve duration is 90 seconds.
Captured images stay in memory; the fixture reports metadata and assertions.

Before the capture-driver and welcome-window changes, the Windows window
fixture passed on a secondary display through WSL interop:
discovery, physical capture bounds, growth/shrinkage, live colors beneath the
opaque cover, wide/tall click mapping, keyboard delivery, EOF cleanup, and
release on target closure with the input channel still open. The local wgpu
viewer also rendered the selected window over SSH, and the CLI returned its ID
exactly once. Temporary windows, processes, and executable files were removed.
