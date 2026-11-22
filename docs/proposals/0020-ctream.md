# Proposal 0020: Graphical session control with ctream

- Status: Proposed
- Created: 2026-11-19

## Summary

`ctream` discovers X11, Wayland, and Windows displays and forwards control to a
selected display or window through a CLI and a native wgpu viewer. Its crates live under `ctream/`
as members of the root Cargo workspace and share its lockfile. The CLI, agent,
viewer, native Windows drivers, and Linux helpers run independently of `ctld`. Lifecycle management
through `ctld` is planned once the tool is stable.

## Motivation

A host can have several graphical sessions. An operator needs to identify the
intended desktop and direct input to it from either interface. Keeping ctream
independently buildable and runnable allows backend changes to be tested on a
Linux or Windows desktop throughout development.

## Design

Separate Rust libraries expose the protocol types, typed client, shared driver
traits, and official capability drivers. The CLI queries displays and windows through the
client over SSH and uses the platform library for local queries. The wgpu
viewer uses the same client for discovery, capture, input, and semantic actions.

### Current architecture

| Crate | Directory | Runs in | Ctream dependencies | Responsibility |
| --- | --- | --- | --- | --- |
| `ctream-proto` | `ctream/proto` | Client and display host processes | None | Shared display, window, capture-driver catalog, control, and wire contract types |
| `ctream-client` | `ctream/client` | CLI or viewer process | `ctream-proto` | Typed sessions, SSH/local transports, frame decoding, input forwarding, and coordinate transforms |
| `ctream-driver` | `ctream/driver` | Display host process | `ctream-proto` | Platform-neutral driver and capability traits, with capture cancellation |
| `ctream-driver-capture` | `ctream/driver-capture` | Linux or Windows display host, or the local macOS viewer process | `ctream-driver`, `ctream-proto`, `ctream-windows` (Windows) | Official capture drivers, window enumeration, and macOS system picker capture |
| `ctream-driver-input` | `ctream/driver-input` | Linux or Windows display host | `ctream-driver`, `ctream-windows` (Windows) | Official input drivers |
| `ctream-windows` | `ctream/windows` | Windows interactive session | `ctream-proto` | Shared window identity, enumeration, and physical geometry for native drivers |
| `ctream-platform` | `ctream/platform` | Agent or local CLI on the display host | `ctream-driver`, `ctream-proto`, `ctream-driver-capture` (Linux/Windows) | Display discovery, window-query dispatch, and AT-SPI controls |
| `ctream-cli` | `ctream/cli` | Display host for local queries; client machine for SSH queries | `ctream-client`, `ctream-platform`, `ctream-proto` | The `ctream` command |
| `ctream-agent` | `ctream/agent` | Linux display host or Windows interactive session | `ctream-driver`, `ctream-proto`; `ctream-driver-capture`, `ctream-driver-input`, `ctream-platform` (Linux/Windows) | Reusable request validation, stdio hosting, and stock-agent operation routing |
| `ctream-preview` | `ctream/preview` | Viewer machine | `ctream-client`, `ctream-proto` | Native wgpu setup and viewer windows, view-only source preview, controls, and overlays |
| `ctream-wayland-input` | `ctream/wayland-input` | Linux display host | `ctream-proto` (Linux) | Wayland input and window capture helper binaries |

The dependency direction keeps native drivers out of the client library and
viewer. `Client<T>` selects wire contracts, validates requests, and decodes typed
replies. `Session<T>` binds operations to a selected display or window. The
client's `media` module decodes bounded RGB frames, while `geometry` supplies
the shared transform used by the preview image, control outlines, and pointer
mapping.

### Driver composition

Official driver crates are organized by capability, then platform and backend.
Each capture backend has its own concrete `Driver` and associated session type.
The paths below are relative to `ctream_driver_capture`; these are the current
stock-agent providers, not a promise that every backend supports every source.

