# Architecture

The monorepo contains two products with independent responsibilities:

- `ctmux` provides persistent local terminal sessions.
- `ctl` routes terminal and task commands locally or through an SSH-authorized
  remote command. Taskd owns task definitions and background execution.

The current milestones make `ctmux` usable through a desktop client that mixes
local and SSH targets, and through `ctl` from an SSH-authorized remote client.
The remote boundary exposes the fixed `ctmux` and `task` services; generic
remote administration, files, port forwarding, and desktop control
remain out of scope.

## Process ownership

`ctmuxd` is a per-user daemon. It owns every PTY and the processes using them. A local
`ctmux` client connects over per-user IPC and may disappear without affecting a
session.

`ctl-agent connect` is a disposable SSH remote-command gateway:

```text
local:  ctmux / ctmux-app -> local IPC -> ctmuxd -> PTY -> shell
remote: ctl / ctmux-app -> OpenSSH -> ctl-agent connect -> local IPC
                                                    -> ctmuxd -> PTY -> shell
```

Each SSH channel gets a new `ctl-agent connect` process. Ending that process drops
only its local attachment stream and must not affect a terminal session. If
`ctmuxd` itself exits, an exact running PTY is not recoverable in the initial
architecture. Later disk-backed metadata may reconstruct explicitly
restartable tasks as a new process generation.

`ctl-agent` owns no terminal, session, or task state. It relays raw bytes
between SSH stdin/stdout and the selected fixed local data endpoint. The default
`ctmux` service uses `ctmuxd`; `connect --service task` uses `ctl-taskd`. The gateway
does not decode or reframe either protocol, and a remote peer cannot choose a
local socket path or service outside this enum. Taskd owns background child
processes; its interactive tasks use ctmuxd's PTYs and normal ctmux attachments.

## Crate boundaries

- `ctmux-process-info`: read-only, best-effort shell cwd and foreground-job inspection
  on macOS/Linux, independent of ctmux protocols, PTY ownership, and shell hooks.
- `ctmux-proto`: versioned, platform-independent wire messages and framing.
- `ctmux-core`: output journal and portable session-domain behavior.
- `ctmux-client`: portable client-side protocol state, checkpoint restoration,
  attachment liveness, and terminal attachment behavior over an injected byte
  stream.
- `ctmux-ipc`: per-user local endpoint selection and transport setup.
- `ctmuxd`: local IPC, PTY/process ownership, and session coordination.
- `ctmux`: canonical local CLI and reusable ctmux command implementation.
- `ctmux-app`: local/SSH Tauri/React terminal client in `apps/desktop`. Its Rust
  adapter composes `ctl-client` transport with `ctmux-client`; its webview owns
  xterm rendering, viewport, and local scrollback.
- `ctl-client`: local/SSH transport selector. Its remote path owns an OpenSSH
  child, invokes one fixed `ctl-agent connect` command, and exposes the resulting
  byte stream to the selected control-domain client.
- `ctl-agent`: per-connection SSH remote-command adapter for the fixed local ctmux
  data and task endpoints.
- `ctl`: control router. `ctl ctmux` redirects the canonical ctmux command
  surface locally by default or through an explicit OpenSSH destination.
  `ctl task` routes the managed-task command surface through the same target.

OS-specific IPC and PTY implementation details must not enter `ctmux-proto` or
`ctmux-client`.
Local IPC uses Unix-domain sockets on macOS and Linux and owner-restricted
named pipes on Windows. `ctl-agent` relays the appropriate local data endpoint
without changing the SSH transport or ctmux protocol. Unix remote commands prepend
the fixed app-managed directory to `PATH` and use `exec ctl-agent connect`;
Windows hosts with the default cmd.exe SSH shell use
`ctl-agent.exe connect`, selected through `--remote-platform windows`.
Task routing appends the fixed `--service task` arguments on either platform.

## Current invariants

1. The daemon, never the client, owns the PTY and child process.
2. Disconnecting a client does not terminate a session.
3. Output is persisted as raw bytes in a bounded in-memory journal.
4. Output positions are session-global, monotonically increasing byte offsets.
5. Reattachment resumes from an explicit stream sequence.
6. The daemon starts on demand and exits after its final session and client are
   gone.
7. Checkpoints are derived from raw VT output and never replace it as the
   canonical session record.
8. A reconnect after journal compaction restores a checkpoint and replays only
   later raw output.
9. Each live attachment may view a session independently, but input and PTY
   layout are independently leased capabilities.
10. An attachment can claim only an unheld lease; it never implicitly takes a
    lease from another attachment.
11. Leases belong to a logical attachment rather than one transport stream.
    Explicit detach releases them immediately; unexpected transport loss
    preserves them for a bounded reconnect grace, after which they are
    released while the shell continues.
12. OpenSSH owns host verification, encryption, and user authorization. `ctl`
    adds no network listener, forwarding, application key, or pairing state.
13. A `ctl-agent connect` exit closes only its local stream and never terminates an
    `ctmuxd` session. A replacement SSH channel may rebind the logical
    attachment using its memory-only token and renderer-applied raw sequence.
14. Optional shell-awareness metadata is advisory, memory-only session state.
    It is delivered as complete snapshots beside raw output, never inferred
    from rendered text or used for authorization, filesystem operations, or
    lease ownership.

An ordinary attaching client requests input only when no other attachment owns
it. It does not resize an existing PTY. A client must explicitly request the
layout lease before its terminal size is applied, so a small secondary client
cannot disturb an established desktop layout.

## Desktop client boundary

`ctmux-app` is a client, not an embedded daemon. Every Tauri request carries an
explicit local or OpenSSH target. The backend composes `ctl-client` with
`ctmux-client`, connects to the same per-user local endpoint as the CLI, and may
start a sibling `ctmuxd`, but it does not link PTY, journal,
checkpoint-production, or session-lifetime logic into the app process.
Closing the window drops its attachment and leases while the daemon-owned
session continues.

