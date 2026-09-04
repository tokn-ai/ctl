# App-owned workspace

The app's workspace answers “which sessions do I want to keep here?” It is
separate from `rmuxd`'s live inventory and from SSH authorization. A session
can be remembered by several clients. `ctl-agent` remains a stateless SSH bridge;
`rmuxd` continues to own all shells, PTYs, history, checkpoints, and leases.

## Disk format and ownership

The native backend reads and writes `workspace.json` in Tauri's app-data
directory (`~/Library/Application Support/io.rmux.desktop` on macOS). The
versioned document contains:

- `workspace_id`, `schema_version`;
- `hosts`: stable `host_id` plus a local or structured SSH target;
- `sessions`: `(host_id, session_id)`, name, and last-known cwd/display cwd;
- ordered `tabs` and optional `active_tab`, referencing known sessions.

An outer opaque `revision` fences stale writers. Runtime status, process names,
command lines, terminal output, output sequences, passwords, and attachment
tokens are excluded. Cached cwd is presentation only: it is not treated as live
shell awareness or used to create a shell automatically.

Native commands `load_workspace` and `update_workspace` run filesystem I/O off
the UI thread. Writes use an interprocess lock, revision comparison, a private
same-directory temporary file, file sync, atomic replacement, and directory
sync on Unix. Workspace files are owner-only on Unix. Invalid references,
corrupt/future schemas, symlinks, and oversized files are rejected without
overwriting the existing document. The UI serializes its writes and offers
retry for I/O failures; a revision conflict requires reloading the app.

The first launch migrates the old `rmux.remote_hosts` WebView value only if no
native workspace exists. It saves hosts without contacting them and removes the
legacy value only after saving successfully. No earlier session membership was
stored, so users must explicitly import their previous sessions. Migration
never assumes every session on a remembered host belongs in this workspace.

## Lifecycle

| Action | Workspace effect | Remote effect |
| --- | --- | --- |
| Launch/restart app | Restore entries, tabs, selection as unverified | Attach the selected tab automatically if local; no SSH connections |
| Connect host | Inspect its known entries; resume the selected tab on that host, otherwise its first open tab | Authenticate through SSH, inspect known IDs, then attach |
| Open session | Select/open its tab | Connect to its host and attach |
| Create shell | Persist new membership before attaching | Create one session |
| Add existing session | Remember selected entries | Enumerate only the selected host; no attachment |
| Refresh known sessions | Update observations; retain missing/unreachable entries | Inspect known IDs only, not full inventory |
| Close/detach tab | Remove tab, retain membership | Detach its view; shell continues |
| Remove from workspace | Remove membership and its tab | No kill |
| Close session | Remove membership after accepted kill or not-found | Explicitly terminate the session |
| Restart local rmuxd | Mark local entries missing | Terminate all local sessions, including other apps' sessions |

An unreachable host is not evidence of a missing session. Successful inspection
or attachment is authoritative for live state; `session_not_found` marks an
entry missing. A session that exits while attached remains remembered as exited.
On the next app restart, saved entries are again unverified until contacted.

Only one terminal tab is attached per window. Local startup attachment is a
one-shot intent and uses the normal connection/error handling once the renderer
is ready. Background local tabs do not replace a selected remote tab. Remote
tabs remain disconnected until opened explicitly or their host is connected.
Connecting a host with no open tabs only refreshes its known entries; it does
not reopen detached tabs or import sessions. A late host inspection cannot
override a newer tab selection, reopen a closed tab, or attach after window close.

If remote creation succeeds but local persistence fails, the shell is not
killed or created again automatically. Its entry stays in memory, attachment
is deferred, and the app asks the user to retry saving. Normal window close
waits for queued writes and pauses on failure or an ongoing shell creation.
Force-quitting during an unfinished save can lose that latest change, but
atomic replacement preserves the previously saved workspace.

Saved membership does not make terminal processes durable across a remote
`rmuxd` or machine restart. After app restart, attachment is fresh and uses a
checkpoint; no old attachment token or keyboard input is replayed.

## Verification

Run the frontend suite and native persistence/transport tests:

```sh
pnpm --dir apps/rmux test
cargo test -p rmux-app -p ctl-agent
```

The opt-in live test targets the repository's Docker/Podman fixture at
`rmux@127.0.0.1:2222`. It starts two independent native client processes. The
first creates two temporary sessions and persists only one; the second loads
that workspace, inspects only its known ID, and attaches from a fresh
checkpoint. The parent cleans up only its two recorded test sessions, even
when a child fails. Configure and verify SSH host trust first (an isolated
known-hosts file can be supplied through a test-only `ssh` launcher).

```sh
RMUX_WORKSPACE_TEST_IDENTITY=/absolute/path/to/private-key \
  cargo test -p rmux-app \
  workspace::remote_test::docker_workspace_survives_client_restart \
  -- --ignored --exact --nocapture
```
