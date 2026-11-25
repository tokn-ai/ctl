# ctream TypeScript SDK

`@tokn-ai/ctream-sdk` lets a Node.js or Bun agent observe and control a selected
display or window on the local machine or over SSH. A screenshot contains the
PNG and its dimensions. A pointer action uses pixel coordinates from that
screenshot. The SDK also exposes keyboard shortcuts, OCR, and Wayland
accessibility controls.

## Install from this checkout

Build the CLI on the machine running your TypeScript application and pack the
SDK from the repository root.

```sh
cargo install --locked --path ctream/cli
pnpm install --frozen-lockfile
pnpm --dir ctream/sdk pack
```

In the downstream project, install the resulting tarball. Adjust the relative
path to your checkout.

```sh
pnpm add ../ctl/ctream/sdk/tokn-ai-ctream-sdk-0.1.1.tgz
```

Install `ctream-agent` on the display host with
`cargo install --locked --path ctream/agent` or use the agent from an existing
ctream installation. For remote access, use the SSH destination and agent
executable that work with `ctream --host HOST list --json`. On macOS, run
`ctream-agent serve` in the logged-in desktop session with Screen Recording
permission for capture and Accessibility permission for input.
[Platform setup](https://github.com/tokn-ai/ctl/blob/main/docs/proposals/0020-ctream.md#macos-over-ssh)

## Observe and act

```ts
import { Ctream, findText } from "@tokn-ai/ctream-sdk";

const ctream = new Ctream({ host: process.env.CTREAM_HOST });
const displays = await ctream.displays();
console.log(displays.map(({ target, selectable }) => ({ target, selectable })));

// Select the display and exact title supplied by the caller.
const [target, title] = process.argv.slice(2);
if (!target || !title) throw new Error("supply a display target and a window title");
const window = await ctream.resolveWindow(target, { title });
try {
  const capabilities = await window.capabilities();
  const frame = await window.screenshot();

  if (capabilities.input) {
    const matches = await findText(frame, "Continue");
    if (matches.length !== 1) throw new Error("expected one visible Continue label");
    await window.click(matches[0], { size: [frame.width, frame.height] });
    const after = await window.screenshot();
    const png = Buffer.from(after.data, "base64");
    console.log(after.width, after.height, png.length);
  }
} finally {
  await window.close();
}
```

`findText` is optional. It runs Tesseract on the **caller’s machine** and uses
its installed language data (`eng` by default). It matches whole words or a
phrase on one line and returns the text, bounding box and center in screenshot
pixels. For example, `findText(frame, label, { language: "deu" })` selects the
German model. A vision agent can consume `frame.data` directly and call
`window.click({ x, y })` using a point from its observation.

### Selecting a target

- `new Ctream()` uses the local host. `new Ctream({ host })` uses SSH. The
  constructor accepts `executable` for the local CLI, `agent` for the display
  host’s executable, and `ssh_program` and `ssh_args` for an SSH-compatible
  launcher. Each method returns a promise. An optional `timeout_ms` bounds a
  discovery command or dispatched session request.
- `displays()` includes each display’s metadata and its selectable target,
  such as `:0`, `wayland-0`, `windows-1`, or `macos`. An inaccessible
  display has a null target. `windows(target)` returns `id`, `app_id`, and
  `title`. On Windows, `windows("windows")` lists windows across monitors.
- `session(target)` selects a display. `session(target, window.id)` selects a
  specific window. `resolveWindow(target, { app_id, title })` accepts either
  field or both and requires exactly one match. Discover the window again
  if the application recreates it.

### Session lifetime

Keep the object returned by `session()` or `resolveWindow()` for the whole
task. Its first operation starts one local `ctream session` process. The first
screenshot opens a capture stream, which keeps receiving complete frames until
the session closes. On updated agents, the first physical input opens a second
channel to the same target. Subsequent pointer and keyboard calls reuse it.
Accessibility operations still issue individual agent requests.

Calls on one session run in submission order. Call `await session.close()`
in a `finally` block to release its channels and wait for the CLI to exit.
Set `timeout_ms` to bound how long a pending request can delay close.
Runtimes with async disposal also support `await using session = ctream.session(target)`.
The SDK never replays input after a disconnect or timeout. If the connection
fails, open a new session and observe the application before deciding what to do.

The default SDK configuration requires a local CLI with `ctream_sdk` 1.0.4
and a display host with `ctream_agent` 1.1.16. On Wayland, update
`ctream-wayland-input` alongside the agent. On macOS, also update the
running `ctream-agent serve` broker.

### Agents on different machines

Construct one client per agent. When you omit `agent_id`, the SDK picks a
lowercase word and four random hexadecimal characters, such as `otter#7c2a`.
Read `client.agent_id` to correlate its actions with the host's history. The
client uses that ID for every session it opens. Pass an explicit ID to keep
the same label across application restarts.

```ts
import { Ctream } from "@tokn-ai/ctream-sdk";

const client = new Ctream({ host: "display-host" });
console.log(client.agent_id);
await using window = await client.resolveWindow(":0", { title: "Editor" });
await window.pressKeys("Tab");
```

On the **display host**, use the desktop account that runs ctream to read its
recent activity. The command returns JSON, newest first.

```sh
ctream-agent activity --limit 20
```

Each entry contains the caller's `agent_id`, a host-generated `session_id` and
`action_id`, a timestamp in Unix milliseconds, the selected target and window,
the operation name, and its status. A host records `unknown` before dispatch.
It updates the entry to `succeeded` on acknowledgement or `failed` on a
reported error. An interrupted operation can leave an unknown entry, and a
failed batch can have partial effects. History retains the newest 10,000
mutations. Typed text, keys, coordinates, and screenshots stay out of history.

Agents connecting to the same display-host account can keep independent
sessions open on the same window. The host serializes whole pointer, keyboard,
and accessibility actions across SDK sessions. Observations run independently,
so another agent can change the screen between a screenshot and the next
action. When two clients happen to use the same label, the host-generated
session IDs distinguish their sessions.

`agent_id` is a caller-supplied or generated label. SSH authenticates the
connecting account. Explicit labels such as `reviewer` can contain 1–64
ASCII letters, digits, periods, underscores, and hyphens and must start with
a letter or digit. These labels also work with local CLI contract 1.0.3 and
host contract 1.1.15. An explicit ID in the generated format uses the newer
contracts and has the same 64-character limit. A host that cannot attribute
the session rejects it before dispatch.

### Actions and observation

| Task | TypeScript call |
| --- | --- |
| Read a screenshot from the stream | `await session.screenshot()` |
| Move, click or drag | `move(point)`, `click(point, { button: "right" })`, `drag(from, to)` |
| Scroll at a point | `scroll(point, "down", { steps: 3 })` |
| Enter text or press a shortcut | `typeText("Hello!")`, `pressKeys("ctrl+a")`, `pressKeys(["Super_L", "Return"])` |
| Inspect a window’s controls | `const controls = await session.elements()` |
| Invoke an advertised action | `invoke(control.path, index, control.actions[index])` |
| Replace an editable control’s contents | `setText(control.path, text)` |

Pointer coordinates are integers from the top-left pixel of the selected
image. By default the session uses the dimensions of its most recent
screenshot. Supply the source dimensions explicitly for an earlier frame or
after processing an image asynchronously. Map points from a resized model
image back to the source image first. Scrolling accepts 1–256 steps.

`pressKeys` sends a simultaneous chord of up to 32 physical keys, then releases
them in reverse order. `typeText` sends US keyboard positions for printable
ASCII, tabs, and line breaks. The active keyboard layout and Caps Lock affect
the resulting text. Wayland editable controls accept Unicode via `setText`.
Physical input resolves when the agent acknowledges dispatch. Applications
can repaint later. A screenshot returns a complete frame from the selected
stream and includes its current dimensions after a resize. Later screenshots
wait up to 250 ms for an update and can return the same frame on an idle
desktop. To verify an action, check for the expected visual or accessibility
state and observe again within your application's deadline.

| Selected target | Screenshot | Pointer and keyboard | Accessibility elements and actions |
| --- | --- | --- | --- |
| X11 display or window | Yes | Yes | — |
| Wayland display | Yes | Yes | — |
| Wayland window | Yes | — | Yes, with AT-SPI |
| Windows monitor or window | Yes | Yes | — |
| macOS window | Yes | Yes, with a matching active capture | — |

`capabilities()` describes operations supported for that type of target. The
display host checks permissions, backend availability, and window identity
when each operation runs. The SDK holds the required capture during macOS
window input. Each named macOS session binds input to its own live window
capture. CLI sessions pinned to 1.0.2 and the viewer share the legacy capture slot.

## Moving an AUV workflow

| AUV workflow | ctream equivalent |
| --- | --- |
| Connect and select a Runner or Device | Construct `Ctream` with the local or SSH connection |
| `runner.displays.list()`, `runner.windows.resolve(...)` | `ctream.displays()`, `ctream.resolveWindow(target, { app_id, title })` |
| Capture a frame and fetch its encoded image | `session.screenshot()` returns dimensions, MIME type, and base64 PNG in one call |
| Find a label and click its center | `findText(frame, label)`, then `session.click(match)` on a target with input |
| Click, scroll, type, or send a chord to a window | Use the corresponding method on the selected session |

AUV window coordinates can be logical points. Recompute positions using the
ctream screenshot’s pixel dimensions when migrating a recorded workflow.
Run tracing, Device pairing, extension runners, clipboard paste, and browser
transports have no direct ctream SDK equivalent.
[AUV TypeScript Runner API](https://github.com/moeru-ai/auv/blob/main/js/packages/sdk/README.md#call-runner-capabilities)

## CLI operation reference

Other languages can open `ctream session TARGET [--window ID] [--agent-id ID]`.
The default contract, `ctream_sdk` 1.0.4 (build 4), generates an ID when the
option is omitted and uses it for the life of that process. Read the ID in the
host's activity history, or pass a known ID to correlate it with caller logs.
SDK 1.0.3 (build 3) requires an explicit classic label. Both use the same
newline-delimited requests and replies as 1.0.2.

To open a legacy session outside host attribution and action ordering, pass
`--protocol 1.0.2` (build 2) and omit `--agent-id`.
Write one UTF-8 JSON operation per line, up to 64 KiB excluding the newline.
Read one flushed JSON reply per request, in order. Success carries
`{"ok":true,"result":...}`. An operation error carries
`{"ok":false,"error":"..."}` and allows the next request. Closing stdin
releases the session. A truncated or oversized record ends the process with
a nonzero exit status.

```sh
printf '%s\n' '{"operation":"capabilities"}' '{"operation":"screenshot"}' |
  ctream session :0 --protocol 1.0.4
```

The retained one-shot contract, version 1.0.1
(build 1) reads one UTF-8 JSON object of up to 64 KiB on stdin. A successful
process writes one JSON value and a newline to stdout. A failure exits with
a nonzero status and writes a diagnostic to stderr.

```sh
printf '%s\n' '{"operation":"capabilities"}' | ctream do :0 --protocol 1.0.1
printf '%s\n' '{"operation":"screenshot"}' | ctream do :0 --window x11-7 --protocol 1.0.1
```

| `operation` | Additional request fields | Result |
| --- | --- | --- |
| `capabilities` | — | `{capture,input,semantics}` booleans |
| `screenshot` | — | `{width,height,mime_type,data}` |
| `move` | `point,size` | `null` |
| `click` | `point,size,button` | `null` |
| `drag` | `from,to,size,button` | `null` |
| `scroll` | `point,size,direction,steps` | `null` |
| `press_keys` | `keys` array of native ctream key names | `null` |
| `type_text` | `text` | `null` |
| `elements` | — | Array of window controls |
| `invoke` | `path,index,name` | `null` |
| `set_text` | `path,text` | `null` |

A point is `{"x":10,"y":15}`, a size is `[width,height]`, a button is
`left`, `middle`, or `right`, and a direction is `up`, `down`, `left`, or
`right`. Use `ctream list --json` and `ctream windows TARGET --json` for
discovery. The CLI chooses the retained remote agent contract for each operation.