The app persists session/workspace state separately from saved host definitions.
Schema 8 of `~/.tokn/ctl/workspace.json` contains session references, cached cwd
labels, last observed terminal dimensions and times, task references, forwards,
tab order, selection, and observed remote
identities for referenced hosts. Schema 1 of `~/.tokn/ctl/hosts.json` contains
remote hosts with stable IDs, named connection methods and preferred method IDs,
and reusable gateways. The local host is synthesized. Runtime status, output,
credentials, and attachment tokens are never written to either file. A host
represents a machine, with one remote account/ctl environment per host. Addresses,
OpenSSH aliases, and gateway routes are connection methods. Each method must reach
the pinned account-owned remote UUID; matching UUIDs never merge separate hosts.

**Add host** collects the address, display name, **Connect through** choice
(Direct by default, a saved VPN, or a reusable gateway), and authentication, verifies the
connection, and automatically saves a named host with an `SSH` method. Additional
methods and gateway routes use **Host settings**. New-host creation does not
write OpenSSH config. The advanced method editor can explicitly export a new
direct method with **Also save to OpenSSH config**, off by default.

A read-only native command discovers concrete aliases from OpenSSH config and
recursive `Include` files; wildcard and negated patterns are omitted. Discovery
never opens a connection. The frontend projects aliases into runtime hosts with
deterministic `ssh-config:<encoded alias>` IDs. A saved record with the same ID
wins; otherwise an unreferenced projection is hidden when a saved method already
uses exactly that alias without overrides. Referenced projections remain distinct.
Unconnected projections stay in connection and session pickers. The sidebar
shows them after successful verification or when they have workspace references.
Connecting does not persist their definitions. Saving a customization promotes
the projection without changing its ID. Catalog serialization explicitly excludes
projected/unavailable hosts and runtime fields. Missing definitions retain
unavailable placeholders for workspace references; unavailable targets fail before
transport creation instead of treating a vanished alias as a DNS name.

`ctl host` edits this same catalog using shared Rust storage, the same
`workspace.lock`, atomic replacement, and content revisions. CLI changes retain
stable IDs and pinned remote identity; stale writers fail with `hosts_conflict`.
The CLI does not edit workspace references or promote discovered aliases unless
explicitly added. Passive host status observes each method through ctld without
starting it; explicit connect/disconnect reuse its authentication and pause
policy. An already-open desktop must reload to observe external catalog edits.

Workspace identity observations are separate from catalog identities and take
precedence when reconnecting remembered entries. They cannot overwrite catalog
metadata merely because the workspace autosaves. Host settings renames hosts and
methods and selects the preferred method. **Connect host** uses that preference;
**Connect using** explicitly chooses another method. Failure never triggers an
automatic fallback.

The frontend derives transport targets from a saved method, resolved gateway
definitions, and host-level expected identity. The selected runtime route is
separate from saved preferences. Saving changes leaves existing session
transport snapshots intact; verifying an edited method does not attach existing
sessions. An explicit connection replaces the selected route while preserving
session keys and invalidating older in-flight inspection results.

Startup restores entries and tabs with unverified runtime status, then
automatically attaches the selected tab if it is local. Remote terminal tabs
remain disconnected and session inventory is not enumerated; enabled port
forwards restore separately. **Connect host** authenticates that host,
inspects its known sessions, and resumes its selected tab (or first open tab
if another host was selected). Hosts without open tabs are only inspected.
The sidebar is workspace membership, not a mirror of
daemon inventory. New shells are remembered before attachment. **Add existing
session** explicitly enumerates one selected host and adds only chosen sessions,
without attaching. Explicit refresh inspects only remembered IDs; connection
failures retain entries as unreachable, while not-found responses mark them
missing rather than removing them. Opening a session connects on demand.

`ctld` owns each port forward globally by `forward_id` and retains its exact
SSH target and listener definition. Moving a forward to another connection
method cancels the previous listener before starting the new one; disabling it
also uses the retained owner rather than the caller's current route. This works
across desktop reloads and edits or removal of the previously selected method.
Cancellation failure preserves the old ownership for retry. Configuration and
post-authentication activation share a serialized registry so a late old-master
activation cannot recreate a moved or disabled forward. Listener ownership is
tracked separately from displayed status, and a forward configured during
master startup is not activated twice. The local `ctld` IPC protocol is version
11; older clients and daemons must be updated together and the daemon restarted.

### Component diagnostics and replacement

The About page performs bounded, passive queries against selected local owners.
It enumerates both SSH and VPN ctld endpoints, deduplicating identical owners.
An independent ctld lifecycle protocol reports build identity and data-protocol
version even when the app's data protocol differs. ctmuxd exposes equivalent
metadata through its local-control handshake, with a data-handshake fallback for
legacy owners. ctl-taskd accepts a passive control metadata query. Standalone
`--component-info` prints JSON for helper executables without starting services.

`ctl-core::component` embeds the release version, source revision, dirty flag,
and a deterministic fingerprint of Rust component sources and dependency definitions.
The fingerprint normalizes platform path separators and text line endings. It
excludes credentials, runtime configuration, and build output. Equal release
versions with unequal fingerprints are different builds, not ordered releases.
Status comparisons use the app's compiled component build as the baseline;
matching running and installed helpers do not hide a mismatch with the app.
Remote diagnostics retain identity and server-handshake metadata on existing
attachment actors; opening About never creates an SSH transport or uses persisted
host metadata as evidence of a currently running agent.

ctld restart pins a lifecycle stream to an instance and verifies the selected
replacement before confirmation. Native confirmation tokens are window-scoped,
expire, and can be consumed once. On confirmation, the same owner and replacement
are rechecked. The daemon stops accepting requests, releases its VPN heartbeat interests and drains owned
forwards, releases its endpoint, and closes the pinned stream before replacement
startup. The client verifies a fresh instance with matching build and protocol.
Legacy owners are inspected where possible but never stopped by process-name or
PID guesses. Restart does not delete saved VPN identities or profiles.

