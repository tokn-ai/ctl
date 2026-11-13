# ctl-files

Shared read-only filesystem and Git operations for local clients and the fixed
`ctl-agent files` service. The wire contract lives in `ctl-proto::files`.

`execute` accepts one typed operation. Relative paths require the originating
pane's absolute working directory; absolute paths and account-home paths are
resolved on the machine executing the operation. Existing symlinks resolve to
their targets for reads. Only regular files can be read; directory entries
retain their symlink flag. Paths must be bounded valid UTF-8, with unsupported
names reported explicitly rather than silently changed.

Reads are ranged and capped at 4 MiB. Directory pages contain at most 1000
entries, use filesystem enumeration order, and are best effort while a directory
changes. At most 100,000 entries may be skipped to reach a page. Git status uses
NUL-delimited porcelain records and returns index/worktree status plus rename
origins. Git stdout is capped at 8 MiB, stderr at 64 KiB, and catalogs at 100,000
entries. Operations have a 30-second deadline and eight-operation admission
limits. Git child processes retain their own eight-process admission until they
have exited and been reaped, including after cancellation. Each remote response
is bounded by a 16 MiB frame and retains service
admission until its consumer finishes reading or its write deadline expires.

`serve` handles one negotiated request per stream and concurrently watches peer
closure. Cancelling a request drops its work and signals a subprocess supervisor
which kills and reaps its Git child. It leaves sibling service streams intact.
Filesystem cancellation stops further work; an OS filesystem call already
issued through Tokio may finish on its worker pool.

Run `cargo test -p ctl-files` to check ranged reads, directory pages, host-local
path resolution, symlinks, subprocess cancellation, Git path parsing, and
independent request cancellation.
