# ctmux app

The desktop client for local and SSH-connected daemon-owned `ctmux` terminal
sessions. It uses Tauri 2, React/TypeScript, and xterm.js.

Connection indicators distinguish observed SSH availability, active terminal
attachments, and last-known session activity. See the
[state definitions and transition rules](../../docs/connection-state.md).

## About and component versions

Open **About ctmux** from the info button, command palette, or macOS app menu.
The page shows the app version, local `ctld`, `ctmuxd`, and `ctl-taskd` versions and
protocols, and `ctl-agent`/`ctmuxd` metadata observed on active remote terminal
connections. Separate SSH and VPN ctld owners appear separately when configured.
Running versions are compared with this app's component build. Each component
uses one compact row with its version, protocol, status, and action. Hover over
the component, version, or protocol for explanations and full build metadata.
Local rows also show the selected helper version.

**Outdated** means a lower comparable release version. **Different build** marks
different source fingerprints without claiming which is newer. **Protocol
mismatch**, **Build not reported**, **Build unverified**, and **Not running**
remain distinct. Legacy processes keep their known versions and protocols visible;
a missing build is never inferred from the installed executable. Refreshing About
does not start services, connect hosts, deploy remote components, or detach
terminal sessions. A disconnected host's saved metadata is not presented as a
live version.

Each supported daemon row offers **Restart**, with a replacement check and an
impact confirmation. The replacement's build and protocols are verified afterward.

- `ctld` stops its owned port forwards, releases its VPN heartbeat interests, and
  can interrupt SSH connections. A shared VPN remains available while another
  daemon keeps it alive, then expires after its heartbeat timeout. Surviving SSH
  masters can be reused after restart.
  Saved VPN profiles and Tailscale identities remain available for reconnecting.
- Local and remote `ctmuxd` restarts end all of that daemon's terminal sessions,
  including other clients and interactive tasks. Runtime options return to the
  replacement's defaults.
- `ctl-taskd` refuses while tasks are running and preserves its storage, definitions,
  history, and terminal-daemon endpoint.
- `ctl-agent` offers **Reconnect** for the matching app transports across windows.
  Remote terminal processes remain running; the new connections verify identity
  and report the actual agent build.

Preparations expire without changing the daemon. Remote preparation uses an
existing authenticated SSH connection. An older remote agent must be updated
before it supports prepared daemon restarts; older ctld owners without lifecycle
support require a manual restart once. Unavailable or incompatible replacement
helpers are rejected before shutdown. Restarting uses the selected installed
helper and does not install a newer build.

## VPN connections

Open **VPN** in the sidebar to add a named OpenConnect or Tailscale connection.
OpenConnect uses a server, username, and password; advanced settings include an
authentication method and an optional SSH connectivity-check target. Save an
OpenConnect connection, then choose **Connect**. For a new Tailscale connection,
enter a name and choose **Sign in with Tailscale**. The dialog shows startup
progress and opens your browser when sign-in is ready. Finish authentication,
review the connected account and tailnet, then choose **Save connection**.
**Open browser** lets you retry opening the sign-in page. Settings are saved only
after login succeeds; Cancel releases the unsaved connection and removes its
local identity once its container has stopped. If the shared container is still
running or its status cannot be checked, the dialog reports **VPN cleanup
pending** and offers **Retry cleanup**. A failed save keeps the authenticated
setup available for retry.
Closing the dialog also retries pending cleanup in the background for up to
25 seconds. If the app exits first or cleanup remains unconfirmed, the unsaved
identity is preserved; cleanup never forcibly removes a shared running container.