About uses the same confirmation model for local ctmuxd and ctl-taskd. A preparation
retains the existing owner's stream and hashes the selected replacement executable.
It rechecks the helper before shutdown and startup, then verifies the successor's
build and protocols. ctmuxd's control-v1 restart remains usable without build
reporting; ctl-taskd can retain an unsent control stream for legacy idle-only restart.
Taskd's daemon-side mutation lock rejects active runs and preserves storage and
ctmux endpoints through shutdown. No diagnostic query starts a missing daemon.

Remote ctmuxd preparation runs a fixed ctl-agent maintenance command through an
existing multiplexed SSH connection, with fresh authentication disabled. It pins
the account identity, daemon control stream, and installed companion executable
before awaiting confirmation. Session-reset events reconcile all app windows
belonging to the affected environment after a confirmed or possible shutdown.
Reconnect for ctl-agent instead asks each owning window to replace exact attachment
IDs while retaining the remote sessions. Native acknowledgement validates new
actors against the original environment and session before reporting success.

### Managed VPNs

`ctl vpn create` opens a provider-specific questionnaire and saves a private
OpenConnect or Tailscale profile without starting a daemon or container.
`ctl vpn start NAME_OR_ID` asks the local `ctld` to acquire heartbeat interest in
the saved profile's container, starting the daemon and recreating a missing
container when needed. Its SOCKS5 listener uses a
random loopback port, printed in a readable connection-status table after the VPN and
proxy are ready. Create, start, list, stop, and remove accept `--json` for
machine-readable output. Create remains interactive and returns only saved metadata, with prompts
on stderr. `ctl vpn list` combines saved profiles with local interests and compatible
shared containers, retaining runtime entries without saved profiles. Its JSON
snapshot adds sanitized merged `entries` to the runtime `connections`, capability
fields, and discovery warnings. Saved metadata excludes passwords and URL
credentials, paths, queries, and fragments. A saved profile without a runtime
connection is disconnected only when inventory is complete; otherwise it is
unavailable. If the catalog cannot be read, runtime entries remain available with
`profile_warnings`. `ctl vpn stop NAME_OR_ID` accepts a saved profile's exact ID
or unique exact name, or a runtime VPN ID, and releases only the selected local
interest while keeping the broker running. An omitted start selector opens a
picker in an interactive terminal. An omitted stop selector opens a picker when
the daemon reports `supports_multiple: true`; a legacy daemon's single local
connection uses untargeted stop directly. Scripts must specify a start selector;
an untargeted stop requires zero or one local connection.
List, stop, and remove never start a daemon. An absent daemon or incomplete engine
inventory reports a discovery warning rather than a confident empty result. VPN
commands reject `--host` and use
the owner-only local IPC endpoint, selectable through `CTLD_SOCKET_PATH`.

`ctl vpn remove NAME_OR_ID` deletes a saved catalog entry after interactive
confirmation, defaulting to No. It resolves an exact profile ID before a unique
exact name; an omitted selector opens a saved-profile picker. There is no `--yes`
bypass, including with `--json`. Removal retains Tailscale identity volumes and
does not revoke devices in the remote tailnet. Choosing No or cancelling makes
no IPC request or catalog write. After confirmation, remove checks complete
runtime inventory and refuses active containers (including shared ones), stopping,
or unverified state. The
VPN must be stopped and its container gone before the catalog entry is deleted.
Missing, incomplete, or legacy inventory blocks deletion; remove never starts
ctld or stops the VPN automatically. JSON success contains exactly `removed: true`,
`connection_id`, `name`, and `provider`.

The desktop VPN panel stores named connection details in private, schema-versioned
`vpns.json` under the app configuration directory. Native commands return metadata
and password-presence flags, use hashed revisions for optimistic writes, and load
the saved secret only when connecting. Both desktop and CLI use the shared
`ctl-ipc::vpn` client. CLI start resolves an exact saved profile ID before a unique
exact name, then connects through that client. CLI create, start, list, remove,
and host VPN routes share a bounded private profile reader accepting schema 1 and 2. The
catalog defaults to `~/.tokn/ctl/vpns.json`; `CTL_VPNS_PATH` selects another file.
OpenConnect structured starts send the configuration to the container
over its attached stdin; the container writes a mode-0600 environment file on
private tmpfs. No generated credential file is left on the host. Saved CLI
profiles use the same stdin/tmpfs flow. Cancellable preparation keeps status and
stop responsive. Status
lists each runtime ID, saved connection ID, gateway origin, and username from
its actual startup snapshot. Each entry independently prepares, starts, connects,
and stops; one failure or cancellation leaves the others running. Native
coordination is keyed by VPN ID so a delayed start cannot escape its own Stop or
cancel another connection. An optional snapshot in IPC 11 responses carries the
collection while preserving legacy single-connection responses. Old owners remain
readable as singleton snapshots with `supports_multiple: false`; targeted stop
requires an updated daemon. Explicit `ctl vpn stop` remains available for a legacy
owner, without risking a different connection through an implicit fallback.
Desktop status polling continues while other panels are open and updates the VPN
tab indicator. Signed development keeps its isolated daemon endpoint;
`CTLD_VPN_SOCKET_PATH` explicitly selects another daemon for native VPN operations.
Every ctld discovers the same protocol-compatible containers for its user and
engine. `locally_connected` distinguishes local interest from passive discovery.
Containers without the current protocol and ownership labels are unsupported
and excluded from inventory. Each status request observes shared inventory once
and derives its singleton response from that same snapshot.
OpenConnect creation verifies the local image's protocol label before sending
credentials and uses its immutable image ID, so a mutable tag cannot replace the
verified image. Adopting a compatible running container does not require that
image tag to remain installed. Tailscale installs the shared scripts from the
daemon's embedded entrypoint rather than relying on its upstream image.
Connect takes interest even when a compatible container is already running;
Disconnect releases it and keeps globally running status visible. Status never
renews interest. Profile mutations require complete inventory. Closing the app
does not terminate a durable daemon or its renewal tasks.

