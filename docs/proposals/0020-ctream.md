# Proposal 0020: Graphical session control with ctream

- Status: Proposed
- Created: 2026-11-19

## Summary

`ctream` discovers X11 and Wayland instances and forwards control to a selected
instance through a CLI and a native wgpu viewer. Its CLI and agent (`ctream`),
viewer (`ctream-preview`), and Linux helpers (`ctream-wayland-input`) are members
of the root Cargo workspace and share its lockfile. They run independently of
`ctld`. Lifecycle management through `ctld` is planned once the tool is stable.

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

From the repository root on the remote Linux host, install `ctream-agent` with
`cargo install --locked --path ctream --bin ctream-agent --root ~/.local`.
From the repository root on the viewer machine, run
`cargo run -p ctream-preview -- HOST` to discover remote displays and select
one in the viewer. Append `:DISPLAY` or
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

### Window streaming

Install the window helper on the remote host with
`cargo install --locked --path ctream/wayland-input --bin ctream-wayland-window --root ~/.local`.
Open the display picker and choose **Windows…** beside an X11 or Wayland
display, or launch the viewer with `HOST DISPLAY --windows`. The separate
window helper is only required for the Wayland backend.

From the CLI, run `ctream windows DISPLAY` on the display host, or
`ctream --host SSH_HOST windows DISPLAY` to query through the remote agent.
The command lists window IDs, application IDs, and titles. Add `--json` for
structured output.

After selecting a display, the viewer can request its top-level windows from
the remote agent and open a capture stream for one of them. Expose titles and
application identifiers as labels. Bind the capture to an opaque target
identifier scoped to its display. If the window closes or its identity can no
longer be verified, end the stream and require a fresh selection.

Start with the user-owned nested Sway session. Discover windows through
`ext_foreign_toplevel_list_v1` and capture the selected toplevel through
`ext_foreign_toplevel_image_capture_source_manager_v1` and
`ext_image_copy_capture_manager_v1`. The test session advertises all three
interfaces. On X11, the agent enumerates top-level windows and reads the
selected window's offscreen backing pixmap through X Composite. Each frame
carries its current dimensions. A mapped window can be captured while
covered, and the stream pauses while the window is unmapped. The X11 window
preview is view-only. A Wayland session may also offer window selection
through the ScreenCast portal when its backend advertises that source type.

Verify capture with two distinct windows. Move, resize, and cover the selected
window and check that only its content is streamed. Verify what happens when
it becomes hidden and when it closes. Keep the viewer's frame handling and
transport independent of the capture backend.

### Semantic actions

Choose **Inspect controls** in the window preview to list available actions
and highlight controls with known bounds. The viewer draws the outlines over
the video using the same fitted rectangle, so they follow local preview
resizes. Inspect again after the remote window changes its layout. The Sway
backend requires `swaymsg`, Python 3 with PyGObject and the AT-SPI typelib,
and access to the user's accessibility bus.

Model semantic actions as requests that identify the selected window and an
application element or command. Return an explicit result. AT-SPI is the
first candidate for applications that expose accessible controls. Probe
element discovery, invoking a button action, and setting an editable field
in one test application. An application that exposes its own messages or
commands can use an application-specific adapter if that becomes a priority.

Resolve each request against the selected window and validate the object's
identity before dispatch. Report unavailable capabilities and stale objects
explicitly. Window capture can work independently of semantic actions.

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
- [Sway's Wayland server](https://github.com/swaywm/sway/blob/master/sway/server.c)
  registers the foreign-toplevel capture source manager.
- [X Composite](https://man.archlinux.org/man/XCompositeNameWindowPixmap.3.en)
  documents the offscreen backing pixmap and its lifetime.
- [ScreenCast portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
  describes window source selection and session lifetime.
- [AT-SPI Action](https://gnome.pages.gitlab.gnome.org/at-spi2-core/devel-docs/doc-org.a11y.atspi.Action.html)
  and [EditableText](https://gnome.pages.gitlab.gnome.org/at-spi2-core/libatspi/iface.EditableText.html)
  define operations exposed by accessible elements.

## Open questions

- Does the first release forward input locally, over SSH, or both?
- Which sessions should be discoverable from an SSH login with no graphical
  session environment?
- Should semantic actions prioritize accessible controls or messages defined
  by a particular application?

## Protocol impact

The standalone `ctream_agent` contract starts at 1.0.1 (internal build 1).
It provides discovery as JSON, capture as RGB frames, and input as a line-based
stream. This is the first version of the contract. Existing ctl contracts
retain their current versions.

The window milestone advances `ctream_agent` from 1.0.1 (build 1) to 1.0.2
(build 2). It adds window discovery, window capture, accessibility element
discovery, and semantic action requests. The agent continues to accept
display-level operations under 1.0.1.

Version 1.0.3 (build 3) adds X11 targets to window discovery and capture,
with dimensioned PPM frames following the existing Wayland window format.
The agent retains the 1.0.1 and 1.0.2 contracts.

Version 1.0.4 (build 4) adds optional normalized control bounds to the
Wayland `elements` response for the local overlay. The agent retains
contracts 1.0.1 through 1.0.3. The viewer can inspect controls on earlier
agents using the existing 1.0.2 response.
