# Windows CI exploration

Status: Windows background tasks and local ConPTY sessions implemented,
2026-09-04. Full workspace Windows tests remain deferred. The findings below distinguish source compatibility from runtime
support.

## Initial exploration evidence

The following cross-check was run on macOS with Rust 1.97.0 and the installed
`x86_64-pc-windows-msvc` target:

```sh
cargo check --locked --target x86_64-pc-windows-msvc --all-targets \
  -p rmux-proto -p rmux-core -p rmux-client -p task-proto \
  -p ctl-core -p rmux-ipc -p process-info
```

It passed. `--all-targets` typechecks test code; it does not link or execute
Windows test binaries. This is evidence of source compatibility, not proof of
Windows runtime behavior.

| Component | Finding | Implication for Windows CI |
| --- | --- | --- |
| `rmux-proto`, `task-proto` | Windows cross-check passed, including test code | Initial compile-only candidates |
| `rmux-core`, `rmux-client` | Windows cross-check passed, including test code | Initial compile-only candidates |
| `ctl-core`, `rmux-ipc` | Windows cross-check passed; local transport remains unsupported | Check portability without claiming local session support |
| `process-info` | Cross-check passed with dead-code warnings for `valid_pid` and `process_name`; native inspection returns unsupported | Keep separate from the initial warning-clean candidates |
| `rmux` CLI | Separate executable check passed with dead-code warnings; non-Unix main exits with an unsupported-platform message | Compilation does not make the CLI usable on Windows |
| `ctl` / `task-cli` | Check failed: unconditional `UnixStream` and `rustix::process` usage in task-cli | Task CLI integration currently prevents even ctl's non-Unix fallback from compiling |
| `taskd` | Check failed: the library is Unix-only, while main imports its exports unconditionally | Missing Windows execution backend and binary platform boundary |
| `ctld` | Check failed: explicit non-Unix compile error and Unix socket import | Gateway requires a Windows local IPC implementation |
| `rmuxd` | Source explicitly rejects non-Unix builds | Requires Windows daemon plumbing, not just a CI matrix entry |
| `rmux-app` | Source has unsupported local-transport stubs and a batch-SSH path; Windows build was not checked | Separate native build investigation required |

The failing executable checks were exploratory and expected to expose gaps.
They do not change the passing Linux/macOS CI gates.

## Initial compile-only job

CI checks native Windows compilation of the six portable crates. Keep the
package list explicit so adding a Unix-only workspace member cannot silently
expand this gate. The job in [the CI workflow](../.github/workflows/ci.yml) uses
the following configuration:

```yaml
windows-check:
  name: Windows portable compilation
  runs-on: windows-2025
  timeout-minutes: 20
  defaults:
    run:
      shell: pwsh
  steps:
    - uses: actions/checkout@v6
    - uses: dtolnay/rust-toolchain@stable
      with:
        targets: x86_64-pc-windows-msvc
    - uses: Swatinem/rust-cache@v2
    - name: Check portable crates and test sources
      run: >-
        cargo check --locked --target x86_64-pc-windows-msvc --all-targets
        -p rmux-proto -p rmux-core -p rmux-client -p task-proto
        -p ctl-core -p rmux-ipc
```