Tailscale uses the same per-profile owner and stable route IDs. Profiles carry a
provider tag; schema 1 OpenConnect files are read without rewriting and migrate
to schema 2 on a successful mutation. Tailscale settings contain only an optional
device hostname and an `accept_routes` preference. Its node identity lives in a
durable Docker volume keyed by the local owner and profile ID. Disconnect and
profile deletion retain that volume. Container names reserve each identity
exclusively. User, protocol, profile, and routing-settings labels permit
compatible reuse across daemons. Creation tokens allow cancellation to remove
only its own still-Created reservation; running containers are never force-removed.
Container inspection parses typed
JSON from Docker and Podman, including their ID spelling variants, without
engine-specific Go template assumptions.

Desktop Tailscale setup uses a provisional, window-owned enrollment. Native code
assigns the ID, fixes the connection settings, and starts asynchronously without
writing `vpns.json`. The editor observes startup, browser sign-in, device approval,
and errors, and opens the browser only for a current validated login URL. Once
connected, Save inserts the exact enrolled settings without reconnecting or
replacing its identity. Failed writes leave the enrollment available for retry.
Cancel releases the provisional connection's heartbeat interest and removes
only an unused local identity volume. If a container still references it or
inventory is unavailable, the draft stays registered with Cleanup pending and
a retry action after watchdog expiry. This does not revoke the device in the
remote tailnet. A profile
already written to disk is never removed by delayed enrollment cancellation.

Starting a saved Tailscale profile from the CLI or desktop app launches a pinned official
image in userspace mode, with a random loopback SOCKS5 port and no host route
changes. Both providers use the same in-container heartbeat watchdog. Every
interested ctld renews through an independent engine exec addressed to an immutable
container ID, every two seconds. A monotonic 15-second deadline expires only after
all senders stop; startup has its own 15-second grace. Stdin EOF and creator CLI
exit have no lifetime authority. The watchdog closes VPN and SOCKS5 services,
then the engine removes the container. Tailscale identity volumes persist.
Startup waits for the local service and SOCKS5 listener,
then returns when connected or when browser sign-in or device approval is needed.
Browser authentication can remain pending without a deadline.
Status reports `starting` plus an optional `auth_url` until Tailscale is running,
then exposes the endpoint only while the backend and proxy are ready. Stop also
cancels pending sign-in. The app asks native code to open sign-in by runtime ID;
native code fetches fresh owner status and validates the HTTPS Tailscale login
URL before opening the default browser. No caller-supplied URL is accepted.
Host connection attempts that need login return an actionable sign-in-and-retry
result, while preserving their selected VPN ID.

IPC 11 snapshots add `supported_providers`, defaulting to OpenConnect when absent,
and `supports_tailscale_enrollment`, defaulting to false for older owners.
The enrollment capability gates both setup and the identity-cleanup request.
Tailscale starts check provider support on the selected owner before sending any
profile settings. Additive status fields retain compatibility with clients that
only understand the existing stopped/starting/connected/stopping states.

Host methods persist an optional `vpn_connection_id`, independent of the VPN's
runtime port. Native mapping prepends a typed VPN hop containing that ID and the
selected VPN owner's socket path. These stable values participate in broker,
master, and credential identities. An explicit SSH probe, install, or restart
starts the saved VPN through its existing per-ID coordinator before SSH begins.
Cancelling the host attempt stops waiting without cancelling shared VPN startup.
Status, disconnect, and credential cleanup only map the stable route and never
start the VPN. The proxy helper resolves the current connected SOCKS5 endpoint
from the specified owner when opening a new transport, requires a loopback
endpoint, and fails closed when the selected VPN is unavailable. VPN and SOCKS5
routes force a private master, preventing reuse of a direct SSH-config master.

App-local settings become separate, validated OpenSSH arguments and cannot
introduce arbitrary options or change the fixed ctl-agent command.
Rows, tabs, shell-state caches, mutations, and reconnect intent use
`(stable host ID, session ID)`, independent of the host's name or selected
connection method. Task references and port forwards also retain host ownership.
A failed target reports its own error without hiding successful targets.
OpenSSH remains responsible for key contents, proxies, host verification, and
the encrypted transport. On macOS/Linux, the per-user `ctld` owns explicit
OpenSSH control masters shared by the desktop and `ctl`; each master persists
for five idle minutes. All logical service channels require the selected master
with batch mode enabled, so they cannot race by independently prompting or
using incidental user `ControlMaster` configuration. The daemon's owner-only
Unix socket carries a random-capability askpass request to the initiating
client. Desktop prompt replies remain window/attempt-scoped and single-use;
cancellation or window destruction terminates the authentication attempt.
Host trust is confirmed explicitly and remains in OpenSSH's known-hosts files.
For interactive identified connections, the fixed remote command emits an
authentication preface before attempting to execute `ctl-agent`. This lets the
credential choice complete on the same SSH channel even when the agent is not
installed, without mistaking password submission for successful authentication.
After the control master authenticates on macOS, but before any remote identity
command, `ctld` asks the initiating client to present Yes, No, and Never choices
for a newly entered reusable credential. Identity-file passphrases are eligible
only after a trusted local unlock verifies the configured key snapshot; success
of the SSH connection alone is not sufficient. Yes stores eligible credentials
in the device-local Data Protection Keychain under `biometryCurrentSet`;
retrieval requires Touch ID and changing the enrolled fingerprints invalidates
the item. No discards the candidate, while Never stores only a device-local
suppression marker for the connection scope.

