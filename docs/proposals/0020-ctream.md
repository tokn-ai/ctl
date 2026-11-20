# Proposal 0020: Graphical session control with ctream

- Status: Proposed
- Created: 2026-11-19

## Summary

`ctream` discovers X11 and Wayland instances and forwards control to a selected
instance through a CLI and a native wgpu viewer. Its crates live under `ctream/`
as members of the root Cargo workspace and share its lockfile. The CLI, agent,
viewer, and Linux helpers run independently of `ctld`. Lifecycle management
through `ctld` is planned once the tool is stable.

## Motivation

A host can have several graphical sessions. An operator needs to identify the
intended desktop and direct input to it from either interface. Keeping ctream
independently buildable and runnable allows backend changes to be tested on a
Linux desktop throughout development.

## Design

Separate Rust libraries expose the protocol types, typed client, shared driver
traits, and official capability drivers. The CLI queries windows through the
client over SSH and uses the platform library for local queries. The wgpu
viewer uses the same client for discovery, capture, input, and semantic actions.

### Current architecture

| Crate | Directory | Runs in | Ctream dependencies | Responsibility |
| --- | --- | --- | --- | --- |
| `ctream-proto` | `ctream/proto` | Client and display host processes | None | Shared display, window, control, and wire contract types |
| `ctream-client` | `ctream/client` | CLI or viewer process | `ctream-proto` | Typed sessions, SSH/local transports, frame decoding, input forwarding, and coordinate transforms |
| `ctream-driver` | `ctream/driver` | Display host process | `ctream-proto` | Platform-neutral driver and capability traits, with capture cancellation |
| `ctream-driver-capture` | `ctream/driver-capture` | Linux display host | `ctream-driver`, `ctream-proto` | Official capture drivers and capturable-window enumeration |
| `ctream-driver-input` | `ctream/driver-input` | Linux display host | `ctream-driver` | Official input drivers |
| `ctream-platform` | `ctream/platform` | Agent or local CLI on the display host | `ctream-driver`, `ctream-proto`, `ctream-driver-capture` (Linux) | Display discovery, window-query dispatch, and AT-SPI controls |
| `ctream-cli` | `ctream/cli` | Display host for local queries; client machine for SSH queries | `ctream-client`, `ctream-platform`, `ctream-proto` | The `ctream` command |
| `ctream-agent` | `ctream/agent` | Linux display host | `ctream-driver`, `ctream-proto`; `ctream-driver-capture`, `ctream-driver-input`, `ctream-platform` (Linux) | Reusable request validation and stock-agent operation routing |
| `ctream-preview` | `ctream/preview` | Viewer machine | `ctream-client`, `ctream-proto` | Native wgpu preview, controls, and overlays |
| `ctream-wayland-input` | `ctream/wayland-input` | Linux display host | `ctream-proto` (Linux) | Wayland input and window capture helper binaries |

The dependency direction keeps native drivers out of the client library and
viewer. `Client<T>` selects wire contracts, validates requests, and decodes typed
replies. `Session<T>` binds operations to a selected display or window. The
client's `media` module decodes bounded RGB frames, while `geometry` supplies
the shared transform used by the preview image, control outlines, and pointer
mapping.

### Driver composition

Official driver crates are organized by capability, then platform and backend.
For example, `ctream_driver_capture::linux::wayland::Driver` implements capture,
and `ctream_driver_input::linux::wayland::Driver` implements input. Both crates
also expose `linux::x11`. Capture drivers enumerate their capturable windows;
the platform library dispatches window queries and resolves AT-SPI controls.

`ctream_driver::Driver` associates a `Target` with a `Session` and opens that
session from a borrowed target. Each capability has its own trait and resource
type. The agent's generic helpers require only the capability they execute, so
a capture-only session can be used without implementing input or discovery.
The stock agent selects the official capture and input drivers independently.

A user crate can group several capabilities under its own modules, such as
`user_ctream_driver_android::{capture, input}` from a package named
`user-ctream-driver-android`. This is a packaging example; Android support is
not implemented. Such a crate implements the shared traits and is compiled
into a custom agent. It can define its own associated target type, including
device identifiers, without depending on an official driver crate.