| Public driver path | Wire ID | Source and default | Runs in |
| --- | --- | --- | --- |
| `linux::x11::ffmpeg::Driver` | `linux.x11.ffmpeg` | X11 display capture; display default | Linux display host |
| `linux::x11::composite::Driver` | `linux.x11.composite` | X Composite window capture; window default | Linux display host |
| `linux::wayland::grim::Driver` | `linux.wayland.grim` | Wayland display capture; display default | Linux display host |
| `linux::wayland::image_copy::Driver` | `linux.wayland.image_copy` | Foreign-toplevel image-copy capture; window default | Linux display host |
| `windows::dxgi::Driver` | `windows.dxgi` | DXGI Desktop Duplication; display default | Windows interactive session |
| `windows::wgc::Driver` | `windows.wgc` | Windows Graphics Capture; window default | Windows interactive session |

Input remains a separate capability crate, with
`ctream_driver_input::{linux::x11, linux::wayland, windows}::Driver`.
Windows input uses `SendInput`. Window enumeration belongs to providers that
support it, and the platform library dispatches queries and resolves AT-SPI
controls. Windows display discovery uses a shared native display catalog;
enumerating a display does not require creating a DXGI capture session.

`ctream_driver::Driver` associates a `Target` with a `Session` and opens that
session from a borrowed target. Each capability has its own trait and resource
type. The agent's generic helpers require only the capability they execute, so
a capture-only session can be used without implementing input or discovery.
The stock agent selects the official capture and input drivers independently.
`ForwardWindowInput` is a separate optional capability from display input.
The Windows drivers share native window utilities through `ctream-windows`;
neither capability driver depends on the other.

The capture catalog lists the compiled providers applicable to a display and
their supported `display` or `window` scopes. It describes available code,
not a successful permission or compositor-capability check. Selecting `auto`
uses the default in the table for the chosen source. An explicit ID selects
exactly that backend; an unknown ID, incompatible scope, missing dependency,
or initialization failure is reported rather than silently choosing another
provider. The capture handshake reports the actual driver and frame format.
Input selection continues to follow the target independently of capture.

The earlier bundled capture paths `linux::x11::Driver`,
`linux::wayland::Driver`, and `windows::Driver` are replaced by the concrete
backend paths above. Custom agents must update those Rust imports. This is a
compile-time API change; retained agent wire contracts keep their default
capture behavior.

A user crate can group several capabilities under its own modules, such as
`user_ctream_driver_android::{capture, input}` from a package named
`user-ctream-driver-android`. This is a packaging example; Android support is
not implemented. Such a crate implements the shared traits and is compiled
into a custom agent. It can define its own associated target type, including
device identifiers, without depending on an official driver crate.

A driver's associated `Target` is separate from `ctream_proto::Target`, which
currently represents X11, Wayland, and Windows display wire targets. The stock
agent does not load user drivers automatically. A custom agent supplies its
driver selection, routing, and protocol adaptation. It can reuse
`ctream_agent::protocol::parse` for the existing requests; adding a wire target
also requires corresponding protocol and client support.
Catalog IDs are extensible strings, so a custom agent can advertise and route
its own compiled providers using the same source scopes and frame contracts.
Those wire IDs do not replace the concrete Rust `Driver`, associated target,
session, and capability types behind each routing adapter.

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

`AgentLaunch` selects one executable independently of the transport and target
platform. `SshCommand` configures an SSH-compatible program and argument prefix;
`Ssh` retains the default OpenSSH behavior. The CLI and viewer share `--agent`,
`--ssh-program`, and repeated `--ssh-arg` options. Executable paths and protocol
arguments are quoted as individual POSIX shell arguments, including Windows
executables launched from a WSL shell. No shell-script launcher is required.

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

### Welcome window and viewer lifecycle

The viewer starts with a small native wgpu welcome window. Choose a capture
driver first, then select one of its compatible sources. Every provider
advertised by the connected agent appears as a card with platform/session and
Display/Window capability tags. Drivers with no accessible matching session
remain visible with a **No sources** tag. A driver supporting just one scope
sets that scope directly; only Automatic and drivers supporting both scopes
need an additional source-type choice. A live preview shows the selection
before control begins.
Target arguments preselect the welcome controls; they do not bypass Start.
Choosing a driver or changing its source type selects the first available
source and starts a view-only preview. Window sources appear in one picker
across the sessions supported by the driver. Titles use the viewer machine's
installed font fallbacks.
An incompatible window preselection stays an error until an explicit driver
choice changes its scope; catalog discovery never widens it to display capture.
With no arguments the welcome window lists saved workspace hosts. Use `--local`
to capture on the viewer machine. The `--host`, `--agent`, `--ssh-program`, and
`--ssh-arg` options can still launch a manually installed remote agent.
`--capture-driver ID` preselects
an explicit provider, while omission selects **Auto**.

