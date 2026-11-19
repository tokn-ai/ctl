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

For the first live preview, run `cargo run -p ctream-preview -- HOST :DISPLAY`
from the ctream workspace. The viewer starts FFmpeg with x11grab through SSH
and displays the frames in a local wgpu window. While focused, the viewer
forwards keyboard and pointer events over a second SSH channel. The host needs
Python 3, libX11, libXtst, and access to the selected X11 display with XTEST.
The viewer fits the desktop to the window and uses the same rectangle to
map clicks to X11 pixels. Closing the window releases held inputs and ends
both channels.

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

Protocol changes: none.