Only `ctld` links Keychain code. Password entries retain their host-route/prompt
scope and are answered directly to the matching OpenSSH askpass process. New
identity passphrases use a separate namespace bound to the canonical key path,
file-content digest, and locally verified public identity. Preparation lists
public identities without reading passphrases. Only a signature request for an
eligible configured key triggers an exact protected-item read, current-file
check, and local unlock. The resulting public key must match the requested key
before the temporary agent signs. Existing agent connections retain their
session bindings, and lazy local connections replay their own binding history;
the temporary agent and unlock state live only for that connection attempt.
Cancellation and unlock timeouts mark pending blocking reads before they can
start authenticated Keychain access after acquiring the operation lock; they do
not dismiss an already-open OS authentication dialog. The passphrase is never
returned to an SSH password prompt or client. Replacing or
re-encrypting the file invalidates reuse. Legacy prompt-scoped key entries are
not silently promoted to verified identity entries. Host removal cleans up
host-scoped credentials; identity passphrases have independent, explicit Forget
actions so removing one host cannot remove a key shared by others. Native
plaintext buffers are zeroized after use. On Linux, newly entered reusable
secrets are discarded after authentication. Other interactive responses are
not stored.

The desktop **Credentials** page lists identity-file metadata and saved ctmux
credential attributes. Its bounded one-shot `ctld --credential-request` and
`ctld --identity-request` helpers provide metadata, verified identity-passphrase
writes, and exact-item deletion without restarting the running daemon.
Inventory reads separate, non-biometric metadata records with authentication UI
explicitly forbidden. Older protected attributes are imported only through an
explicit user action; interrupted imports remain retryable. Every interactive
Keychain query carries a reason identifying its credential and purpose. Identity
reuse retrieves the exact secret and binding together rather than listing every
saved identity before each read. See [credential management](credentials.md)
for discovery, state meanings, verification, and the storage boundary.

SSH startup diagnostics are bounded and returned to the client instead of being lost behind a generic
missing-transport-marker error.

Native workspace and host-catalog writes are serialized, content-revision checked
across app processes, and atomically replaced with owner-only files. Invalid or
future files are preserved and block writes. Schema 8 imports saved hosts and
gateways into the separate catalog before committing the smaller workspace;
the import is idempotent so a crash between commits is recoverable. Schema 7 is
preserved in `workspace-v7.backup.json`; earlier versions receive their own
backups and target-to-method migration. IDs and references never change.
Legacy WebView host settings migrate only when no native workspace exists;
the legacy copy is removed only after successful catalog and workspace writes.
Previous sessions were never persisted and require explicit import.
See `docs/ctmux-workspace.md` for the lifecycle and migration contract.

The GUI omits a name when it creates a shell, so `ctmuxd` applies the same
collision-safe `session-N` allocation used by every unnamed client. Its session
list merges authoritative geometry changes from the active attachment into the
matching row. **Disconnect** removes a selected open tab and preserves the
PTY; for the active tab it detaches the attachment, while an inactive tab is
already detached and is removed only from this window. **Remove from workspace**
also forgets membership without terminating the shell. **Terminate session** is the
explicit one-shot kill operation and terminates the session for all attachments.

`Restart ctmuxd` is a command-palette-only, destructive maintenance action. It
first preflights a separate owner-only local-control endpoint beside the normal
data endpoint. If an already-running older daemon does not support that
endpoint, the action returns `daemon_restart_unsupported` before detaching the
active view. After it accepts restart, `ctmuxd` atomically stops admitting new
sessions and attachments, snapshots all live sessions, and requests their
termination. Existing data connections are then closed so a stalled client
cannot pin daemon drain; a connected attachment may observe its normal
session-ended event before that close. The GUI waits for both local endpoints
to drain, then starts a fresh daemon. It never unlinks a live endpoint or
guesses and signals a process ID.

The local-control endpoint is deliberately distinct from `ctmux-proto` and is
never relayed by `ctl-agent`; a remote `ctl` client cannot restart a daemon.
The backend records the target owned by its active attachment actor, so a
local restart detaches only a local attachment; a remote attachment in the
same window remains live. The frontend marks local entries missing after a
successful or potentially destructive local restart, retaining their references
and tabs. It does not refresh or reconnect remote hosts as a side effect. The
confirmation warns that local sessions opened by other apps are affected too.
Because `ctmuxd` owns the PTYs, this is not a reconnect or session-preserving
recovery mechanism. If a restart has been accepted but the old daemon does not
drain in time, the action fails without force-stopping it. Raw-protocol version
compatibility remains the solution for an incompatible daemon, rather than
turning restart into a protocol-mismatch escape hatch. See
`docs/ctmux-local-control.md` for the local-control protocol and lifecycle.

Concurrent CLI and GUI auto-start attempts may launch more than one daemon
candidate. Candidates serialize stale-socket inspection and replacement with
an owner-only endpoint lock, so a losing candidate cannot unlink a socket that
another candidate has just bound.

The backend exposes one window-scoped attachment actor through a Tauri channel.
Each presentation event has an opaque event ID, and only one checkpoint,
output, or geometry event crosses the webview bridge without an acknowledgement.
The frontend acknowledges output only after xterm's asynchronous write callback
and acknowledges checkpoints only after recreating a clean renderer and
feeding the normalized history and both checkpoint byte fields. This keeps the
bridge bounded without tying attachment heartbeats to webview rendering speed.

Attachment transitions are serialized per window by the backend. Opening a
different session detaches and awaits the previous actor before reserving its
replacement, which releases the previous attachment's leases without ending
its persistent shell session. The frontend therefore never asks a user to
manually detach before switching sessions.

The desktop normally uses one native window and one WebView. Its terminal tabs
are client-owned presentation state over a single window-scoped attachment
actor, so only the active tab is attached. Switching tabs atomically replaces
that attachment; inactive daemon-owned sessions continue running. **New Tab in
Current Folder** creates and attaches an auto-named persistent session without
reloading the WebView. The cwd is used only after an explicit user command and
only when shell awareness has a reported or OS-observed cwd; clients never infer
a directory from rendered terminal output. Closing a tab detaches its view without ending its
shell session. If the renderer is still starting, the GUI retains only the
latest selected attachment intent and connects it once the renderer is ready.
A failed attachment leaves the selected tab retryable rather than treating
local tab selection as proof that a daemon attachment exists.