A driver's associated `Target` is separate from `ctream_proto::Target`, which
currently represents only the existing X11 and Wayland wire targets. The stock
agent does not load user drivers automatically. A custom agent supplies its
driver selection, routing, and protocol adaptation. It can reuse
`ctream_agent::protocol::parse` for the existing requests; adding a wire target
also requires corresponding protocol and client support.

Capture sources currently write encoded frame bytes into a caller-supplied
output and observe a cancellation lifetime. The existing raw RGB and PPM
formats remain selected by the protocol adapter. Input and accessibility helper
adapters retain their standard-IO endpoints. The driver interfaces do not yet
provide typed frame or input-event APIs.

### Client sessions

`Client<T>` is generic over a transport with associated reader and writer
types. A JSON query associates its reply type with its command. Version
selection and frame formats belong to the client. Both SSH and local subprocess
stdio carry the same agent protocol.

`Frame<B>` carries dimensions and pixel storage. `VideoSource::Buffer`
defines the storage used by a source, and the current RGB reader reuses a
`Vec<u8>` between frames. The preview converts completed frames into egui
images and keeps only the latest pending image.

`Point<Space>`, `Rect<Space>`, and `Transform<From, To>` distinguish
coordinate spaces. A fitted image geometry provides the forward transform
used by overlays and the inverse transform used by input. Bounds are clipped
to the displayed image. Local resizing recomputes this geometry.

Each operation owns its channel. Capture separates its frame reader from a
lifetime guard held by the viewer. Dropping the guard closes the control pipe
so the agent can stop capture. Process transports allow a bounded cleanup
period before terminating a stalled child. Input closure lets the helper
release held keys and buttons.

This step implements the existing display and window capabilities. Audio,
encoding, capability negotiation, and a shared remote session runtime are
future additions. The current client session groups a target selection across
independent agent operations. The existing protocol adapter supports deployed
agents during development.

The first milestone is read-only discovery of both X11 and Wayland from an SSH
login. An initial Linux host check found a reachable Xorg display with both
filesystem and abstract sockets, alongside a Wayland greeter in another user's
runtime directory. Report the two X11 listeners as one display. Label a
greeter separately when session metadata identifies it, and distinguish an
observed listener from a display that accepted a client connection.

From the repository root on the remote Linux host, install `ctream-agent` with
`cargo install --locked --path ctream/agent --root ~/.local`.
Install the CLI separately with
`cargo install --locked --path ctream/cli --root ~/.local`.
From the repository root on the viewer machine, run
`cargo run -p ctream-preview -- HOST` to discover remote displays and select
one in the viewer. Append `:DISPLAY` or
`wayland-N` to open a target directly.

For a display on the viewer machine, build with
`cargo build -p ctream-agent -p ctream-preview` and run
`cargo run -p ctream-preview -- --local`. The local transport finds the agent
beside the viewer executable, falling back to `PATH`, and launches it over
stdio IPC. It uses the same operation contracts as SSH.

The viewer opens a transport channel for each agent operation. The SSH
transport starts a standalone `ctream-agent` process per channel under the
remote account. Discovery returns structured display metadata. Capture streams
frames on a separate channel, and input uses its own duplex channel. Closing
the capture channel cancels its backend process. Closing the input channel
releases held inputs.

The agent uses FFmpeg with x11grab for X11 display capture and reads display
geometry through x11rb. X11 input needs Python 3, libX11, libXtst, XTEST, and
access to the selected display. The local wgpu viewer uses the same rectangle
for drawing the desktop and mapping clicks to display pixels.

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

- [AUV driver traits](https://github.com/moeru-ai/auv/blob/main/crates/auv-driver-common/src/traits.rs)
  associate providers with their sessions.
- [AUV capture model](https://github.com/moeru-ai/auv/blob/main/crates/auv-driver-common/src/capture.rs)
  retains capture origin and geometry alongside image data.
- [libobs backend design](https://docs.obsproject.com/backend-design)
  provides the source, encoding, and output boundaries for future media work.
- [libobs scenes](https://docs.obsproject.com/reference-scenes)
  compose sources through explicit transforms.
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

- How should providers report capability and permission changes during a session?
- Which operations need shared remote session state beyond independent channels?
- How should a fresh accessibility observation be associated with a video frame?

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

The provider, client, media, geometry, and local transport refactor, crate split,
and driver composition use `ctream_agent` 1.0.4 (build 4) with the same retained
contracts.
Protocol changes: none.
