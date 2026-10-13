# Proposal 0013: Paged terminal history without blocking live output

- Status: Implemented
- Created: 2026-10-09
- Implementation PRs: [#67](https://github.com/tokn-ai/ctl/pull/67),
  [#69](https://github.com/tokn-ai/ctl/pull/69),
  [#91](https://github.com/tokn-ai/ctl/pull/91)

## Summary

Restore the live screen and a small recent history tail immediately, then fetch
one pinned bounded daemon-history snapshot in the background. Client-local
persistence and archives retain only output that the client actually observed.
This extends Proposal 0001; the daemon and its PTYs remain memory-backed.

## Motivation

Large remote scrollback must not delay live output or reconnect readiness.
Resizes and clears can replace history while transfer is in flight, so clients
need coherent snapshot identity rather than text-overlap guesses.

## Design

At a checkpoint boundary, ctmuxd freezes physical primary-buffer scrollback
with the live screen and recent logical tail. Normalization joins soft wraps
and excludes alternate-screen history. A manifest binds snapshot ID, raw
sequence, generation, revision, byte size, and hash. Resize and saved-history
clear create new replacements, even when raw byte sequence is unchanged.

Clients apply and acknowledge the live checkpoint independently of paged
history transfer. Bounded pages may split encoded text and must be reconstructed
and verified as one snapshot. Background work rebuilds history off-view, replays
newer raw output, and publishes only at a current presentation boundary. It
never rewinds the live renderer or its safe resume position. Expired snapshots
and lost replay baselines require replacement or explicit incomplete history.

Renderer-applied sequence remains the reconnect cursor; receiving bytes or
queuing a write does not advance it. Bounded delivery credit keeps output queues
controlled while heartbeat and control traffic remain available.

Desktop persistence runs in order behind a bounded queue so slow storage cannot
hold up live rendering. Legacy local history remains recoverable when the new
bounded projection replaces it. TUI copy mode freezes each pane's screen/history
independently of new output and reconnects. Ended or confirmed-missing sessions
can be archived locally; transport failure alone is not an exit. TUI archives
expire after seven days, and desktop archives use their own store.

By default, TUI selection and copying preserve the frozen view. Releasing a
mouse drag selects text without copying; `y`, Enter, Alt+W, or Ctrl+W explicitly
copies without leaving history or archives. Retained mouse selections and
copied keyboard selections keep fixed endpoints while navigating. Click or
Ctrl+G clears the selection in place; Esc or `q` returns from pane history to
live output.

Client-local `~/.tokn/ctl/ctmux.json` persists selection preferences across TUI
launchers, including remote shells and archives. `selection.mouse_release`
chooses `select` (default) or `copy`; `selection.after_copy` chooses
`keep-selection` (default), `clear-selection`, or `exit-copy-mode` for both
explicit and automatic copies. Clearing stays scrolled, while exiting returns
pane history to live output or closes an archive copy view. Clicks without a
selection never copy or exit. `CTMUX_CONFIG_FILE` can override the path; invalid
configuration fails before new session creation. These startup settings do not
change the desktop native viewer, plain/raw presenters, detached commands, or
outer-terminal Cmd+C handling.

## Invariants

1. Raw PTY bytes remain canonical output; normalized history is a bounded projection.
2. Screen restoration and history synchronization have independent progress.
3. A checkpoint/history pair shares an exact raw boundary and snapshot identity.
4. Stale history work cannot rewind live rendering or resume progress.
5. Missing, truncated, or incomplete history remains visible to the user.
6. Client storage cannot recover output the client never received.

## Protocol impact

Paged projection advances `ctmux` from 1.0.13/build 13 to 1.1.14/build 14,
retaining 1.0.13 with complete inline history. The new selected contract adds
history manifests, bounded page requests/responses, expiration, and explicit
checkpoint recovery. Local caches, archive repairs, and resize rendering fixes
add no named protocol changes of their own. Later pane contracts retain this
history boundary.

The next release cycle adds styled physical history in `ctmux` `1.2.21`
(build 21), following the frozen `1.1.20` (build 20) contract. Optional typed
foreground/background colors and text attributes accompany literal history
text. Earlier contracts still receive their original plain rows, while updated
clients preserve styles through paged transfer, local projection, and copy-mode
rendering. Existing text-only archives cannot recover discarded attributes.
The TUI footer shows the active pane's retained physical scrollback count and
the visible row range of a frozen copy snapshot; the latter includes the screen.
Selection and copy settings are client-local: `ctmux` remains `1.2.21` (build 21).

## Out of scope

Unlimited daemon history, durable PTY/process recovery, retroactively coloring
text-only archives, and treating archive completeness as proof of complete
remote output.

## Unresolved questions

None for the implemented bounded projection.

## Detailed specifications

- [Paged history protocol](../ctmux-protocol.md#paged-history-projection-published-contract-1114)
- [Checkpoint and history architecture](../architecture.md#checkpoints)
- [Desktop persistence and archives](../ctmux-workspace.md)
- [TUI history and copy mode](../../apps/tui/README.md)
