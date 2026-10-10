# Shared action API design

This documents the initial shared Rust client API, not a new public wire
contract. It supports [Proposal 0016](proposals/0016-shared-actions.md). The inventory below
describes the current source on 2026-10-10. Semantic action IDs organize the
design; the initial implemented APIs below use typed methods. Existing command
syntax stays intact.

## Layers

```text
ctl CLI (Rust) -------------------------+
ctmux : mode (Rust) --------------------+--> shared Rust client API
                                       |       |-- client storage
desktop frontend (TypeScript)          |       +-- existing transports
  --> desktop-only Tauri adapter ------+             --> ctld / ctmuxd / ctl-taskd

UI-only actions stay in their respective clients.
```

The shared Rust client API is ordinary library code running in each client
process. It adds no daemon or IPC hop. CLI, TUI, and desktop Rust code reuse the
implementation; they do not communicate with one another to invoke it.

Tauri is an implementation detail of the desktop app's TypeScript-to-Rust
boundary. The ctl executable, TUI, shared libraries, and daemons have no
dependency on Tauri and never call the desktop bridge. Dependencies point from
each client adapter into the shared libraries. Changing the desktop framework
must not require changing the domain API or daemon protocols.

A command is an entry point with parsing, selection, confirmation, and output
formatting. An action is a typed operation with a defined effect and owner.
A protocol request is the negotiated wire operation used to perform it. One
command can compose several actions: a new shell may ensure a connection,
create a session, open an attachment, then create a local tab.

Reuse the existing `ctl-client`, `ctmux-client`, and task client libraries.
Introduce shared client API functions where execution currently lives in client
adapters; do not add a universal crate merely to collect command names.
Protocol enums remain in their existing domain crates. Shared client requests
may reuse suitable protocol types, but must not expose raw messages as the
application API or bypass negotiation.

## Ownership and representative action inventory

These are mappings, not promises that every surface supports every action.
“No direct command” means there is no corresponding entry in the inspected
command registry/parser; related UI flows may still use the operation.

| Proposed action | Current ctl entry | Current desktop entry | Current `:` entry | Owner |
| --- | --- | --- | --- | --- |
| `host.list/create/update/remove` | `host list/create/update/remove` | Host catalog/settings flows | No direct command | Client catalog |
| `host.method.add/update/remove/prefer` | `host method …` | Connection method settings | No direct command | Client catalog |
| `connection.status` | `host status` | Connection status queries | No direct command | ctld; passive |
| `connection.ensure` | `host connect`; transport setup | `host.connect`; explicit connection flows | Transport setup | ctld |
| `connection.disconnect` | `host disconnect` | Host disconnect flow | No direct command | ctld |
| `port_forward.configure/list` | `port …` | `host.port_forwarding` opens management | No direct command | ctld |
| `vpn.start/status/stop` | `vpn …` | VPN management flows | No direct command | Selected VPN runtime owner |
| `session.list` | `ctmux list` | `session.refresh` | `list-sessions` opens session selection | ctmuxd |
| `session.create` | `ctmux new`; `shell` composition | `session.new_shell`; `tab.new_shell_here` | `new-session` | ctmuxd |
| `session.terminate` | `ctmux kill` | `session.close` | No direct command | ctmuxd |
| `archive.list/read` | `ctmux archives/archive` | Retained session views | Archive picker via TUI controls | Client archive store; passive |
| `terminal.split` | `ctmux split` | Terminal controls | `split-window` | ctmuxd |
| `terminal.promote` | `ctmux promote` | No direct command | `break-pane` | ctmuxd |
| `terminal.terminate` | `ctmux kill-terminal` | Terminal controls | `kill-pane` | ctmuxd |
| `layout.swap/resize/zoom` | No direct subcommand | Pane controls | `swap-pane`; `resize-pane` including zoom | ctmuxd |
| `attachment.open` | `ctmux attach`; `shell` composition | Session attachment flow | Session switching composes this | ctmuxd + client transport |
| `attachment.detach` | Interactive detach | `session.disconnect` | `detach-client` | ctmuxd + client cleanup |
| `attachment.reconnect` | Interactive recovery | `terminal.reconnect` | No direct command | Client transport + ctmuxd |
| `attachment.lease.request/release` | Attachment options request leases | `terminal.toggle_input`; `terminal.toggle_resize_control` | `take-input/release-input`; `take-resize/release-resize` | ctmuxd |
| `task.create/list/show/start/stop/restart/remove` | `task …` | Task views/flows; supported subset | No direct command | ctl-taskd |
| `task.logs/attach` | `task logs/attach` | Task views/flows | No direct command | ctl-taskd + stream adapter |
| `task_definition.save/list/remove` | `task save/definitions …` | Task definition editor | No direct command | Client catalog |
| `component.inspect` | `components status` | Component versions/About | No direct command | Existing maintenance services; passive |
| `component.update` | `components update` | Component update flow | No direct command | Existing maintenance services |
| `component.restart` | `components restart`; `taskd restart` | Component controls; `daemon.restart`; `ctl-taskd.restart` | No direct command | Existing maintenance services |
| `component_bundle.list/import/select` | `components list/sync/select` | Bundle inventory and selection | No direct command | Client bundle store |
| `history.logs/audit.read` | `logs`; `audit` | Credential/audit UI | No direct command | Local history reader |
| `ui.session.select` | Interactive selection | `session.select` | `switch-client` | Client UI |
| `ui.pane.focus` | Interactive focus | `terminal.focus` and pane focus controls | `select-pane`; `last-pane` | Client UI |
| `ui.history.open/paste` | Interactive history/paste | Terminal history/clipboard controls | `copy-mode`; `paste-buffer` | Client UI; paste uses input lease |
| `ui.palette/tab/preferences` | No direct command | Palette, tab navigation, keybindings, workspace | `list-keys`; `display-panes` and other TUI controls | Client UI |

