# App-owned workspace

The app's workspace answers “which sessions do I want to keep here?” It is
separate from `rmuxd`'s live inventory and from SSH authorization. A session
can be remembered by several clients. `ctl-agent` stores only its environment ID;
`rmuxd` continues to own all shells, PTYs, history, checkpoints, and leases.

## Disk format and ownership

The native backend keeps two files under `~/.tokn/rmux` on every platform:

- `hosts.json` (schema 1) owns saved remote hosts and reusable gateways.
- `workspace.json` (schema 8) owns session membership and presentation state.

Their shared lock file and migration backups live alongside them. If `workspace.json`
is absent, rmux imports a valid workspace from the former Tauri app-data
directory (`~/Library/Application Support/io.rmux.desktop` on macOS). The original
file and backups remain available for recovery. An existing new-location file
always takes precedence.

The host catalog contains:

- `hosts`: stable `host_id`, display `name`, named `connection_methods`, and
  `preferred_method_id`. Each method has a stable `method_id`, name, and structured
  SSH `target`. Optional host-level `remote_info` contains the verified remote ID,
  agent version, and bundle metadata;
- reusable `ssh_gateways`, referenced in order by a method's `gateway_route`.

The workspace contains:

- `workspace_id`, `schema_version`;
- `sessions`: `(host_id, session_id)`, name, and last-known cwd/display cwd;
- ordered `tabs` and optional `active_tab`, referencing sessions or managed tasks;
- task references, sidebar selection, and task drafts with source scopes and
  original saved-definition revisions;
- loopback-only port-forward definitions and whether each should be restored;
- `host_identities`: observed remote identity pins only for hosts referenced by
  sessions, tasks, or forwards. These observations are not host definitions.

Schema 8 imports existing saved hosts and gateways into the catalog before
removing them from the workspace. Import is idempotent across a crash between
file commits. Schema 7 is preserved in `workspace-v7.backup.json`; earlier schemas
receive their corresponding backups. Existing conflicting catalog definitions
are preserved and block migration rather than being overwritten. Host IDs and
all session, tab, task, and forwarding references remain unchanged.

Schema 7 introduced machines with multiple connection methods. Earlier SSH
targets migrate to one `default` method named `SSH`, use the destination as the
initial host name, and move verified environment metadata to the host. Saved
hosts are never merged. The local host has ID `local`, name `Local`, and no SSH
methods or remote identity; it is synthesized rather than saved in the catalog.

Every saved remote host has at least one method and a preferred method that
exists on that host. Method IDs are unique within a host; destinations need not
be globally unique. Method targets exclude runtime host metadata and resolved
gateway copies. The app resolves gateway references and supplies the expected
remote identity when connecting.

Schema 6 introduced reusable SSH gateways. Schema 5 adds the central Ports
sidebar selection. It preserves the schema 4 port-forward definitions and
creates a recoverable v4 backup during migration.
The Ports sidebar groups saved forwards by host, including stopped forwards,
while runtime status remains owned by `ctld` and is never written to the
workspace. Schema 3 stores saved task definitions in separate shared project/global
catalogs. The workspace keeps `task_definition_scope` and references, while
definitions are loaded as view data and excluded from subsequent workspace
writes. Schema 2 definitions are imported into the global catalog with a
recoverable backup before migration commits. See
[shared task definitions](task-definitions.md) for paths and migration behavior.

An outer opaque `revision` fences stale writers. Runtime status, process names,
runtime command lines, terminal output, output sequences, passwords, and attachment
tokens are excluded. Cached cwd is presentation only: it is not treated as live
shell awareness or used to create a shell automatically.

Native commands `load_workspace`, `update_workspace`, `load_hosts`, and
`update_hosts` run filesystem I/O off the UI thread. Writes use an interprocess
lock, revision comparison, a private
same-directory temporary file, file sync, atomic replacement, and directory
sync on Unix. Both files are owner-only on Unix. Malformed or duplicate references,
corrupt/future schemas, symlinks, and oversized files are rejected without
overwriting the existing document. The UI serializes its writes and offers
retry for I/O failures; a revision conflict requires reloading the app.

The first launch migrates the old `rmux.remote_hosts` WebView value only if no
native workspace exists. It saves hosts into the catalog without contacting them
and removes the legacy value only after both stores save successfully. No earlier
session membership was
stored, so users must explicitly import their previous sessions. Migration
never assumes every session on a remembered host belongs in this workspace.

## Hosts, connection methods, and remote identity

A host represents a named machine, independently of the addresses and gateways
used to reach it. This version supports one remote account/ctl environment per
host. **Add host** asks for an SSH address or existing alias, a display name, and
authentication. It verifies the remote environment, then automatically saves the
named host with an initial `SSH` method in `hosts.json`. Display names may contain
spaces and do not rename SSH aliases. This flow does not write OpenSSH config.
Additional methods and gateway routes use the connection editor in **Host settings**.
A new direct method can explicitly export a managed OpenSSH entry with **Also
save to OpenSSH config**. Export is off by default and unavailable for existing
config aliases or gateway routes; the host catalog remains authoritative for
the saved method.