Tailscale's optional **Device name in Tailscale** is under Advanced options. It
names this VPN device in the Tailscale device list; ctmux assigns a name if left
blank. Advanced options also allow access to advertised subnet routes.
Disconnecting a saved connection retains the Tailscale device's login.
Each saved connection has one item combining its settings and live status,
including the VPN server, username, and copyable SOCKS5 endpoint when connected.
An active connection without a matching saved profile appears as a temporary item. The VPN tab shows an active
indicator even while another panel is open. Connections started from the CLI are
also visible, including their server and username. Multiple VPNs can run at once,
each with its own SOCKS5 endpoint. A shared container appears as **Connected**
even when this app's daemon has no heartbeat interest in it. **Connect** on a
matching saved profile lets this daemon keep it alive. **Disconnect** releases
only this daemon's interest and also cancels its pending startup; the container
remains visible while it is running. Settings cannot be changed or deleted until
the container stops. An older daemon can still be inspected, but must be
updated to connect multiple VPNs or disconnect a connection by ID. The CLI's
untargeted `ctl vpn stop` remains available for its current connection.

Only containers using the current shared-heartbeat protocol appear in inventory.
Incomplete inventory shows **Status unavailable** rather than claiming all VPNs
are disconnected.

Connection settings, including OpenConnect passwords, are stored in a private `vpns.json` file in
the app's configuration directory. The webview receives metadata and a
password-presence flag; editing with an empty password keeps the stored password.
The OpenConnect adapter generates its environment file internally when connecting.
Users do not need to create or select an environment file. Tailscale stores its
device identity in a private Docker volume, separate from the JSON settings.
Existing OpenConnect profiles remain readable and migrate on the next save.

Signed development uses its persistent, per-worktree signed daemon for VPN and
SSH operations. Each daemon discovers shared VPN containers through the local
Docker engine, so the CLI and app can see the same containers while keeping
independent connections to them. Set `CTLD_VPN_SOCKET_PATH` to select a custom
VPN daemon; otherwise VPN operations use `CTLD_SOCKET_PATH` like SSH operations.

This requires a running Docker-compatible engine. Build the OpenConnect image
with `./docker/openconnect/run.sh build` from the repository root; Tailscale pulls
its pinned official image on first use. Tailscale requires an updated `ctld`.
On macOS and Linux, each connected `ctld` sends heartbeats to its VPN container.
Closing the app leaves its daemon and VPN interest running. Disconnecting or
exiting `ctld` releases that daemon's interest; after all heartbeats stop, the
container exits automatically after its timeout. `ctl vpn status` distinguishes
connections kept alive by **this ctld** from discovered **shared** containers.
The SOCKS proxy follows the container's routes;
it does not change the Mac's system routes. Choose a saved VPN in a host's
**Connect through** step to route that host through its current SOCKS5 endpoint.
See the [OpenConnect guide](../../docker/openconnect/README.md) or
[Tailscale guide](../../docker/tailscale/README.md) for setup and identity retention.

## Develop

From the repository root, install the frontend dependencies and start Tauri:

```sh
cd apps/desktop
pnpm install
pnpm tauri dev
```

On macOS and Linux, Tauri builds `ctld`, `ctmuxd`, and `ctl-taskd` before every native
launch, including Rust hot reloads. The Cargo runner preserves the selected
target, profile, and output directory. On Windows, helpers are built once at
development startup. `pnpm dev` starts only the frontend; `pnpm daemons:build`
rebuilds local daemons separately. Ordinary development does not replace an
already running daemon; `ctld` uses protocol-specific sockets, and rejects an
incompatible helper executable before starting it.

Development startup also performs a local-only bundle preflight. It warns but does
not block local or already-provisioned SSH work when remote install bundles are
absent. To test installation on a new SSH host, first commit and push the
current branch, then run:

```sh
pnpm agents:sync
```

The command reuses or dispatches the bundle workflow for the exact commit,
waits for it, verifies all four archives, and stages them in the ignored Tauri
resource directory. `pnpm agents:sync --main` is an explicit compatibility
shortcut for using the latest successful main-branch set.