SSH/scp compatibility, exec, setup, credential management, and VPN enrollment
remain separate existing flows. Inventory their typed APIs when migrating those
domains; do not reinterpret arbitrary OpenSSH argv as generic action arguments.

## Implemented coverage

The table above maps commands to possible actions; it does not mean every row
uses one shared action implementation. This table records the current boundary
so a command's implementation status can be checked without inferring it from
the proposal status.

| Entry points | Shared execution today | Remaining adapter work |
| --- | --- | --- |
| `host status/connect/disconnect`; desktop connection flows | `ctl_client::connection::ConnectionClient` | Target selection, prompts, and presentation |
| `ctmux list/new/kill/attach`; desktop and TUI session flows | `ctmux_client::session::SessionClient` | Target selection, UI session lifecycle, and presentation |
| Attachment detach and input/layout leases | `ctmux_client::AttachmentControl` | Controller ownership and UI lease events |
| Local `components status`; desktop local About rows | `ctl_client::component_status::observe_local` passively inspects the chosen owner endpoint | Available-helper discovery, compatibility/status presentation, and desktop SSH/VPN owner selection |
| Remote `components status` and remote `ctmuxd` restart | `ctl_client::maintenance` inspection and prepared restart | CLI target selection and confirmation; desktop remote observation and confirmation |
| `components list/sync/select`; desktop bundle inventory and selection | `ctl_client::components` inventory, import, and selection | Source discovery, progress, and output |
| `components update`; desktop component update | `ctl_client::component_update` prepare and install | Target orchestration, prompts, and progress |
| Local `components restart`; desktop local component restart | Owner lifecycle preflight/restart APIs, called by each adapter | Shared typed local restart action; preserve pinned owner and confirmation semantics |
| Host catalog, port forwarding, VPN, tasks, terminal/layout, archives, history, and other entries above | Existing domain-specific APIs vary | Inventory and migrate only where execution remains duplicated |

`ctmux :` mode has no component or task commands. A shared client action does
not add a command to another surface. The component status extraction does not
prepare a helper, start a service, or change how “Available build” is selected.

## Names and effects

Use `domain.verb` semantic IDs with snake_case words. IDs describe actions for
documentation, tests, and metadata; typed methods perform execution.
Existing desktop IDs remain stable for user keybindings and map to these actions.

- **Detach** releases this client's attachment and closes its transport; the
  session continues. Desktop `session.disconnect` maps here.
- **Terminate** ends a session or terminal owned by ctmuxd. Desktop
  `session.close` maps to session termination after its existing confirmation.
- **Disconnect** closes selected SSH masters and can affect multiple channels.
  It is not part of ordinary tab cleanup.
- **Forget/remove** changes a saved reference, definition, or credential in its
  owning store. Removing a saved host does not implicitly disconnect it.
- **Reconnect** repairs an attachment transport, attempting supported resume;
  it does not mean force-replacing a healthy SSH master.
- **Focus/select** changes this client's selection. Shared layout mutations are
  separate actions requiring the existing layout lease.

Avoid domain-level `toggle` methods: translate UI toggles into explicit lease
request/release or desired state. A request remains subject to the owner's
current lease state; observing an enabled button does not grant ownership.

## Implemented APIs

These are concrete APIs in existing libraries, with no new daemon or crate.
The signatures below omit implementation bodies; the linked Rust files are
source of truth.

### Connection API in ctl-client

[ConnectionClient](../ctl/client/src/connection.rs) is available on Unix, matching
existing broker support. Frontends provide prompts; shared code never reads a
terminal or receives Tauri objects.