Raw byte fields cross the Tauri boundary as base64. Every `u64` sequence and
revision crosses as a decimal string so JavaScript number precision cannot
corrupt a resume or acknowledgement boundary. Attachment and event IDs fence
late callbacks from a replaced renderer generation.

The desktop status line groups shell type and cwd separately from prompt
activity, the TUI hint, input ownership, layout behavior, and daemon-authoritative
PTY geometry. Lower-priority indicators collapse as the terminal pane narrows.
Connection and history-recovery warnings take priority, while raw sequence and
revision values remain diagnostics. The GUI does not request or serialize the
sensitive editable command buffer.

The renderer adopts daemon-authoritative PTY geometry. Selecting an existing
session never acquires layout ownership or changes its PTY grid. **Resize with
window** explicitly acquires the layout lease, measures the terminal container,
and sends debounced PTY resizes while that lease remains held; turning the mode
off releases the lease. A GUI-created session may request the lease with its
initial attachment because that GUI establishes the new session's layout.
Scrollback, scrolling, selection, and copy remain local xterm behavior and
generate no protocol viewport commands.

App actions are registered as stable frontend command IDs. The command palette,
toolbar, session list, and app-local shortcuts dispatch through that shared
registry, including dynamic commands for switching to a known session. The
webview intercepts only an exact enabled or disabled registered key combination
before xterm; unregistered keystrokes remain PTY input. Command search,
selection, and focus are client presentation concerns and do not add protocol
messages or daemon state.

The new-tab command uses `Cmd-T` on macOS and `Ctrl-Shift-T` on
Windows/Linux. It remains disabled with an explanatory palette reason until a
current shell-reported cwd is available.

On macOS, the native application menu routes `Cmd-W` to the shared detach-tab
command and `Cmd-E` to the shared close-session confirmation command. The
WebView does not also process those accelerators, preventing one keystroke from
dispatching twice. `Cmd-Q` remains the standard application quit and therefore
detaches without killing daemon-owned sessions. Windows and Linux use
`Ctrl-Shift-W` and `Ctrl-Shift-E` in the WebView so terminal `Ctrl-W` and
`Ctrl-E` remain PTY input.

## Attachment ownership

`ctmuxd` treats each `attach_session` request as a logical attachment. Its
identifier remains daemon-private and is not a client identity. The client
receives only a random, memory-only token that may rebind a replacement
transport during a bounded grace period.

Input and layout leases are independent. One attachment can type while a
different attachment owns PTY resizing. Requests to acquire a held lease leave
the requester attached as a viewer; they never force a takeover. Only
possession of an attachment's token may supersede that attachment's stale
transport generation; it does not displace another logical attachment.

To prevent a sleeping or half-open client from pinning either capability,
`ctmuxd` negotiates a heartbeat cadence and liveness deadline during the
handshake. Only inbound client activity renews that deadline after the initial
attachment transfer. That transfer has its own finite delivery deadline, since
a client learns the heartbeat cadence only after `attached` and `ctmuxd`
serially delivers initial replay before it can process queued heartbeats. A
silent transport is closed, but its logical attachment remains resumable for
one bounded reconnect interval. Possession of the attachment token immediately
supersedes the stale transport generation while preserving its leases. An
explicit detach or expired grace releases the leases; the PTY, shell, journal,
and checkpoint state remain intact.

## Remote control boundary

`ctl ctmux`, persistent `ctl shell`, and `ctl task` connect directly to the current
user's owner-only endpoint for the chosen command domain by default. Global
`--host`/`-H` resolves saved ctl hosts before OpenSSH aliases/destinations and
invokes the system OpenSSH client with
PTY allocation and all forwarding disabled, an OpenSSH destination supplied
by the user, and a fixed managed-directory `PATH` prefix followed by
`exec ctl-agent connect` for ctmux or `exec ctl-agent connect --service task`
for tasks. The desktop may additionally run closed, fixed platform-probe and
per-user installation commands after explicit user action; callers cannot
supply a command, version path, or archive destination. OpenSSH
configuration owns host verification, user authentication, and proxying. On
Unix clients, `ctld` owns the explicit authenticated control master used by
both `ctl` and the desktop. These service transports never disable host-key
checking, enable agent forwarding, or accept an arbitrary remote command.

The ordinary `shell --plain`, `exec`, `ssh`, and `scp` commands operate through
OpenSSH directly, preserving its authorization boundary without adding an
arbitrary-command RPC to `ctl-agent`. `ssh`/`scp` use saved ctl host names as host
aliases; compatible Unix sessions reuse ctld's master. Explicit transport
options bypass managed reuse and retain OpenSSH behavior. `port` manages the
existing ctld forwarding registry. Host/catalog models and read-only Tailscale
discovery live in `ctl-client` for both desktop and CLI use; saved files retain
their existing schema and location. See [Proposal 0008](proposals/0008-connection-cli.md).

`ctl-agent connect` has no network listener or session registry. Its persistent
account-owned UUID lives in `~/.tokn/ctl/remote-id`; it is an environment identity,
not a machine ID. Its service enum chooses ctmux or task. It writes one fixed
readiness marker, after which SSH stdin/stdout carries that service's raw protocol;
diagnostics use stderr. The helper connects only to the current user's fixed
data endpoint and cannot reach ctmux's owner-only maintenance endpoint. Taskd
may use maintenance locally to manage interactive runs. Its authority is exactly
that of the already SSH-authenticated operating-system account.