The welcome preview opens capture only. Pointer movement, clicks, scrolling,
and typing in the welcome window never open an input channel or send remote
input. **Start** opens a separate native viewer window, hides setup, and enables
mouse and keyboard forwarding where that source supports it. Setup retains the
same capture and source selection; driver/source changes are locked while the
viewer is open. A view-only source can still open in the viewer. Input
initialization failure is reported without discarding a usable capture stream.
The viewer opens without a title bar. Hover at its top edge to reveal the
window controls. Drag the bar to move the viewer, or use its minimize,
maximize, and close buttons. Closing the viewer releases input, activates
setup, and keeps the same stream available as a view-only preview. On macOS
the controls appear in an attached panel flush with the top of the viewer, or
with the bottom when there is only room below. The panel stays visible while
you drag it to move the viewer. It keeps the same horizontal position and
width as the video, including when the viewer extends past a screen edge or
across displays. The video keeps its screen position and size
as the controls appear and hide. Windows, Linux, and macOS windows without room
for the panel use a separate strip inside the viewer, above the video.
A resizable viewer grows to make room for that strip.
Video clicks use the image rectangle in either layout. The viewer opens at the
source aspect ratio and adjusts its inner size after a source resize or a
manual window resize. Use
the native resize edges on macOS or the bottom-right resize grip on Linux and
Windows. The inspector keeps its own panel. The compositor can constrain
window sizes, especially when maximized or fullscreen.

In agent-backed setup, enable **Debug overlay** before selecting **Start** to show
diagnostics in the viewer. Download and upload measure ctream application
bytes per second, before SSH compression and framing. Latency is round-trip
time to the agent over a dedicated channel on the same transport. Agents older
than `ctream_agent` 1.0.10 show `N/A` for latency. FPS counts fresh frames the
viewer submits for rendering. After a long gap between paints, the overlay
waits for a fresh sample. **Frame** shows the received capture dimensions in
pixels.

Both windows use the same egui context and wgpu renderer. The native viewer is
an immediate child viewport, so its events and repaint requests run through
the root setup viewport even while setup is hidden. Only the viewer viewport
forwards input; painting a view-only preview must not reset viewer-held keys
or buttons. Capture keeps the newest frame and signals a root repaint without
waiting for the renderer.

On a **local native Wayland viewer**, winit 0.30.13 implements window visibility
and focus requests as no-ops. Automatic setup hiding and activation therefore
cannot be guaranteed on that backend. This is a local window-management
limitation: it does not affect capturing or controlling a remote Wayland
session from a viewer on another platform.

Changing the source or driver cancels the previous preview capture and clears
its image. The agent-wide driver catalog has its own request lifetime, so
changing a source keeps the full provider list. Window queries, connection
results, frames, and errors belong to the selection that requested them.
A late result must not replace a newer choice or keep a discarded capture
alive. Start remains unavailable until the current capture has produced a
live frame. Closing setup or quitting the application cancels capture, input,
and pending requests. Closing the viewer cancels its input initialization and
releases held keys/buttons while retaining capture for setup. When a remote
window capture ends cleanly, the viewer closes and setup refreshes the window
list. If capture fails, the viewer keeps the last frame and ends input so you
can check the capture error and retry from setup.

The target-independent catalog uses `ctream_agent` 1.0.8 (build 8), with
explicit system metadata so custom driver IDs need no platform naming
convention. With a 1.0.7 agent, the viewer combines catalogs from discovered
targets; it cannot show providers for systems with no discovered session.
Older agents remain usable through **Auto** and their retained capture
contracts. An explicit driver override requires the 1.0.7 capture handshake;
the client must not pretend an older agent honored a backend choice.

Protocol changes: none. The separate-window lifecycle keeps `ctream_agent`
at 1.0.8 (build 8); capture and input operations, handshakes, and transport
behavior retain their existing contracts.

### Workspace hosts

Install and open ctream with `ctlup`:

```sh
ctlup ctream install
ctlup ctream run
```

Install and update use the latest successful **ctream preview bundles** workflow
on `main`. Automatic downloads require the GitHub CLI (`gh`) with an authenticated
session. Use `--run RUN_ID` for a specific successful run from the official
repository or `--from ctream-preview-<target>.zip` for offline installation.
Use `ctlup ctream update` to select a newer bundle, `ctlup ctream status` to see
the active build, and `ctlup ctream cli -- list` to invoke the CLI. Preview flags
can be passed with `ctlup ctream run -- OPTIONS`.

`ctlup` keeps the active selection and installed generations in
`$CTL_DATA_DIR/ctlup/store/ctream/` (`~/.tokn/ctl/ctlup/store/ctream/` by default).
Install or build a fresh generation after upgrading from `ctream/installs/`.
The local `$CTL_DATA_DIR/ctream/` directory is reserved for configuration.
The viewer and its `ctream-agents` directory stay together in each generation.
`ctl-bundle` defines the layout used by the preview, development builder, CI
packer, and installer. For a manual install, extract the ZIP and run
`ctream-preview` directly. Keep `ctl` available on `PATH` or beside the preview.
For a host that needs an interactive SSH prompt, launch the viewer from a
terminal or connect the host in ctl first.

1. Select a saved host and click **Connect**. For a Windows desktop, select its
   WSL SSH endpoint. The viewer detects the platform, syncs a matching agent
   over the saved route, and verifies it can launch before loading sources.
2. Choose the capture driver and source and check its live preview. Click
   **Start** to open the viewer and enable control for supported sources.
3. Close the viewer to return to setup. **Change host** returns to the workspace
   list.

The archives include agents for x86-64 Windows and x86-64/ARM64 Linux. The
workflow produces viewer packages for Apple Silicon macOS, x86-64 Linux, and
x86-64 Windows. The
Linux Wayland helper binaries require glibc 2.35 or later and the input helper
uses the system `libxkbcommon`. Capture providers still need their platform
dependencies and access to the graphical session.