The app may also use the path in `CTMUXD_BIN`. A saved host represents a named
machine with one remote account/environment. Addresses and gateway routes are
named connection methods on that host. **Add host** guides you through
`[user@]hostname[:port]` (or an SSH config alias), a display name, **Connect through**,
and authentication. **Direct** is selected by default and uses the host's SSH
settings. You can instead select a saved VPN or an existing SSH/SOCKS5 gateway.
A selected VPN starts automatically when you connect the host; its saved ID
keeps the route valid when its randomly assigned port changes. If that VPN is
missing or cannot connect, the host connection fails without a direct fallback.
Cancelling a host connection leaves the VPN available to other hosts; use the
VPN page to disconnect it. After verification, the app saves the named host and
its first `SSH` method in `~/.tokn/ctmux/hosts.json`. Display names may contain
spaces; they are independent of SSH aliases. No storage-choice step or implicit
OpenSSH config write is involved.

Concrete aliases discovered from `~/.ssh/config`, including its `Include`
files, are grouped under **SSH config · Virtual** in Add host, Connect host,
and session pickers, separately from **Saved hosts**. They enter the
sidebar after a successful connection; hosts with remembered sessions, tasks,
or forwards remain visible after restart. OpenSSH continues resolving their
connection settings. Connecting does not save their definitions; saving a customization
from **Host settings** creates a saved host with the same ID. A saved pure alias
method suppresses an otherwise-unused duplicate projection. Missing aliases
with workspace references remain visible as unavailable; restoring the alias
restores access without losing session or task references.

The connection-method editor supports direct SSH, saved VPNs, optional
identity-file paths, and ordered routes through reusable gateways. **Verify and save** verifies the
remote environment and saves the method in the host catalog. For a new direct
method, **Also save to OpenSSH config** optionally exports a managed `Host`
block after verification. This option is off by default and unavailable for
existing config aliases, VPNs, or gateway routes; the saved method remains in the
host catalog. OpenSSH requests host-key confirmation, passwords,
passphrases, or interactive responses through the quick-input overlay. If
`ctl-agent` is missing, packaged builds can install the matching
checksummed `ctl-agent`, `ctmuxd`, and `ctl-taskd` bundle for the remote user. Custom
command paths are not supported. Each development commit has a distinct bundle
ID, so a remote host cannot silently retain an older build with the same app
version. App-local connection settings are supplied to OpenSSH as fixed
arguments; methods based on an existing SSH config alias retain that alias.
On macOS, SSH passwords and verified identity-file passphrases use separate
entries in the device-local, Touch ID-protected Keychain. Password entries are
scoped to the host route and prompt; passphrases are bound to the canonical key
path and verified file version, so multiple hosts can reuse the same key. The
private key stays in its file. `ctld` is the only component that accesses
Keychain, either as the running connection daemon or as the signed one-shot
helper used by **Credentials**. That page lists identity files and saved metadata
and supports verified Save/Replace and individual Forget actions. It never
reveals saved secrets or deletes identity files. See
[credential management](../../docs/credentials.md).

On macOS and Linux, connection methods have a **Use SSH-config master** checkbox
in **Host settings → Edit connection method**. It defaults on for **SSH config · Virtual** methods and
off for direct and Tailscale methods. VPN and SOCKS5 routes always use a private
master so an existing direct connection cannot bypass the selected route.
Checked methods honor the destination's effective
`ControlMaster`, `ControlPath`, and `ControlPersist` settings, including `Include`
and `Match` rules. Unchecked methods use ctmux's private master while retaining
their alias and other SSH settings. The choice and alias origin are retained per
method in `hosts.json` when the host is saved or customized; older saved methods
keep their defaults. Verification uses the selected mode, and an explicit
connection applies it to remembered sessions. An existing configured master can be reused even
with `ControlMaster no`. If no usable control path is configured, or sharing is
disabled and no master is running, ctmux uses its private master with a five-minute
idle lifetime and protocol keepalives that detect an unresponsive server after
roughly thirty seconds. Existing and configured shared masters keep their current
policy. For
`ControlMaster ask` or `autoask`, start the alias in a terminal first so its
master retains a working helper for sharing confirmations; ctmux can then reuse it.