Reconnect state stays inside `ctmuxd`. Its opaque attachment tokens are random,
memory-only, session-scoped credentials for rebinding a replacement stream;
they are not device identities or substitutes for SSH authorization. See
`docs/ctl-protocol.md` for the transport contract and `docs/remote-mvp.md` for
setup.

## Shell awareness

`ctmuxd` can track a shell descriptor, cwd display string, prompt phase,
optional editable command buffer/cursor, optional bounded running-command
summary, native process identity/foreground job, and an alternate-screen
presentation hint. This is not terminal emulation and does not introduce
viewport commands: clients still own
scrolling, selection, search, and rendering.

`ctmux-process-info` anchors observations to the spawned child's PID and birth token.
On macOS it uses libproc; on Linux it reads `/proc`. The daemon supplies the
PTY's foreground process group, and the crate verifies candidate ancestry back
to the managed shell. Only process names are collected, never argv or environment.
The cwd is the root shell's physical cwd, not a foreground child's or nested
shell's cwd. Shell reports take precedence and preserve logical `$PWD` spelling.
OS observations never set shell-integration capabilities or infer prompt state.

One worker per session manager samples live sessions roughly every 500 ms,
including detached sessions. Only changed snapshots advance metadata revisions;
updates use the existing latest-state channel, not the raw journal. The worker
holds no session or PTY lock across process queries. It holds weak references and
is not joined on shutdown: a stuck network-filesystem cwd lookup can delay
metadata for this manager but cannot stall terminal traffic or shutdown. Lookup
failures clear native observations without discarding shell-reported fields;
session end disables sampling and clears live process identity. These are
advisory, non-atomic samples, never authorization or proof of liveness.

This layer needs no dotfile edits. Automatic, dotfile-free shell integration is
a later layer. The app already receives cwd through its existing adapter; the
new process fields are currently available in the Rust/wire model, not yet
mapped into the app's title UI.

On the current Unix implementation, an opt-in shell integration writes bounded
full snapshots to a unique owner-only FIFO supplied as
`CTMUX_SHELL_STATE_PIPE`. The integration removes that environment variable and
opens the FIFO only for each report, so commands it executes do not inherit a
reporter capability. The FIFO is not an `ctmux-proto` client endpoint. That
keeps the reporter's separate typed-buffer records out of raw journal,
checkpoints, replay, and future journal persistence; normal terminal echo is
still canonical raw output. Reports are advisory because a child process can
lie; `ctmuxd` assigns the revision and output-sequence correlation itself, and
coalesces/rate-limits reports before they can contend with PTY ingestion.

The live edit buffer and running-command summary may contain secrets. They are
never in `SessionInfo` or the session list, and `get_shell_state` always
redacts both. An attachment must opt in to each separately and currently own
the input lease before `ctmuxd` sends it. The shipped `zsh` v2 integration
replaces editable text with a bounded running-command summary before command
execution; `bash` does not advertise either live-editing or running-command
capability. `ctmux attach` and `ctl ctmux attach` are raw terminal presenters and
intentionally do not request or print either value.

Version-2 FIFO reports preserve the version-1 nine-field NUL-delimited wire
shape. Their active-text fields are phase-exclusive: prompt phases carry an
editable buffer/cursor while `running` carries only the non-editable summary.
The daemon still accepts version-1 reports, which retain their old
editable-buffer-only semantics.

`tui_hint` means only that DEC alternate-screen modes were observed. It is not
a classification of the child process: some TUIs do not use alternate screen,
and normal applications can use it. It may inform a client overlay but never
changes input or layout ownership.

## Checkpoints

`ctmuxd` continuously interprets raw output into terminal state. At bounded
output intervals, or before a prior checkpoint would no longer bridge retained
journal data, it creates a versioned checkpoint. A checkpoint captures the
live terminal state and the parser state required to consume subsequent output.
At that exact raw sequence, `ctmuxd` also captures a bounded full replacement of
normalized logical lines above the live grid. It does not capture process
memory, shell-awareness state, or cwd. A current shell-awareness snapshot
travels separately with `attached` and later state-change messages.

The current `ctmux` CLI restores a compatible checkpoint by writing its VT
restore stream to the local terminal. It reports a size mismatch but does not
resize the remote PTY. Because a native terminal cannot atomically replace its
outer scrollback, the CLI does not inject the normalized history snapshot. The
GUI recreates its owned renderer and restores both history and live state.

### Renderer-safe checkpoint application

A checkpoint is an initialization program for a clean terminal renderer, not
an idempotent screen delta. In particular, the current `ctmux_vt_state` version
1 payload is an `avt` VT dump. It recreates the represented buffers and modes,
but does not promise to erase unrelated content or parser state already held by
the receiving renderer.

A graphical presenter that supports this format must therefore:

1. Verify the format and format version before changing its live presentation.
2. Stop applying later output while the restore is in progress, with a bounded
   local queue.
3. Discard or recreate its terminal emulator at the checkpoint's
   `terminal_size`; this resets both screen buffers and its input decoder.
4. Seed the paired normalized history above the new live grid. The history and
   checkpoint sequences must match; neither is valid without the other.
5. Feed `payload`, then `input_prefix`, byte-for-byte through the same terminal
   input path that will consume later raw output. `input_prefix` may be an
   incomplete UTF-8 prefix, so it must not be converted to text or decoded
   separately. Incomplete terminal-control parser state is represented by the
   checkpoint payload itself.
6. Treat `checkpoint.sequence` as the next raw-stream offset after the
   restored state. It is not derived from the length of either checkpoint
   field. Apply only output beginning at that offset after the restore.

The current raw stdio presenter establishes that clean state with a terminal
reset and full-screen clear before writing the version-1 stream. A graphical
client should instead recreate its terminal model; it must not depend on a
particular physical terminal's reset behavior.

The renderer must not send `resize` merely to match the checkpoint dimensions.
PTY geometry remains layout-owner controlled; a viewing client renders at the
daemon's geometry and may use its own viewport scaling or scrolling around
that grid. An unsupported checkpoint is an explicit compatibility failure: the
client must not advance its resume position or claim that the visible terminal
state was restored.