For development, use `ctlup dev --ctream` with `--agent-target` for each remote
platform you need. See [ctream development builds](../local-bundles.md#install-a-ctream-development-build).
A direct `cargo run -p ctream-preview` can also find agents built in Cargo's
target directories. The workspace picker uses ctl's saved-host CLI. The remote
ctream agent is a standalone process started over SSH for each operation. The preview
saves synchronized agents under `~/.tokn/ctl/ctream/agents` on remote Linux
hosts, honoring an absolute `CTL_DATA_DIR` in the remote environment.

### Windows through a WSL endpoint

The workspace host flow saves `ctream-agent.exe` under `.tokn/ctl/ctream/agents`
in the native Windows user's profile, accessed from WSL. For a manual install,
build the executable on Windows with
`cargo build --release -p ctream-agent` and place it in a user-owned Windows
directory that WSL can access. A Windows binary is required for the native
capture and input drivers.
The WSL SSH endpoint launches the executable through interop and carries the
same binary stdio protocol. The agent must reach the intended interactive
Windows session. Direct Windows OpenSSH launches in session 0 did not expose
desktop outputs in the initial experiment; the WSL route reached the interactive
desktop without a scheduled task.

| Component | Runs where |
| --- | --- |
| CLI, wgpu viewer, overlays, frame decoding | Client machine |
| SSH server and executable launch | WSL distribution |
| `ctream-agent.exe`, DXGI/WGC capture, `SendInput` | Windows interactive session |

For example, replacing the host and mounted executable path with the installation:

```sh
ctream --host wsl-host --agent /mnt/c/ctream/bin/ctream-agent.exe list --json
ctream-preview --host wsl-host --agent /mnt/c/ctream/bin/ctream-agent.exe windows-1
```

When using a saved ctl host, add `--ssh-program ctl --ssh-arg ssh` before the
CLI command or preview target. This invokes the installed `ctl ssh` command;
ctream still has no dependency on ctld or its lifecycle.

Display names use the number in the Windows GDI device name (`windows-1` for
`DISPLAY1`), rather than the order in which GPUs are enumerated. Discovery
includes physical desktop bounds. Capture returns full-resolution RGB PPM
frames in the display's visible orientation, with a ceiling of eight frames
per second. Input maps the preview's fitted image into that selected display
and the virtual desktop, including negative origins and mixed DPI settings.
Physical key events use the Windows keyboard layout. Closing or resetting the
input channel releases keys and buttons injected by that channel.

Windows supports displays and individual windows. Accessibility actions, audio,
and hardware video encoding remain future work. A separately rendered hardware
cursor is not composited into the DXGI display image. Windows integrity restrictions still apply to elevated apps
and secure desktops. Display reconfiguration or desktop access loss requires
reconnecting; input rejects a changed display layout before sending new events.
WSL restart, Windows sign-out, and session switching require further live checks.

This step implements the existing display and window capabilities. Audio,
encoding, broader capability negotiation, and a shared remote session runtime are
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
one in the welcome window. Append `:DISPLAY` or `wayland-N` to preselect a
target, verify its live preview, and choose **Start**.

On macOS 14 or later, open the viewer and select **Capture a local window**.
`ctream-preview --local` opens the same setup directly. With Screen Recording
permission, the **Window** list works like the remote window list. Selecting a
window opens its preview, and **Refresh windows** updates the list. **Use macOS
picker…** grants ScreenCaptureKit access to one window for the capture session.
The setup preview is view-only. **Start** opens the separate viewer and enables
mouse and keyboard control when the viewer has macOS Accessibility permission
and ScreenCaptureKit provides the selected window's identity. For windows chosen
with the macOS picker, this identity is available on macOS 15.2 and later.
Closing the viewer releases held input and returns to setup. The ScreenCaptureKit
and input drivers run inside the viewer process.

Input targets the selected app and checks that its focused window matches the
capture. AppKit views can decline clicks while their app is inactive. Selecting
a window for capture leaves the Mac desktop's focus and pointer in place.

### macOS over SSH

Install a native `ctream-agent` on the Mac with
`cargo install --locked --path ctream/agent --root ~/.local`.
From a terminal in that Mac's logged-in desktop session, start the agent with
`~/.local/bin/ctream-agent serve` and leave it running. The host process needs
Screen Recording permission to list and capture windows. Grant it Accessibility
permission to control the selected window. Set those permissions in macOS
System Settings and restart the host process after changing them.

From the viewer machine, connect over SSH as the same macOS user. Substitute
your SSH destination and the absolute path to the installed agent.

```sh
ctream --host MAC_HOST --agent /path/to/ctream-agent list --json
ctream --host MAC_HOST --agent /path/to/ctream-agent windows macos
ctream-preview --host MAC_HOST --agent /path/to/ctream-agent
```

Select **ScreenCaptureKit** and choose a window. **Start** enables control
when the agent has Accessibility permission. The macOS agent supports one
active capture and accepts input through a separate stream. Closing the capture
ends control. The remote agent is installed and started manually.

| Component | Runs where |
| --- | --- |
| CLI, wgpu viewer, and overlays | Viewer machine |
| SSH server and per-operation stdio relay | SSH login on the Mac |
| ScreenCaptureKit and window-scoped input | `ctream-agent serve` in the Mac's logged-in desktop session |

For a display on a Linux or Windows viewer machine, build with
`cargo build -p ctream-agent -p ctream-preview` and run
`cargo run -p ctream-preview -- --local`. The local transport finds the agent
beside the viewer executable, falling back to `PATH`, and launches it over
stdio IPC. It uses the same operation contracts as SSH.

The viewer opens a transport channel for each agent operation. SSH launches
`ctream-agent` under the remote account. On macOS this process relays requests
to the desktop agent. Discovery returns structured session metadata. Capture
streams frames on one channel, and input uses its own duplex channel. Closing
the capture channel stops the stream, and closing the input channel releases
held inputs.

The agent uses FFmpeg with x11grab for X11 display capture and reads display
geometry through x11rb. The X11 display driver keeps only the newest pending
frame when transport writes stall. It waits at least 125 ms after each completed
frame write before sending another. X11 input needs Python 3, libX11, libXtst,
XTEST, and access to the selected display. The local wgpu viewer uses the same
rectangle for drawing the desktop and mapping clicks to display pixels.

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
Choose a window driver in the welcome window or launch the viewer with
`HOST --windows` to open the window picker. X11 and Wayland windows retain
their originating session internally. The separate window helper is only
required for the Wayland backend.

From the CLI, run `ctream windows DISPLAY` on the display host, or
`ctream --host SSH_HOST windows DISPLAY` to query through the remote agent.
The command lists window IDs, application IDs, and titles. Add `--json` for
structured output.

To preselect a previously discovered ID, use
`ctream-preview --host SSH_HOST DISPLAY --window WINDOW_ID`. The same agent and
SSH launcher options apply to this command and to the window picker. Its live
preview remains view-only until **Start**.

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

On Windows, the desktop-wide target in `ctream_agent` 1.0.9 enumerates visible
top-level windows across monitors. Use `ctream --host SSH_HOST windows windows`
to query that target from the CLI. Older agents use monitor-scoped queries, and
the preview combines their results into one picker. Windows Graphics Capture
(Windows 10 1903 or newer) follows resizing and remains bound to the window
when it moves across monitors or another window covers it. Minimizing requires
restoring and reconnecting, and closing the window ends capture. The provider
keeps the operating system's capture border.

Windows window input uses physical DWM frame bounds shared with capture.
The viewer maps its fitted image into those bounds after resizing. An explicit
click or wheel action may activate the selected window; hover never activates
it, and keyboard input requires it to be foreground. Foreground and hit tests
reject input that would reach an unrelated window. `SendInput` still uses the
shared desktop cursor and focus; these checks cannot make injection atomic with
other desktop activity. Closing/resetting input releases this channel's held
keys and buttons. Windows accessibility/message-based actions are separate
future capabilities.

If Windows denies foreground activation, a physical click can still use normal
mouse activation when the selected window is visible at the verified cursor
position and no other window owns mouse capture. Scrolling has no such fallback.

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
- Welcome previews never open input; Start is the explicit transition into the
  full viewer's control lifecycle.
- Source and driver changes discard old capture lifetimes and stale results.
- An explicit driver selection never silently falls back to a different backend.
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

Windows display support advances `ctream_agent` from 1.0.4 (build 4) to 1.0.5
(build 5). It adds the `windows-N` target for `capture` and `input` and a
`windows` discovery variant containing `number`, `name`, and physical `bounds`.
Windows frames use the existing dimensioned P6 RGB format. Its input channel
starts with `ready` and accepts dimensioned `move`, `button`, `key`, and `reset`
commands. Windows window and accessibility operations remain unavailable.

All Linux contracts 1.0.1 through 1.0.4 remain supported with their existing
wire shapes, including normalized control bounds under 1.0.4. New clients try
1.0.5 discovery and fall back to 1.0.1 for earlier agents. Windows display
operations require 1.0.5; selecting an earlier contract never returns the new
Windows discovery variant. Executable selection and WSL launch configuration
are local transport settings. Other named protocols and product versions are
unchanged.

Windows window support advances `ctream_agent` from 1.0.5 (build 5) to 1.0.6
(build 6). It enables `windows` and `capture-window` for Windows targets and adds
`input-window DISPLAY WINDOW`. That channel uses `ready` and the existing Windows
dimensioned move/button/key/reset format, with coordinates relative to the window
capture. Frames remain P6 RGB. Windows accessibility is still unavailable.
Contracts 1.0.1 through 1.0.5 remain supported unchanged; clients select 1.0.5 for
Windows display operations and 1.0.6 only for Windows window operations.
Other named protocols and product versions remain unchanged.

Capture driver selection advances `ctream_agent` from 1.0.6 (build 6) to 1.0.7
(build 7). `capture-drivers DISPLAY` lists compiled capture providers and their
source scopes. `capture-with-driver DISPLAY DRIVER [WINDOW]` accepts `auto` or
an explicit provider ID and prefixes its existing frame bytes with a bounded
JSON line identifying the actual `driver` and `format`. Initialization must
succeed before the handshake reports a driver. Earlier contracts 1.0.1 through
1.0.6 retain their request and frame shapes, default capture providers, and
input behavior. Welcome-window staging and the Rust module split do not add
input operations or change input permissions. Other named contracts and
product release versions remain unchanged.