```rust
pub enum InteractionPolicy { Quiet, Interactive }
pub struct ConnectionStatus {
  pub connected: bool,
  pub manually_disconnected: bool,
}
pub struct ConnectionReady {
  pub control_path: PathBuf,
}

// ConnectionClient methods:
async fn status(&self, target: SshTarget) -> Result<ConnectionStatus, Error>;
async fn disconnect(&self, target: SshTarget) -> Result<(), Error>;
async fn ensure<F, P>(
  &self, target: SshTarget, interaction: InteractionPolicy, ask: F,
) -> Result<ConnectionReady, Error>
where
  F: FnMut(PromptKind, String, Option<String>) -> P,
  P: Future<Output = Result<Option<Zeroizing<String>>, Error>>;
```

`status` observes without starting ctld or establishing SSH. `ensure` reuses a
healthy master; quiet mode never calls the prompt adapter. `disconnect` closes
one selected master and may affect its channels. Host commands that select all
saved methods compose those calls, rather than inventing a bulk wire operation.

The client serializes exchanges and reuses its negotiated broker socket when
supported. Cancellation discards an in-flight socket. Older contracts retain
one-shot behavior. Requests and mutations are never automatically replayed
when a response is lost.

### Session API in ctmux-client

[SessionClient](../ctmux/client/src/session.rs) consumes an already selected
transport stream and the caller's client identity. Target resolution and
connection policy belong to the connector before this API runs. One-shot
ctmux exchanges consume the client; attachment opening transfers the live
stream back to the presenter/controller. This preserves the existing protocol.

```rust
pub struct SessionId(pub String);
pub struct CreateSessionRequest {
  pub name: Option<String>,
  pub cwd: Option<String>,
  pub command: Vec<String>,
  pub terminal_size: TerminalSize,
}

// SessionClient<S> methods:
fn new(stream: S, identity: ClientIdentity) -> Self;
async fn list(self) -> Result<Vec<SessionInfo>, ClientError>;
async fn create(self, request: CreateSessionRequest) -> Result<SessionInfo, ClientError>;
async fn terminate(self, session_id: SessionId) -> Result<(), ClientError>;
async fn attach(self, request: AttachRequest) -> Result<(S, AttachedSession), ClientError>;
```

`create` preserves argv boundaries and explicit geometry. It does not attach,
start a presenter, or open a tab. CLI/desktop/TUI retain their existing cwd
selection policy before calling it. `terminate` ends persistent work and is
never used for tab cleanup. Adapters resolve selectors within their target;
legacy CLI name selectors remain accepted by the daemon. Session IDs are not
newly generated by the client API.

`attach` reuses the existing `AttachRequest`, including replay position, input
and layout lease requests, and presentation window settings. Reconnect uses
existing attachment resume paths rather than forcing a new session or master.

### Attachment API in ctmux-client

The existing [AttachmentControl](../ctmux/client/src/lib.rs) and controller are
the shared attachment API. Retain their event stream and lifecycle model rather
than wrapping them in another runtime. Frontends own the controller task and
close/wait or cancel it through their existing cleanup paths.

```rust
// AttachmentControl methods:
async fn request_lease(&self, kind: LeaseKind) -> Result<(), AttachmentCommandError>;
async fn release_lease(&self, kind: LeaseKind) -> Result<(), AttachmentCommandError>;
async fn detach(&self) -> Result<(), AttachmentCommandError>;
```

`LeaseKind` is `Input` or `Layout`. Success means the command entered the local
queue; authoritative lease outcomes arrive through `AttachmentEvent::LeaseStatus`.
The API does not return a cached status as if it acknowledged that request.
The legacy `acquire_lease` spelling remains as a compatibility alias.

`detach` requests graceful closure without killing the session or disconnecting
SSH. It retains the existing cloneable control endpoint; unlike the earlier
proposal sketch, invoking it does not consume every clone or complete frontend
cleanup synchronously. The owning frontend closes its controller and releases
resources. Existing output, input, checkpoint, history, and layout interfaces
remain intact.

### Local component observation in ctl-client

`ctl_client::component_status::observe_local(owner, socket)` takes a typed
`LocalOwner` (`Ctld`, `Ctmuxd`, or `CtlTaskd`) and an explicit owner socket. It
returns a `LocalObservation` with `Absent`, `Legacy`, or `Running` state,
reported build and protocols, legacy protocols, protocol mismatch, and restart
support. The CLI uses it for local `components status`; desktop About uses it
for its selected SSH/VPN ctld owners and local ctmuxd/taskd. Observation only
contacts an existing owner. Each frontend still discovers an available helper
and computes its own display and restart policy.

### Adapter boundaries

Names, saved connection methods, routes, and UI selections resolve before the
exchange. A later tab switch cannot change the stream's target. Reuse existing
local/SSH target types, remote identity verification, platform selection, and
fixed service routing. Do not accept arbitrary daemon sockets from a peer.

