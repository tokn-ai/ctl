# process-info

Read-only, best-effort OS observations for a shell whose PID the caller owns.
Independent of rmux, Tauri, terminal rendering, and shell-integration scripts.

```rust,no_run
use process_info::Inspector;

# fn example(shell_pid: u32, foreground_pgid: Option<u32>) -> std::io::Result<()> {
let inspector = Inspector::new(shell_pid)?;
let observation = inspector.inspect(foreground_pgid)?;
// observation.cwd is the root shell's physical directory.
// observation.foreground distinguishes unknown, shell-owned, and a known job.
# Ok(())
# }
```

Capture the inspector while the spawned child's handle is still owned. Supply
the PTY's current foreground process group for each observation. Birth tokens
fence PID reuse before and after potentially slow observations. Foreground
candidates must belong to that group and have a verifiable ancestry back to
the original shell; traversal and fallback scans are bounded. Prefer the job's
group leader, then a deterministic surviving member if a pipeline leader exited.

- macOS: libproc's safe `pidinfo` API, native BSD/vnode-path structures, and
  process-group enumeration. No subprocesses or unsafe code in this crate.
- Linux: `/proc/PID/stat`, `/proc/PID/cwd`, and a bounded group-membership scan
  only when the foreground group leader could not be used.
- Other systems: explicit `Unsupported` errors.

Only process IDs, birth tokens, parent/group IDs, a bounded process name, and
cwd are read. Arguments, environments, command text, and dotfiles are not read.
Names containing control characters or invalid UTF-8 are omitted. Cwd remains
a `PathBuf`; consumers decide whether a non-UTF-8 path can be represented.

## Limits

These calls are synchronous: use a background worker, never a UI, runtime, or
PTY-ingestion thread. OS calls can stall on network filesystems. A missing cwd
does not suppress a known foreground job, and vice versa. Root disappearance
or PID reuse invalidates the whole sample. The process table is not atomic;
observations are advisory, not authorization or proof of continued liveness.

`cwd` intentionally describes the supplied root shell, not a child's `chdir` or
a nested shell's logical `$PWD`. A physical path may differ from a shell's
symlink-preserving spelling. `Foreground::Shell` only means the shell's process
group owns the terminal: builtins, disabled job control, background children
sharing that group, and line editing cannot be distinguished this way. No
prompt state, command completion, or exit status is inferred from it.

Shell hooks and automatic, dotfile-free integration are a separate later layer.