Open **Host settings** from the host row to rename the machine or its methods,
add or edit a connection, remove a method while retaining at least one, or
**Make preferred**. **Connect host**, **New shell**, and **Add existing session** use
the current saved preferred method. **Connect using**
chooses a specific method for the current connection without changing that
preference; there is no automatic fallback. Saving names or preferences does not
connect. Adding or editing a method verifies its endpoint before saving, but
does not switch an existing session's transport. Use an explicit connection to
apply that route to the host's remembered sessions.

Remote host rows show live **Connected**, **Connecting**, **Disconnected**, or
**Error** status; hover the status to see active connection methods or diagnostics.
**Disconnect host** closes ctmux's channels for the host's saved and active
methods and pauses its forwards. It stops private masters, but leaves configured
masters and other applications' channels under OpenSSH's lifetime policy. Port
forwards through a configured master use local listeners owned by ctmux, so
disconnecting cannot remove another application's forward. Remote shells and tasks keep running, and tabs,
credentials, and saved forwarding preferences are retained. Use **Connect host**
to resume. Other ctmux windows using those methods also disconnect. Status and manual
pauses are runtime state; status checks never authenticate or start `ctld`.

All methods on a host must reach its verified account-owned ctl environment.
Use a separate host for another account. Matching remote IDs never merge saved
hosts automatically. Renaming a host or changing methods preserves session,
tab, task, and port-forward ownership through the stable `host_id`.
Existing WebView host settings migrate automatically after a successful disk write.

**Tailscale · Virtual** lists online devices discovered from the installed Tailscale
client in Add host, Connect host, New shell, and Add existing session. Discovery
does not block workspace startup, contact remote SSH servers, or save discovered
definitions. A device enters the sidebar after connection or when it has remembered
work; naming/saving it through Add host or customizing Host settings creates a
saved definition. Each Add host opening refreshes discovery and hides stale
Tailscale suggestions while loading. Refresh sessions also refreshes devices.
Saved hosts and remembered work remain available when devices go offline. Online/Offline
details describe Tailscale presence, separately from the host's SSH connection status.

Discovery checks `PATH` and standard app/CLI installation locations. On macOS this
includes `/Applications/Tailscale.app/Contents/MacOS/Tailscale` and the equivalent
under `~/Applications`, with `TAILSCALE_BE_CLI=1`; no shell alias or PATH change is
required. Missing, stopped, signed-out, or unresponsive clients produce a message
in the host selectors while local and saved hosts remain usable.

Saved methods bind to the Tailscale device ID, resolving its current address for
new connections without changing active session transports. A missing device stays
unavailable until discovery finds it again. Connections use ordinary SSH over the
tailnet, including the usual SSH user/config and credential settings; browser
approval for Tailscale SSH check mode is a separate follow-up.

Saved host definitions and reusable gateways live in `~/.tokn/ctmux/hosts.json`;
sessions, tabs, tasks, forwarding, and observed remote identities live in
`~/.tokn/ctmux/workspace.json`. On first load, an existing workspace is imported
from Tauri's former app-data directory if the new file does not exist; the
original remains available for recovery. Schema 8 moves existing hosts and
gateways into the catalog before removing them from the workspace. Schema 7 is
backed up as `workspace-v7.backup.json`; earlier schemas retain their corresponding
backups. Host IDs and all session/task/port references remain unchanged.
It remembers known sessions, cached paths, last observed terminal sizes and times,
tab order, and selection. Unattached sessions show a compact age such as `2h ago`;
hover shows the exact observation time. Startup
restores those entries as unverified and automatically connects the selected
local tab. Remote terminal tabs stay disconnected until explicitly opened; **Connect
host** resumes that host's selected tab, or its first open tab if another host
was selected. No daemon inventory is discovered automatically. Use **Add
existing session** in the sidebar or command palette to discover one host's
inventory and explicitly remember sessions without attaching. **Refresh Known
Sessions** inspects only remembered IDs; it does not adopt other apps' sessions.
**New shell** and **Add existing session** authenticate the selected SSH host
before creating or discovering sessions, reusing its connection when available.
Credential prompts appear within the action, which continues after verification.
Cancelling authentication creates or imports nothing; New shell keeps the working
directory draft. Background refreshes never prompt for authentication.
Old sessions were never saved, so the first migration requires explicit import.
See [workspace persistence](../../docs/ctmux-workspace.md) for recovery and tests.

