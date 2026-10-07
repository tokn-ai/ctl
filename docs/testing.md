# Tests and CI

Use Node 24, pnpm 10, and the Rust toolchain pinned in `rust-toolchain.toml`.
On Linux, install the Tauri system dependencies listed in `.github/workflows/ci.yml`.

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
`ctl-core`'s `test-fixtures` feature in **dev-dependencies**. The command is a
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