Only the desktop adapter uses DTOs to cross its TypeScript-to-Rust boundary,
with snake_case serialized fields, and resolves app-local attachment handles in
Rust. DTOs do not define the shared API. The shared Rust client API receives no
Tauri channel or desktop tab key. Broader DTO generation is future work; current
adapters preserve the existing desktop contract.

## Interaction, availability, and failure

Connection establishment accepts explicit `interactive` or `quiet` policy.
Status uses a passive path. A quiet action that cannot authenticate returns
`authentication_required`; only a later explicit user action may choose the
interactive path. Opening a palette or restoring a workspace does not connect
to every saved host. Client API implementations reuse the shared broker
client/connector rather than constructing one for each nested action.

Each domain returns typed results and errors. Adapters format them into CLI
text/JSON, desktop notifications, or TUI notices. Preserve distinctions such as
not found, invalid target, unsupported negotiated operation, authentication
required, lease unavailable, cancelled, and transport failure. Map existing
protocol errors without losing domain details; these labels do not mandate a
new global wire error enum.

Client availability can be `available`, `unavailable` with a reason, or
`unknown` until a connection negotiates capabilities. Derive it from the same
client API rules and negotiated contract, rather than a second hardcoded version
table in each UI. The owner still validates every operation because connection,
session, and lease state can change after the client checks availability.

Confirmation remains in the initiating surface. Typed requests carry the
explicit object chosen before confirmation; a tab switch cannot change it.
Existing daemon authorization, credential prompts, read-only attachment rules,
and maintenance protections remain enforced in their current owners.

## Streams, retries, and observability

Attachments and task log subscriptions return owned handles/streams with
explicit close/cancel behavior. They preserve existing checkpointing,
backpressure, lease events, and reconnect grace. Terminal input and output do
not pass through a generic JSON action result. A request's cancellation does
not imply terminating the persistent session or task.

Do not automatically retry create, terminate, or other mutations after losing
the reply. Their outcome can be uncertain even when the client reports a
transport error. Reconcile using supported domain queries where possible and
report uncertainty when it cannot be resolved; do not claim idempotency that
the existing protocol does not provide.

Keep [existing log identifiers](logging-audit.md) unchanged:

- Semantic action IDs describe API operations.
- `operation_id` identifies a stable recording call site in source.
- `attempt_id` groups an operation invocation's start and outcome records.
- `correlation_id` identifies the broker's observation of an SSH master lifetime.
- `subject_id` is the existing target/credential hash, not a session or action ID.

Do not create another universal invocation UUID for the initial API or replace
these IDs with the semantic action name. Do not log raw requests, command argv,
terminal contents, credentials, or reconnect tokens.

## Adoption and verification

1. Agree on this inventory, effects, and the first client API contracts.
2. The initial implementation extracts shared session create/terminate actions
   into the existing domain client library and reuses its attachment controls. CLI, desktop, and TUI adapters call
   them where each action is supported, preserving current aliases and
   presentation behavior.
3. The initial implementation consolidates connection status/ensure/disconnect
   in ctl-client, preserving passive/quiet policy and broker reuse. Disconnect
   may open a temporary socket to interrupt authentication on a busy shared
   socket. Attachment cleanup remains separate from SSH master teardown.
4. Apply the pattern to the remaining domains as they are refined. Keep UI-only
   commands in their client registries and defer task workflows from Proposal 0007.

Verify meaningful cross-surface behavior: the same explicit target and action
produce the same domain effect; detach leaves a session running; terminate
targets only the selected session; local navigation performs no remote mutation;
quiet/passive paths show no prompt; unsupported negotiated operations fail
before sending a message; cancellation closes temporary streams without leaking
handles or killing persistent work. Preserve tests of legacy parsing and desktop
keybinding IDs. No protocol bump is required for extraction alone.

## Current source entry points

- [ctl command parser](../ctl/cli/src/main.rs) and [host commands](../ctl/cli/src/host.rs)
- [ctld messages](../ctl/ipc/src/lib.rs)
- [Shared host storage and resolution](../ctl/client/src/hosts/mod.rs)
- [ctmux command parser](../ctmux/cli/src/lib.rs) and [client API](../ctmux/client/src/lib.rs)
- [Task command parser](../task/cli/src/lib.rs)
- [Desktop command IDs](../apps/desktop/src/features/commands/commandIds.ts),
  [command adapters](../apps/desktop/src/features/commands/terminalCommands.ts),
  and [Tauri bridge registration](../apps/desktop/src-tauri/src/lib.rs)
- [TUI prompt parser](../apps/tui/src/prompt.rs) and [actions](../apps/tui/src/actions.rs)
