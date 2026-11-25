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
pnpm add ../ctl/ctream/sdk/tokn-ai-ctream-sdk-0.1.0.tgz
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
const capabilities = await window.capabilities();
const frame = await window.screenshot();

if (capabilities.input) {
  const matches = await findText(frame, "Continue");
  if (matches.length !== 1) throw new Error("expected one visible Continue label");
  await window.click(matches[0], { size: [frame.width, frame.height] });
  const after = await window.screenshot();
  // Send these bytes to an image consumer to verify the new state.
  const png = Buffer.from(after.data, "base64");
  console.log(after.width, after.height, png.length);
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
  CLI invocation.
- `displays()` includes each display’s metadata and its selectable target,
  such as `:0`, `wayland-0`, `windows-1`, or `macos`. An inaccessible
  display has a null target. `windows(target)` returns `id`, `app_id`, and
  `title`. On Windows, `windows("windows")` lists windows across monitors.
- `session(target)` selects a display. `session(target, window.id)` selects a
  specific window. `resolveWindow(target, { app_id, title })` accepts either
  field or both and requires exactly one match. Discover the window again
  if the application recreates it.

### Actions and observation

| Task | TypeScript call |
| --- | --- |
| Read a fresh image | `await session.screenshot()` |
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
Await each action before observing again. An input call waits for the agent to
finish processing its channel. A fresh screenshot or element query can verify
how the application responded.

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
window input. The macOS agent serves one capture at a time, so finish other
viewer or SDK captures before starting a new call.

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

Other languages can use the same local `ctream_sdk` contract. Version 1.0.1
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
