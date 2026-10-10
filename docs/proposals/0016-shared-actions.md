# Proposal 0016: Shared typed actions across ctl clients

- Status: Implemented
- Created: 2026-10-10

## Summary

Define a shared, typed application API for ctl subcommands, desktop commands,
and ctmux `:` mode. Keep existing command syntax and published protocols.
Clients translate their commands into domain actions with explicit targets;
the shared Rust client API executes those actions against the appropriate owner.

The shared Rust client API is library code running inside each client process.
It introduces no daemon, background process, network listener, or new IPC hop.

This extends [Proposal 0002](0002-ctl.md) and the client controls in
[Proposal 0014](0014-shared-panes-tui.md).

## Motivation

Today actions are described in several places: Clap subcommands, daemon wire
messages, Tauri invokes, desktop command IDs, and the TUI prompt parser. These
are different layers, not four equivalent APIs. Client code can duplicate
validation, target resolution, connection policy, and error handling while
similar names conceal different effects.

For example, desktop `session.disconnect` detaches the active tab, whereas
`ctl host disconnect` closes SSH masters. Pane selection is local navigation;
pane sizing changes shared daemon state. A shared API should expose these
distinctions rather than move every operation into ctld.

## Design

Use per-domain request, result, and error types in the existing Rust client
libraries. Separate command parsing and presentation from action execution.
The desktop alone uses a Tauri adapter to translate TypeScript calls into
the shared Rust client API. CLI and TUI clients call that API directly. Tauri and
desktop DTOs are not dependencies of the shared API, ctl executable, or daemons.
Desktop menu IDs and tmux-style aliases remain presentation identifiers.

Ownership remains explicit:

- Host and definition catalogs belong to shared client storage libraries.
- SSH masters, authentication, forwards, and VPN runtime belong to ctld and
  its existing helper boundaries.
- Sessions, terminals, shared layout, and attachment leases belong to ctmuxd.
- Registered tasks and their runtime belong to ctl-taskd.
- Focus, tabs, palettes, copy mode, and client preferences belong to each UI.

Define semantic action identifiers such as `connection.ensure`,
`attachment.detach`, and `session.terminate`. These identifiers describe typed
functions; they do not introduce a string dispatcher or a new wire envelope.
Names and aliases resolve to explicit target IDs before mutations run.

Preserve passive status queries and quiet background connection behavior.
Explicit connect actions may request authentication; passive discovery must
not acquire authentication simply to determine which actions are available.
Connection reuse and attachment reconnect remain separate operations.

Start with session creation and termination, attachment detach, connection
ensure/status/disconnect, and explicit input/layout lease requests. Adopt
other domains incrementally after these establish the client API pattern.
The initial implementation shares connection and session actions across CLI,
desktop, and TUI, and retains the existing attachment controller as the shared
streaming API. Its queued control outcomes remain events, as documented in the
detailed specification. Other domains in the inventory remain future work.

## Invariants

1. Existing ctl and `:` command syntax remains supported.
2. Domain actions do not depend on Clap, Tauri, terminal rendering, or UI state.
3. Each mutation identifies its owner and object; an active selection cannot
   silently redirect an action after it has started.
4. Detach, terminate, disconnect, and forget are distinct effects.
5. Background work never silently escalates quiet authentication to a prompt.
6. Protocol negotiation and daemon authorization remain authoritative; hiding
   or disabling a client command is not enforcement.
7. Stream ownership, backpressure, cancellation, and reconnect remain explicit.
8. Existing log ID meanings and secret exclusions remain unchanged.
9. Client frameworks depend on shared libraries; shared libraries never depend
   on a client framework or its bridge.

## Protocol impact

Protocol changes: none.

This proposal defines a client API and adapter refactoring, without new daemon
messages, negotiation, storage schemas, or version changes. Initial actions
must use existing negotiated operations. Any later action requiring a wire
change must specify the affected named contract's previous/new versions and
internal builds, compatibility, and negotiation requirement in its own change.

## Out of scope

- A generic `ctl action run` command or `action.list` daemon endpoint.
- Turning ctld into a universal session, task, or UI command server.
- Removing existing aliases or renaming saved desktop keybindings.
- Automatic retries of mutations after an uncertain outcome.
- Task workflows and scheduling from deferred Proposal 0007.

## Unresolved questions

None for the initial API extraction. Broader DTO generation and additional
domain migrations remain future work.

## Detailed specifications

- [Shared action API design and surface inventory](../action-api.md)
- [Control routing](../ctl-protocol.md)
- [ctmux protocol](../ctmux-protocol.md)
- [Protocol versioning](../protocol-versioning.md)
- [Connection state](../connection-state.md)
- [Logging and audit identifiers](../logging-audit.md)