The Ports activity panel lists every saved local forward across SSH hosts,
including stopped entries, and shows `ctld`'s current active, waiting, or error
state. Start and Stop act on the shared `ctld` owner; selecting a row opens the
host-specific forwarding dialog for discovery and editing. Runtime state is
refreshed on launch, when the panel opens, and on demand, but is not persisted.

The identity-file input suggests candidate files from the top level of
`~/.ssh`. Type to filter, use the arrow keys and Enter, or click a file. Manual
paths remain available, including when discovery fails. Rust lists names and
metadata only: it does not open key contents. Public keys, common SSH support
files, backups, and directories are omitted; symlinks to regular files are
supported. Suggestions are not proof that a file is a valid private key—OpenSSH
validates the selected identity when connecting.

Wildcard and negated `Host` patterns are not destinations and are omitted from
suggestions. Discovery only fills the editor: the app contacts a candidate when
**Verify and save** or an explicit connection is requested. Local is always
present and remains the default for a new shell. The sidebar groups remembered
sessions under their named hosts. A failed host reports its own error while
last-known sessions from other targets remain usable.

SSH uses `ctl-client` and the system `ssh` executable with a fixed remote command
that prepends the app-managed directory before running `ctl-agent connect`;
SSH agent forwarding, X11, local commands, and PTY allocation remain disabled.
Local agent identities can be used for authentication without forwarding that
agent to the remote host. On macOS/Linux, the per-user `ctld` selects a configured
OpenSSH master when **Use SSH-config master** is enabled or owns a private master for other
methods and the fallback described above. Its owner-only Unix socket carries
askpass requests to the quick-input UI for an active connection attempt. Host-key trust
requires explicit confirmation and is managed by OpenSSH. Private masters remain
available for five idle minutes; configured masters retain their configured
lifetime. All background channels require the selected master and cannot
independently prompt or fall back to another connection.

On macOS, only `ctld` links the Keychain implementation. Saved SSH passwords and
identity passphrases use a device-local, Touch ID-only policy tied to the
currently enrolled fingerprints. Retrieved passwords satisfy the matching
OpenSSH prompt inside `ctld`; saved identity passphrases instead unlock their
verified key locally in a temporary agent and are never supplied as answers to
SSH password prompts. Key preparation uses public metadata and reads a saved
passphrase only when SSH requests that key's signature. Password-only connections
do not unlock unrelated keys. Successful SSH authentication alone does not establish
that an entered passphrase unlocked a key: a new passphrase must pass local
verification before it can be saved. Changed or re-encrypted key files require
verification and replacement of the saved passphrase.

Saved secrets never return to the desktop client. Native plaintext buffers are
zeroized after use; secrets never appear in command arguments, environment
variables, or logs, and one-time responses are not stored. Removing a host asks
`ctld` to delete host-scoped credentials for its saved and active methods,
retaining any scope still used by another host. Identity-bound passphrases are
managed separately in **Credentials** and remain available to other hosts using
the same file. On Linux, `ctld` keeps a newly entered reusable secret only through
authentication and then discards it. An explicit interactive attempt allows up
to three minutes and Escape cancels it. On non-Unix platforms, preconfigured noninteractive SSH
remains available.

On macOS, `ctld` is packaged as the app-like helper
`ctmux.app/Contents/Helpers/ctld.app`. Release builds sign that helper with the
permanent `io.ctmux.desktop.ctld` bundle identifier and embed its matching
Developer ID provisioning profile. This gives `ctld` its own Keychain identity;
the main app and the other sidecars receive no credential-access entitlement.
Without the profile-authorized application identifier, the Data Protection
Keychain rejects credential storage and ctmux reports the signing error instead
of silently weakening the access policy.

