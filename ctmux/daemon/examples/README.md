# Isolated PTY executable-replacement proof

These examples exercise the Unix exec/PTY boundary described in
[Proposal 0018](../../../docs/proposals/0018-daemon-hotreload.md). They do not
start, restart, connect to, or hotreload a running ctmuxd or ctld.

Build and run the proof tests on macOS or Linux:

```sh
cargo build --locked -p ctmuxd --examples
cargo test --locked -p ctmuxd --test exec_hotreload
```

Ordinary package/workspace test selection builds the example executables
automatically. A narrowly selected `--test exec_hotreload` requires the explicit
example build above.

To inspect the JSON reports directly:

```sh
target/debug/examples/hotreload_a target/debug/examples/hotreload_b 2
target/debug/examples/hotreload_a target/debug/examples/hotreload_b 2 --fail-exec
```

The count specifies one to six actual replacements: two produces A → B → A. Reports go
to stderr because stdin/stdout are intentionally used as inherited PTY master
handles. Each executable has a distinct compiled image marker and fingerprint.

The shell owns its own counter and reports its PID, parent PID and TTY after
each replacement. Before exec, the old image consumes a partial record prefix;
the new image imports those bytes and reads the remaining tail from the original
PTY. Odd replacements split a color escape sequence; even replacements split
the UTF-8 encoding of `日` after its first byte. The actor also serializes and
restores the live AVT emulator, input-mode observer, alternate-screen observer,
and incomplete UTF-8 decoder prefix. Tests compare these states with an
uninterrupted replay of every consumed raw byte, including both screen buffers,
scrollback, saved contexts, and mouse/paste/keyboard modes.

Tests require exact chronological history with no loss or duplication,
stable logical IDs, unchanged shell birth identity, working input, no growth
in inherited descriptors, and successful reaping of the original child with
exit code 37. The failed-exec case verifies a live-shell input/output canary
before subsequently replacing the executable successfully.

The test driver bounds execution and cleans up only its owned actor and verified
shell process group on failure. The examples use safe Rust/nix APIs and retain
the workspace's unsafe-code prohibition.

This is a single-threaded experiment with synthetic records and a private
version-2 checkpoint passed as an argument, bounded to 120 KiB before exec and
before decoding. AVT and observer snapshots have their own version-1 formats;
none change the named ctmux client protocols. Production requires a private state
transport, typed ownership of arbitrary inherited descriptors, complete daemon
registry/history/lease/reporter and relay state export, safe worker quiescence,
signal restoration, and verified activation.
The proof does not establish ctmuxd or ctld hotreload support, Windows support,
or arbitrary-crash recovery.
