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