For local Touch ID testing with any Apple Account, first run
`pnpm tauri:dev:provision`. In the Xcode project it opens, select the
`ctld-provisioning` target, choose your Personal Team under **Signing &
Capabilities**, and build once. This Xcode project is copied under `target/`,
so the local team selection does not modify tracked files. Free Personal Team
profiles expire after seven days; after initial setup the signed-development
launcher asks Xcode to refresh an expired profile automatically.

Then run `pnpm tauri:dev:signed`. The launcher searches Xcode's downloaded
profiles and `~/Library/Application Support/ctmux/signing/ctld.provisionprofile`,
selects the newest unexpired profile for `io.ctmux.desktop.ctld`, discovers its
matching signing certificate in the login Keychain, and selects a private,
stable `ctld` endpoint for this worktree under the system temporary directory.
The signed daemon runs independently of `tauri dev`, so quitting or relaunching
the app retains its SSH connections and daemon-owned state. Other worktrees and
the ordinary per-user daemon remain separate.

During a native rebuild, the current app stays open. Only after compilation and
signed-helper preparation succeed does the launcher replace it with the new
build. A failed build or signing attempt leaves the current app running and
Tauri watching for the next edit. If the first build fails, the watcher stays
active until a build succeeds. Frontend hot reload continues to use Vite.
Quitting the launcher closes its app and build processes.

Before each native launch, the launcher uses Cargo's reported helper artifact
and stages a signed bundle when it changes. It starts ctld only if no daemon is
running. A rebuild leaves an existing daemon in place; use **About → Restart**
on the SSH ctld row to apply the staged build explicitly. The app can open About
even when the running daemon uses an older SSH protocol. Build or signing
failures prevent the new client from starting without stopping the existing
daemon. Signed bundles remain available for running helpers and pending restart
operations after the launcher exits. Private SSH masters still expire after
five idle minutes; surviving the app's exit does not disable that timeout.

The launcher needs no signing environment variables. Tauri arguments,
such as `--release`, can be passed through `pnpm tauri:dev:signed --release`.
Explicit `--no-watch` or `--exit-on-panic` still opts out of waiting after a
failed build, following Tauri's behavior.
Ordinary `pnpm tauri dev` remains unsigned and cannot store Touch ID-protected
credentials. Run the launcher regression tests with `pnpm test:dev`.

The release workflow derives the Team ID and signing identity from the profile
and imported certificate. It expects `APPLE_API_ISSUER` and `APPLE_API_KEY` as
non-secret repository variables. `APPLE_CERTIFICATE`,
`APPLE_CERTIFICATE_PASSWORD`, `APPLE_CTLD_PROVISIONING_PROFILE`, and
`APPLE_API_KEY_CONTENT` are repository secrets. The certificate and profile
must be for Developer ID distribution, and the profile must authorize exactly
the team-prefixed `io.ctmux.desktop.ctld` application identifier.
If any required signing value is absent, CI explicitly skips Apple signing
and notarization, builds the desktop packages, and labels the draft release's
macOS assets as unsigned. Those builds cannot store Touch ID-protected
credentials. A complete but invalid signing configuration fails the build.
Main and version-tag builds refresh the app version's draft release only
after every desktop target and remote bundle succeeds; releases are published
manually. Manual branch builds upload artifacts without changing the draft.

GUI-created shells receive an automatic `session-N` name. **Disconnect**
removes an open tab while leaving its shell running. For the active tab it also
detaches the live view; inactive tabs have no live attachment to detach.
**Terminate session** is deliberately destructive: after confirmation it terminates the
session for all clients. **Remove from workspace** forgets an entry and closes
its tab without terminating its shell. Closing the app itself only detaches its
active view. Normal window close waits for pending workspace saves; save failures
remain visible with a retry action.

