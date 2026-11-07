# Build a local Linux bundle

Run `pnpm bundles:build` from the checkout to build `ctl-agent`, `ctmuxd`,
`ctl-taskd`, `ctld`, `ctl`, and `ctmux` for `x86_64-unknown-linux-musl`. The command uses the
current working tree, including uncommitted changes and commits that have not
been pushed. It also builds the separate `ctlup` manager for that target.
It does not publish anything or activate a local or remote installation.

Start Docker or Podman with Linux containers first. Set `CTL_CONTAINER_ENGINE`
to an executable path to choose a particular engine. When Zig, `cargo-zigbuild`,
Make, and Perl are installed, the command cross-compiles with the host's Rust
toolchain, installing the Linux musl Rust target on first use. It then runs the
resulting binaries in a small Linux container to package their metadata. This
keeps compilation native on ARM Macs and other development hosts.

The musl build compiles OpenSSL from source and links it statically into `ctl`
and `ctlup`.
At runtime, HTTPS uses the remote system's installed CA certificates.

When the native cross-compilation tools are unavailable, it builds and packages inside a Linux amd64
container with Rust 1.99, Node 24, and musl. ARM hosts need their engine's amd64
execution support for either path; emulating the complete compiler is slower.
The first build downloads the necessary images and dependencies. Native cross
builds reuse `target/development-bundle-build`; container builds reuse a separate
compiled-artifact volume for this checkout and shared Cargo download volumes.

The output is a complete local bundle at:

```text
target/development-bundles/x86_64-unknown-linux-musl/
  bundle.json
  ctl-agent
  ctmuxd
  ctl-taskd
  ctld
  ctl
  ctmux
target/development-bundles/updaters/x86_64-unknown-linux-musl/
  ctlup-<bundle-id>-x86_64-unknown-linux-musl
  ctlup-<bundle-id>-x86_64-unknown-linux-musl.sha256
  ctlup-x86_64-unknown-linux-musl.json
```

Use that directory as **Provided build files** in the component update dialog,
or pass it to `ctlup update --host NAME --from DIRECTORY`. Do not
enable **Native local build**: the directory is already packaged for Linux.
Updates preserve running sessions; restarting an existing daemon remains a
separate action.

Debug checkout builds discover the adjacent target updater automatically. When
using these files with a standalone `ctlup` or an offline desktop build, retain
the updater sidecars and set `CTLUP_BUNDLES_DIR` to their directory. The updater
metadata and bytes are verified before SSH bootstrap; no remote download is
required. Copying only the six runtime executables is insufficient to bootstrap
an updater for a foreign target offline.

Failed or cancelled builds do not replace the previous output. Consumers must
check the command's exit status rather than reuse that output after a failure.
Normal cancellation removes the owned build container immediately. If the
launcher is killed abruptly, the container stops after its build lease expires
(normally within 20 seconds, plus process termination grace).
