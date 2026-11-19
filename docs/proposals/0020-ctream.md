# Proposal 0020: Graphical session control with ctream

- Status: Proposed
- Created: 2026-11-19

## Summary

Develop `ctream` as a standalone Rust project for discovering X11 and Wayland
instances and forwarding control to a selected instance. Provide a CLI and a
native wgpu GUI. Once the standalone tool is stable, add lifecycle management
through `ctld`.

## Motivation

A host can have several graphical sessions. An operator needs to identify the
intended desktop and direct input to it from either interface. Keeping ctream
independently buildable and runnable allows backend changes to be tested on a
Linux desktop throughout development.

## Design

Place the project under `ctream/` with a shared Rust library. Both frontends
use the same discovery, selection, and input operations. The CLI lists targets
and accepts an explicit target for control. The GUI presents the same list and
uses wgpu for rendering. Keep capture, if needed, behind a separate interface
so CLI discovery can run without initializing a renderer.

The first milestone is read-only discovery of both X11 and Wayland from an SSH
login. An initial Linux host check found a reachable Xorg display with both
filesystem and abstract sockets, alongside a Wayland greeter in another user's
runtime directory. Report the two X11 listeners as one display. Label a
greeter separately when session metadata identifies it, and distinguish an
observed listener from a display that accepted a client connection.

Install `ctream-agent` on the remote Linux host with
`cargo install --locked --path ctream --bin ctream-agent --root ~/.local`.
From the ctream workspace, run `cargo run -p ctream-preview -- HOST` to
discover remote displays and select one in the viewer. Append `:DISPLAY` or
`wayland-N` to open a target directly.

The viewer opens a transport channel for each agent operation. The SSH
transport starts a standalone `ctream-agent` process per channel under the
remote account. Discovery returns structured display metadata. Capture streams
frames on a separate channel, and input uses its own duplex channel. Closing
the capture channel cancels its backend process. Closing the input channel
releases held inputs.

The agent uses FFmpeg with x11grab for X11 capture. X11 capture needs Python 3,
libX11, and access to the selected display. X11 input also requires libXtst
and XTEST. The local wgpu viewer uses the same rectangle for drawing the
desktop and mapping clicks to display pixels.

For a user-owned Sway session, select `wayland-N` in the same command. Capture
uses `grim`. To enable input, install the helper on the Linux host with
`cargo install --locked --path ctream/wayland-input --root ~/.local` from the
repository root. Building it requires libxkbcommon. The helper connects to
the selected Wayland socket and uses the compositor's virtual pointer and
virtual keyboard protocols on a single seat. It supplies a US keyboard map.
Absolute pointer events carry the dimensions of the displayed capture frame.
When input initialization fails, the viewer reports the error and continues
in view-only mode.

A subsequent experiment should cover a Wayland session with Xwayland.
Record which endpoints are reachable, which accept
input, and whether the backend can associate two endpoints with one desktop.
On Wayland, evaluate the RemoteDesktop portal from within the target user's
graphical session. Test consent, cancellation, and session closure.

## Invariants

- Discovery alone never sends input.
- Forward input only to the selected instance. If it disappears, stop
  forwarding and report the loss to the caller.
- Both interfaces run independently of ctld during development.

## References

- [AUV](https://github.com/moeru-ai/auv) provides a reference for exposing OS
  input operations to callers.
- [libobs frontend API](https://docs.obsproject.com/frontends) describes the
  capture engine and preview lifecycle if ctream needs a live desktop view.

## Open questions

- Does the first release forward input locally, over SSH, or both?
- Which sessions should be discoverable from an SSH login with no graphical
  session environment?

## Protocol impact

The standalone `ctream_agent` contract starts at 1.0.1 (internal build 1).
It provides discovery as JSON, capture as RGB frames, and input as a line-based
stream. This is the first version of the contract. Existing ctl contracts
retain their current versions.