The desktop normally has one native window and one WebView. A session and tab
are identified by both host and daemon session ID, so equal IDs from different
daemons remain distinct. Only the active tab holds an attachment.
Switching or closing tabs does not terminate their daemon-owned sessions.
The active tab and native window title show the last observed `path — command`
(or shell name while idle). Inactive tabs retain their last title snapshot in
this window until they are selected again, when they receive a fresh attachment.
If xterm is still starting, the latest selected tab remains in an attaching
state and is connected as soon as the renderer is ready. A failed attachment
leaves its tab selected and can be retried from the session list or command
palette.

Open the command palette with `Cmd-Shift-P` on macOS or `Ctrl-Shift-P` on
Windows/Linux. It exposes session creation, refresh, switching, disconnect and
close, plus terminal input, layout, reconnect, focus, and a destructive
`Restart ctmuxd` maintenance action. Restart has no default shortcut or permanent
button: selecting it opens a quick-input confirmation, with Cancel focused.
It first verifies the running daemon's separate local-control
endpoint; an older daemon that lacks it leaves the active tab attached and
reports that restart is unavailable. Once accepted, it terminates every local
ctmux session (including sessions opened by other apps) before both daemon
endpoints drain and a fresh daemon starts. Local workspace entries become
missing rather than being silently removed.
Remote tabs and their SSH attachments are unrelated and remain intact. It
cannot preserve daemon-owned PTYs, has no remote `ctl` equivalent, and is not a
version-mismatch escape hatch: protocol upgrades must remain compatible. Default
terminal shortcuts are:

- new shell: `Cmd/Ctrl-Shift-N`
- new tab in the current shell-reported directory: `Cmd-T` on macOS or
  `Ctrl-Shift-T` on Windows/Linux
- detach active tab: `Cmd-W` on macOS or `Ctrl-Shift-W` on Windows/Linux
- close active session after confirmation: `Cmd-E` on macOS or `Ctrl-Shift-E`
  on Windows/Linux
- next tab: `Cmd/Ctrl-Shift-]`
- previous tab: `Cmd/Ctrl-Shift-[`

The close shortcut opens a quick-input confirmation with **Cancel** focused.
Press the close shortcut again (`Cmd-E` on macOS, `Ctrl-Shift-E` on Windows/Linux)
or choose **Terminate session** to terminate the session named in the prompt.
Press `Esc` to cancel. Other commands remain blocked while confirmation is open.

**New Shell**, from the sidebar, command palette, or `Cmd/Ctrl-Shift-N`, uses
the same overlay: choose a host (Local is first/default), then enter an optional
working directory. Blank means that host's home directory. Back preserves the
directory draft, and Escape cancels before submission without contacting a
host. Once creation starts, dismissal is disabled until it finishes because
the backend may already have created a persistent shell. Errors remain inline
for correction/retry; a successful creation is saved and opened as before.
Post-creation save or attachment failures preserve the existing shell and use
workspace/session recovery rather than inviting duplicate creation.

Workspace, connection, shortcut, and task notifications appear as cards at the
bottom right. Use the down chevron to hide a card and keep it for review, or
the × to dismiss it. The bell in the bottom status bar opens the notification
center from either Sessions or Tasks; **Show Notifications** is also available
in the command palette. The center supports dismissing individual entries or
clearing all, and Escape hides it. Retry actions use the current workspace state.
Info and success cards hide automatically after eight seconds, paused while
hovered or focused; warnings and errors stay visible until hidden, dismissed,
or their attachment/session inspection confirms recovery. Recovered connection
errors stay in the notification center with a **Resolved** label.
The app shows up to three cards and retains the latest 100 notifications in
this window's memory. History resets when the window is reloaded or closed.
Background sessions and split panes report independently; reconnect actions target
the attachment that failed. Hiding or dismissing a failure survives automatic
retries, while a new explicit connection attempt can report it again. Session
cache/archive errors are reported separately, even when no session is selected.
**Reconnect** reuses the terminal's SSH connection when available. If that
connection has ended, it opens **Connect host** first and then retries the same
terminal. Canceling authentication cancels that retry; automatic background
retries never open an authentication dialog.