An explicit Windows image makes runner changes deliberate. The official
[Windows Server 2025 image inventory](https://github.com/actions/runner-images/blob/main/images/windows/Windows2025-Readme.md)
documents the installed development tools. Use the MSVC target and native
PowerShell rather than WSL: Linux builds under WSL would not exercise Windows
conditional compilation or native behavior.

The portable check does not need Tauri, a display, SSH credentials, or a local
daemon. A later native desktop build needs the C++ build tools and WebView2
described in the [Tauri Windows prerequisites](https://v2.tauri.app/start/prerequisites/#windows),
plus Node 24, pnpm 10, and the frontend assets. Verify those separately rather
than treating a successful portable-crate check as a desktop build result.

## Windows background tasks

`task-ipc`, `task-cli`, `taskd`, and `ctl` now cross-compile for Windows. The
native Windows job also builds both executables, runs task integration tests,
and exercises `ctl task` auto-start, create, logs, restart, stop, remove, and list.
The original six portable crates retain their explicit compile-only check.

Taskd uses local named pipes via `interprocess`. The default name is a stable
UUID derived from the user's local data directory, or `TASKD_RUNTIME_DIR` when
set. On Windows that override is a namespace seed, not a socket directory.
`taskd --socket` accepts an explicit `\\.\pipe\...` name. First-instance
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

State defaults to `%LOCALAPPDATA%\ctl\taskd` and inherits directory ACLs.
`TASKD_DATA_DIR` and `--data-directory` overrides must point to a private user
directory. An exclusive lifetime file lock prevents concurrent state writers.
The temporary state file is replaced with `std::fs::rename`, which supports
replacing an existing file on Windows. See [Rust's rename documentation](https://doc.rust-lang.org/std/fs/fn.rename.html).
State is not fsynced; sudden power-loss durability remains outside this slice.

On daemon restart, previously active runs are marked failed/stopped; there is
no automatic restart or adoption. Windows closes job handles on daemon crash,
terminating descendants. Native tests verify this by holding a file exclusively
in a descendant and checking that stop/crash releases it. Other tests verify
stdout/stderr tail output, completion, and metadata recovery after restart.

`ctl` locates sibling `taskd.exe`, starts it detached, and records the caller's
working directory when creating a definition. Background console programs use
`CREATE_NO_WINDOW`. Commands are passed as a program plus arguments; taskd does
not insert a shell. Invoke `cmd.exe` or PowerShell explicitly when needed.

## Windows local terminal sessions

`rmuxd` now uses `portable-pty`'s ConPTY backend, with the same journal,
checkpoints, leases, flow control, and reconnect protocol as Unix. The local
`rmux` CLI and `ctl rmux` can create, list, attach to, inspect, and terminate
sessions. The daemon auto-start path locates `rmuxd.exe` and detaches it from
the invoking console.

The data and owner-only maintenance endpoints are separate local named pipes,
both restricted by an owner-only DACL and rejecting remote clients. The
maintenance name adds `.control` to the data name. Its exclusive first instance
arbitrates concurrent startup; the data endpoint is published last, so a data
connection also implies maintenance support is available. `RMUX_RUNTIME_DIR`
is a namespace seed on Windows, not a filesystem socket directory.

The daemon answers ConPTY's initial cursor-position query at the canonical
origin before publishing a new session. The bounded startup reader handles
fragmented queries and removes that transport handshake from replayed output;
a detached session does not need a renderer to finish starting.

A child waiter closes the pseudoconsole before joining the independent output
reader. Session kill also closes ConPTY on a separate thread. This keeps output
draining while `ClosePseudoConsole` runs, avoiding the shutdown deadlock on older
Windows versions. See [Microsoft's ClosePseudoConsole documentation](https://learn.microsoft.com/en-us/windows/console/closepseudoconsole).
PTY handles remain owned by rmuxd throughout. A taskd interactive-task adapter
is still separate work.

Native CI exercises real ConPTY input/output, attachment resumption after a
connection loss, resize, final output and exit status, and owner-only daemon
restart. A separate smoke test checks `rmuxd.exe` discovery, auto-start, and
local `ctl rmux` routing. The existing Unix integration suite remains enabled
on Linux/macOS; its shell/FIFO-specific fixtures are not passed off as Windows
coverage. Manual Windows Terminal keyboard/rendering checks and cross-account
ACL tests remain useful follow-up validation.

Windows has no native process inspection or Bash/Zsh FIFO reporter in this
slice, so unavailable shell metadata stays unknown. Terminal output and
terminal-derived state continue to work.

## Remaining Windows work

- Interactive-task integration between taskd and rmuxd. PTYs remain owned
  exclusively by rmuxd.
- ctld's fixed local gateway and remote task routing.
- Native process inspection and the desktop app's local transport/build.
- Broader Windows tests for portable crates, remote clients, and terminals.
- Cross-account ACL tests and filesystem ACL hardening for custom data paths.
- Persistent logs, restart policies, workspace task integration, and other
  unfinished task features shared with Unix.

The Unix whole-workspace gates remain unchanged. No unsupported daemon is
included merely to make the Windows job look comprehensive.
