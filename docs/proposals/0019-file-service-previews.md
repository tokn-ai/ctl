# Proposal 0019: Cancellable file services and floating previews

- Status: Implemented
- Created: 2026-11-13

## Summary

Expose a bounded read-only file service locally and over authenticated SSH for
file previews, directory trees, and Git status. Terminal file links open text,
Markdown, or images in a floating desktop view which can move into a dedicated
native window.

## Motivation

Terminal output often refers to files on a remote host. Opening those paths on
the desktop host gives the wrong result. File inspection should retain the
originating pane's host and working directory, and slow file or Git operations
must remain independent of terminal traffic and other file requests.

## Design

The Rust file engine is shared by local requests and `ctl-agent` handlers. It
supports existence/metadata, bounded ranged reads, directory pages, and
structured Git status. Paths are protocol fields interpreted on the serving
host. Relative paths require an absolute pane working directory; `~` expands
on that host. Symlinks remain visible, while reads resolve and verify a regular
file. Paths must be valid bounded UTF-8. Directory pages follow host enumeration
order and are best-effort across concurrent directory mutations.

The existing shared transport adds a fixed Files service. Each operation uses
one independently flow-controlled logical stream over a reusable SSH command
channel. Stream closure cancels the handler's operation and owned Git process.
The agent monitors closure while work runs, rather than assuming that dropping
a client stream automatically stops server work. Files has its own broker pool,
separate from Sessions and Forwarding. A fixed direct `ctl-agent files` endpoint
serves transports which do not use brokered shared streams.

The service admits at most eight active operations per process. Read ranges
are limited to 4 MiB, directory pages to 1,000 entries, and operation lifetimes
to 30 seconds. Frames, paths, Git output, and pending desktop cancellations are
bounded independently. Cancelling a filesystem operation prevents further
work; an already-issued OS filesystem call may finish on Tokio's worker pool.
Git uses fixed argument arrays, NUL-separated status records, bounded stdout
and stderr, no optional locks or filesystem-monitor execution, and process
termination on cancellation.

Desktop requests have window-owned IDs. Cancellation can arrive before request
registration; bounded pending-cancellation records preserve that intent. Page
reload and native window destruction cancel their owned operations. Duplicate
active request IDs are rejected and results from replaced previews are ignored.

File links retain the source pane's exact target and raw working directory.
Modifier-click checks existence before reading; web-link handling remains
HTTP/HTTPS-only. The nonmodal floating preview supports UTF-8 text/code,
Markdown rendered without active HTML or fetched images, and raster images.
Text displays at most 1 MiB with a truncation notice; images must fit in 4 MiB.
Loading, empty files, errors, and unsupported formats have explicit states.

Promotion transfers the loaded preview's target, path, location, display mode,
content, and scroll position through a bounded in-memory snapshot. The URL
contains only an opaque preview ID. Only the owning native window can load its
snapshot. Its route mounts the preview without loading the workspace or opening
terminal attachments. Closing the source window preserves dedicated previews;
closing a dedicated preview releases its snapshot. There is no disk cache for
preview contents.

## Invariants

1. File operations are read-only and do not select arbitrary commands or sockets.
2. A cancelled request cannot cancel siblings or consume their response bytes.
3. Remote identity is verified on the request channel before any file operation.
4. A relative file path never falls back to the desktop or agent process cwd.
5. Preview contents cannot execute scripts inside the privileged app webview.
6. Promotion preserves the existing preview and never creates a terminal session.
7. Older published contracts retain their prior behavior and service selectors.

## Protocol impact

`ctld` advances from `1.2.18` (internal build 18) to `1.3.19` (build 19), adding
`files` to `open_service`. `ctl_transport` advances from `1.0.1` (build 1) to
`1.1.2` (build 2), adding service selector 3. Both additions require explicit
selection of the new contract, while every previously published contract remains
supported. The independent `ctl_files` contract starts at `1.0.1` (build 1),
negotiating a single bounded operation and response per stream. Identity framing
is unchanged; its existing protocol list advertises the additional contract.
Product releases and storage schemas are unchanged. The authoritative contract
history and wire details are linked below.

## Out of scope

File mutation, editor/save workflows, filesystem watching, a desktop explorer
sidebar, PDF/office rendering, arbitrary HTML, and durable preview persistence.

## Unresolved questions

None.

## Detailed specifications

- [Control and file-service protocols](../ctl-protocol.md)
- [Published protocol versioning](../protocol-versioning.md)
- [Desktop usage](../../apps/desktop/README.md#visual-preview)
