# Proposal 0014: Shared pane geometry and terminal client controls

- Status: Implemented
- Created: 2026-10-09
- Implementation PRs: [#75](https://github.com/tokn-ai/ctl/pull/75),
  [#78](https://github.com/tokn-ai/ctl/pull/78),
  [#79](https://github.com/tokn-ai/ctl/pull/79),
  [#83](https://github.com/tokn-ai/ctl/pull/83),
  [#84](https://github.com/tokn-ai/ctl/pull/84),
  [#87](https://github.com/tokn-ai/ctl/pull/87),
  [#92](https://github.com/tokn-ai/ctl/pull/92),
  [#94](https://github.com/tokn-ai/ctl/pull/94),
  [#96](https://github.com/tokn-ai/ctl/pull/96),
  [#97](https://github.com/tokn-ai/ctl/pull/97)

## Summary

The desktop and TUI display the same daemon-owned split views. Shared zoom,
weighted sizing, divider dragging, and pane moves preserve terminal identity
and explicit lease ownership. The TUI adds a flat session navigation model,
pane-local copy mode, and a local command prompt. This extends Proposal 0001.

## Motivation

A second client must see the same pane arrangement without imposing its window
size. Terminal users need familiar controls while application keyboard/mouse
modes and reconnects preserve the existing terminal contract.

## Design

Ctmuxd owns session roots, views, terminals, split trees, weights, and cell
rectangles. Input leases are per terminal; resize ownership is per view. A
nonowner clips or scrolls the shared canvas rather than resizing member PTYs.
Zoom expands one pane while hidden panes keep running; unzoom restores the
split proportions. Keyboard sizing and exact divider dragging use the resize
lease, minimum subtree sizes, and authoritative acknowledgements.

Attached pane swap and break requests carry source view identity, revision, and
terminal membership guards. Swapping preserves split slots and weights; breaking
moves a running terminal to a new flat session. Its process, history, identity,
and input attachment survive. Source resize attachments are released for the
moved terminal. Task-managed roots reject splits and transfers.

The default `ctmux` launcher creates/attaches through the TUI; scripts explicitly
request detached creation. Raw attachment remains available. `ctl shell` shares
this TUI over local or SSH transport, while `ctl ctmux` retains its existing
transport presenter. Sessions are the navigation layer, with no nested windows.
Prefix bindings preserve modifiers and allow bounded repetition of pane arrows.

Copy selections are pane-local frozen snapshots. Application mouse input uses
pane-relative coordinates and requires input ownership; local selection/history
and read-only browsing remain client concerns. The command prompt occupies the
status row and accepts one literal command from a bounded supported set. It has
no shell fallback or command expansion, and does not send editing input to a PTY.
Output and recovery continue while prompts or pane operations await completion.

Checkpoint restoration preserves application mouse, paste, and keyboard mode.
Under the modified-key contract, TUI encoding follows each pane's requested
xterm mode. Host Kitty negotiation is local and separate from daemon IPC.

## Invariants

1. Every PTY remains in ctmuxd; UI actions never spawn a second terminal owner.
2. Shared geometry requires the view resize lease; input ownership is independent.
3. Moving a live pane does not imply process exit or recreate its terminal.
4. Stale view edits and unsupported operations fail before changing geometry.
5. Local prompts and copy selections do not become terminal input.
6. Disconnect/reconnect preserves authoritative geometry and application modes.

## Protocol impact

`ctmux` progresses from 1.1.14/build 14 to 1.1.15/build 15 (shared zoom),
1.1.16/build 16 (weights and keyboard sizing), 1.1.17/build 17 (exact divider
requests and lease notifications), 1.1.18/build 18 (checkpoint restoration of
`modifyOtherKeys`), and 1.1.19/build 19 (attached `swap_pane`, `break_pane`, and
correlated move results). Every step retains 1.0.13 and all earlier published
contracts in this cycle. Older clients retain their layout behavior; newer
clients reject unavailable operations locally. Modified-key restoration adds
no JSON fields and retains checkpoint format version 1. Prefix, copy, prompt,
and local host keyboard controls add no independent named contract.

## Out of scope

A tmux-compatible scripting language, nested windows/tabbed daemon layouts,
automatic lease takeover, moving task-owned terminals, and daemon-restart survival.

## Unresolved questions

None for the implemented pane and TUI boundary.

## Detailed specifications

- [TUI commands, mouse behavior, and rendering](../../apps/tui/README.md)
- [Session/view/terminal and sizing protocol](../ctmux-protocol.md)
- [Contract evolution and pane move messages](../protocol-versioning.md)
- [Desktop shared workspace](../ctmux-workspace.md)