These are the defaults. **Configure Keyboard Shortcuts** in the palette opens
quick input: select a command and enter a combination such as
`Primary+Shift+Y` (`Primary` means Cmd on macOS and Ctrl elsewhere). Blank removes
the binding; `default` restores it. Commands without defaults can also be bound.
The close command's configured shortcut also confirms its own close dialog.
Dialog accept/cancel/back are scoped commands; accept and back have no default
shortcut. Ordinary Enter still submits a form or activates the focused button,
so Enter on a confirmation's initially focused Cancel button remains safe.

Overrides persist separately from workspace/session data in `keybindings.json`
under the native app configuration directory (the shortcut picker shows its
exact path). Example:

```json
{
  "schema_version": 1,
  "overrides": [
    {
      "command_id": "session.close",
      "keybinding": { "code": "KeyY", "primary": true, "shift": true }
    },
    { "command_id": "tab.new_shell_here", "keybinding": null }
  ]
}
```

Use **Reload Keyboard Shortcuts** after editing the file externally. Invalid,
conflicting, or unknown bindings leave the last valid keymap active and show an
error; the file is not overwritten automatically. Saves reject concurrent edits.
This version supports one single-keystroke combination per command, not chords
or arbitrary `when` expressions. Conflicts must be resolved by unbinding the
other command first. Unmodified typing keys cannot be assigned app-wide.

Sidebar, tab, toolbar, palette, dialog, and native-menu actions use the same
dispatcher, with explicit session/host targets and shared availability checks.
Native Command-modified accelerators and displayed labels derive from the
resolved keymap; unmodified dialog and Alt/function keys use the webview adapter.
Shortcuts are local to the focused app. Text editing, focus/list navigation,
and raw xterm/PTY input remain widget behavior. Standard native editing/window
commands (including Cmd-Q) and emergency reload after a renderer crash remain
platform/recovery operations, outside the configurable app command registry.

## Visual preview

The workbench uses a compact activity bar for Sessions, Tasks, and Ports, a
collapsible host/session tree, and shared tab and command styling. Host and
session row actions appear on hover or keyboard focus. The keyboard icon at the
bottom of the activity bar opens shortcut configuration. Closing a tab keeps its
session running; **Terminate session** is the separate destructive action.

For a browser preview with sample data, run `pnpm exec vite --host 127.0.0.1`
and open `http://127.0.0.1:1430/preview.html`. This development-only entry renders
the actual UI using in-memory Tauri mocks. It cannot execute terminal commands
or open SSH connections. See [preview details](dev/README.md).

## Verify

```sh
pnpm check
pnpm test
pnpm build
cargo test -p ctmux-app
```

An opt-in backend integration test covers remote create, list, attach, and
kill through the same command functions invoked by Tauri:

```sh
CTMUX_TEST_SSH_TARGET=ctmux-docker cargo test -p ctmux-app \
  commands::tests::creates_lists_attaches_and_kills_a_session_over_ssh \
  -- --ignored --exact
```

The SSH prompt bridge also has opt-in tests for the built helper binary and
the local test container at `127.0.0.1:2222`. The host-key test uses a temporary
known-hosts file; provide the fingerprint independently inspected in the
container, never one learned from an unverified connection:

```sh
cargo build -p ctmux-app
CTMUX_TEST_ASKPASS_PROGRAM=/absolute/path/to/target/debug/ctmux-app \
CTMUX_TEST_SSH_IDENTITY=/absolute/path/to/private-key \
CTMUX_TEST_SSH_FINGERPRINT=SHA256:verified-container-fingerprint \
cargo test -p ctmux-app ssh_auth::tests -- --ignored
```

The GUI never resizes an existing PTY merely because it was selected. **Resize
with window** explicitly acquires layout ownership and then keeps the PTY grid
matched to this window; turning it off releases layout ownership. Sessions
created by this GUI start in resize-with-window mode because this window
establishes their initial layout. Authoritative geometry changes update the
active session's size in the sidebar immediately; inactive rows refresh from
the daemon when the session list is refreshed.