Concrete aliases in `~/.ssh/config` and its `Include` files appear as hosts in
memory. Each uses a deterministic `ssh-config:<encoded alias>` ID and delegates
connection settings to OpenSSH. Merely displaying or connecting to one never
persists its definition. Saving a customization promotes it to a saved host with
the same ID. A saved record with that ID overrides the projection. Otherwise,
a saved pure alias method suppresses a duplicate projection only when no
workspace sessions, tasks, or forwards refer to the projected ID. Addresses,
names, and matching remote IDs never cause automatic host merging.

When an alias or saved definition disappears, existing references remain as an
unavailable host. Restoring the definition restores access. Connections to an
unavailable alias are blocked before SSH can interpret it as a DNS hostname.
For a promoted alias, an unavailable original alias method does not disable
other inline methods on the same host. Runtime expected-identity observations
protect remembered sessions even if catalog metadata changes; autosaving those
observations does not overwrite the catalog identity.

**Host settings** manages the host name, method names, connection settings, and
preferred method. **Connect host** uses the preference; **Connect using** selects
a specific method without changing that preference. There is no automatic
fallback after a failed attempt. Editing settings or changing the preference
does not replace an existing terminal transport. Method verification may open
an SSH connection, but applying the method to remembered sessions requires an
explicit connection. The current selected route is runtime state; after app
restart the preferred method is used for subsequent connections.

**Connect host** discovers an account-owned UUID and the installed agent version
on the same SSH stream used to verify the rmux service. Bundled installations also
report the app version, bundle ID, Git revision, and target triple. The host heading's
tooltip shows the last discovered metadata.

All methods saved on a host must verify the same account-owned UUID. Adding a
method retains the existing methods, and verifying a matching UUID never merges
separate hosts. An explicit successful connection updates the selected route for
that host's sessions and tabs while retaining their IDs, ordering, selection,
cwd metadata, and terminal caches. Port forwards and task references continue
to belong to the same stable host ID.

Every subsequent desktop connection checks the expected UUID before sending service
commands. A different UUID at a saved address is rejected without rebinding the saved
sessions. Different SSH accounts normally have distinct ctl data directories and IDs.
An old workspace learns its ID on its next successful **Connect host**. A
different account/environment requires a separate host; matching addresses or
display names are not evidence of shared session ownership.

The identity is `~/.tokn/ctl/remote-id` under the remote user's home directory
on all platforms. Unix component bundles use `~/.tokn/ctl/versions` and the
`~/.tokn/ctl/current` symlink; upgrades leave the identity file in place.
Concurrent first connections publish one complete UUID file without
overwriting a competing creator; upgrades do not replace it. Corrupt identity files
fail discovery instead of silently generating a new identity. The ID identifies a
ctl environment, not hardware: copying its data directory copies its identity, so
independent cloned environments must receive separate IDs. Keep the identity file
when moving or restoring the same environment.

The opt-in `ctl-agent connect --identity` protocol emits `ctl-ssh-v2\n`, a big-endian
32-bit JSON byte length (at most 8192), then the identity JSON, before service bytes.
Normal CLI connections retain the `ctl-ssh-v1\n` protocol. Older agents require an
update for desktop identity discovery. SSH remains responsible for authentication
and host-key verification; the UUID is not an authorization credential. Before
starting the agent, the Unix wrapper emits `ctl-ssh-nf\n` when it is absent so
installation does not depend on the remote shell's diagnostic language.

## Lifecycle

| Action | Workspace effect | Remote effect |
| --- | --- | --- |
| Launch/restart app | Restore entries, tabs, selection as unverified; select each host's preferred method | Attach the selected tab automatically if local; remote terminal tabs remain disconnected; enabled forwards restore separately |
| Add host | Save the verified named host in the catalog | Verify the candidate via SSH |
| Discover SSH config aliases | Project hosts in memory; retain existing references | No connection or write |
| Save projected host settings | Persist a host with the existing projected ID | No connection for metadata-only changes |
| Add/edit connection method | Save the verified method on its host | Verify the candidate via SSH; leave existing terminal transports unchanged |
| Rename host/method or change preference | Save metadata; preserve references | No connection or transport switch |
| Connect host / Connect using | Select the preferred / explicitly chosen method; inspect known entries; resume the selected tab on that host, otherwise its first open tab | Authenticate through that method, inspect known IDs, then attach |
| Open session | Select/open its tab | Connect to its host and attach |
| Create shell | Persist new membership before attaching | Create one session |
| Add existing session | Remember selected entries | Enumerate only the selected host; no attachment |
| Refresh known sessions | Update observations; retain missing/unreachable entries | Inspect known IDs only, not full inventory |
| Close/detach tab | Remove tab, retain membership | Detach its view; shell continues |
| Remove from workspace | Remove membership and its tab | No kill |
| Terminate session | Remove membership after accepted kill or not-found | Explicitly terminate the session |
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