### GUI render progress and reconnect

The GUI has an acknowledgement boundary between its Rust transport/controller
and the asynchronous webview terminal emulator. It tracks two different
positions for each attachment:

- `received_next_sequence`: the end of validated output accepted from the
  transport;
- `applied_next_sequence`: the end of output whose effects the renderer has
  completed, after the renderer's write callback or equivalent completion
  signal.

Only `applied_next_sequence` is eligible for the next `attach_session`
`resume_from` value. Receiving an event in the webview, placing it in a queue,
or starting an asynchronous terminal write is not enough. A checkpoint becomes
applied only after the fresh renderer has accepted its `payload` and
`input_prefix`; its applied position is then exactly `checkpoint.sequence`.
The controller sends that progress to `ctmuxd` as coalesced
`presentation_applied` delivery credit. The daemon stops sending presentation
events at the negotiated byte/event window instead of closing the transport.
Heartbeats and control messages remain live while output is paused.

Delivery progress and safe resume state remain distinct. An incompatible
checkpoint may be acknowledged to replenish the daemon's window while the
client keeps its safe resume cursor absent. On reconnect, a GUI uses its local
safe sequence only when it retained a compatible renderer; otherwise it omits
`resume_from` and begins from a checkpoint/history replacement.

If a presenter observes a geometry transition but cannot adopt its PTY grid,
it must acknowledge that incompatibility locally rather than treating the
transition as applied. It may continue displaying bytes, but its reconnect
cursor remains absent until it has applied a later checkpoint. The raw stdio
adapter follows this policy because printing an updated size warning does not
resize the user's terminal.

### Authoritative history and client presentation

The daemon owns two bounded terminal representations: a mutable live emulator
and normalized complete logical lines above its grid. Raw PTY output remains the
ordered delta and short replay journal. At every checkpoint boundary, the
daemon snapshots history and live state together; subsequent raw output evolves
both server and client emulators from that boundary.

Historical lines have already interpreted terminal controls, merge soft wraps,
and exclude alternate-screen output. Version 1 stores text only; style runs are
deliberately deferred. `RIS` and erase-saved-lines start a new generation. A
resize may move the live/history boundary, so version 1 sends a full history
replacement rather than stable incremental line IDs.

Selection ranges, search indexes, viewport position, and any retention beyond
the daemon bound remain client-local presentation data. Scrolling never becomes
a daemon viewport command. Applying a checkpoint recreates the GUI renderer,
seeds its paired history above the grid, restores the live payload, and only
then applies output deltas. If `history_gap` is true, the UI marks the missing
oldest portion even though the live screen is authoritative.

### GUI foundation acceptance tests

Tab, split, and remote-host UX must preserve the following attachment-layer
behavior, proven with a test renderer and, where possible, the chosen terminal
emulator in headless mode:

- Restoring a version-1 checkpoint into a deliberately dirty renderer removes
  stale state, applies the checkpoint at its own dimensions, and matches the
  expected screen/cursor/mode state.
- A checkpoint followed by split UTF-8 input preserves a single byte decoder
  across `payload`, `input_prefix`, and later output; a checkpoint whose
  payload contains partial terminal-control parser state accepts its later
  continuation correctly.
- A delayed renderer acknowledgement never advances `resume_from`; reconnect
  from the last applied sequence loses or duplicates no raw output.
- Recovery with `history_gap` restores the live screen while clearly preserving
  the local-history discontinuity.
- Restoring a checkpoint/history pair places normalized logical lines above the
  live grid without duplicating them in the viewport.
- An ordered daemon geometry update changes every attached renderer's grid at
  its stream boundary without causing a viewing client to send `resize` or
  acquire layout ownership.
- A stalled webview exhausts presentation credit instead of closing the
  transport; the Rust-to-webview queue stays bounded and heartbeats continue.


## Managed tasks

Taskd owns task definitions, desired state, and run records. Background runs use
pipes and process ownership in ctl-taskd; interactive runs use PTYs and process
ownership in ctmuxd. Taskd uses the owner-only ctmux local-control endpoint for
idempotent creation and lifecycle reconciliation. Terminal input, output, leases,
and geometry continue through normal ctmux attachments, including `ctl task attach`.

Interactive run intent is persisted before creation, keyed by task/run UUID and
pinned to the ctmuxd instance UUID. Taskd can recover a live session or its retained
exit result after restarting. It acknowledges the result only after saving it.
An ctmuxd replacement fails old runs without automatic recreation. The CLI routes
task registration, lifecycle, and background logs to the selected local or SSH
target. Interactive attachment uses a separate ctmux channel to that same target;
remote socket metadata never selects a local endpoint. Remote definitions default
to the remote user's home directory, and relative working directories resolve
there. The desktop workspace task interface currently uses local ctl-taskd. See
[proposal 0003](proposals/0003-task-system.md) and the
[local-control protocol](ctmux-local-control.md) for lifecycle and retention rules,
and [proposal 0006](proposals/0006-remote-tasks.md) for SSH service selection.

The local CLI currently captures cwd when a task is created. A registered task
has a unique name and at most one active run, and the desktop reuses one default
registration per saved definition and host. These remain the current behavior.
[Proposal 0007](proposals/0007-local-task-workflows.md), still **Proposed**, describes
extending saved definitions to independent local runs, explicit caller or fixed
working directories, and local scheduling. It preserves the SSH gateway boundary
and defers arbitrary shell-job adoption through `Ctrl+Z` and `task bg`.

Saved local definitions now use the shared `ctl-task-store` crate. CLI and desktop
read the same project/global catalogs; workspace schema 3 retains only definition
references, source selection, and drafts. Saving uses content revisions and
atomic replacement, independently of ctl-taskd's registered-task state. See
[shared task definitions](task-definitions.md) for storage and migration.
