# Windows CI exploration

Status: compile-only CI enabled, 2026-09-04. Windows test execution remains
deferred. The findings below distinguish source compatibility from runtime
support.

## Evidence

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

## Runtime work before full workspace coverage

- Define a Windows per-user IPC endpoint and access-control model for rmuxd
  and taskd, including concurrent startup and stale-endpoint handling.
- Integrate the Windows terminal backend into rmuxd while preserving its PTY,
  process, journal, and lease ownership. Taskd must still never own a PTY.
- Implement non-PTY task process-tree ownership and termination on Windows;
  Windows Job Objects are a candidate to investigate in place of Unix process
  groups. Specify graceful shutdown and daemon-crash behavior explicitly.
- Replace Unix permission and signal assumptions with Windows equivalents.
  Audit atomic state replacement and concurrent writer behavior on Windows.
- Repair the task CLI and daemon platform boundaries before expecting `ctl`
  to compile, even when only its remote or unsupported-platform path is used.
- Audit fixtures that invoke `/bin/sh`, assume `/tmp`, create Unix sockets,
  inspect POSIX permissions, or depend on Unix shell startup behavior.

## Test enablement later

After native compilation is verified, enable Windows test execution for the
explicit portable package set first. Review which tests are compiled out by
platform guards so a green job is not mistaken for daemon coverage.

Then validate the desktop frontend and native remote-client path separately.
Full workspace and process/terminal integration tests follow Windows runtime
implementation. Do not add `continue-on-error` or empty platform stubs merely
to make an unsupported feature appear tested.

## Next step

Use the native compile-only result to assess test readiness. Windows runtime
implementation and test execution remain separate follow-up work; a passing
compile-only job does not establish either.
