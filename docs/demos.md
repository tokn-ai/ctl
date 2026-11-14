# Landing-page demos

Three native-app recordings: persistent sessions, local/remote work, and VPN
access. Managed tasks are deferred. This kit prepares sample content and repeatable
capture/export commands; it does not automate UI interaction or provision SSH/VPN
infrastructure. Protocol changes: none.

## Recording setup

Use the same macOS app build, light/dark appearance, window size, terminal font
size, and capture rectangle for all three clips. Aim for a 1280 × 800 point app
window and terminal text that remains readable at half size. Capture only the
app, including its native title bar; keep the menu bar, Dock, and operator terminal
outside the rectangle. Enable Do Not Disturb and keep the pointer still between
actions. Leave two seconds of stillness at the beginning and end of each take.

Use a dedicated demo OS account and demo SSH/VPN identities. The VPN UI displays
account and server metadata: changing the profile's display name alone does not
hide those values. Use a test environment whose visible values are approved for
publication. Complete authentication, host-key verification, component installation,
Docker image pulls, and any OS recording permission prompts before recording.
Never capture passwords, sign-in links, real account names, personal paths, or
unrelated hosts. Review the entire recording, including transitional frames.

Use the real native app for final footage. `pnpm desktop:preview` is useful for
visual rehearsals, but its terminal, SSH, and VPN operations are simulated.
The fixture below runs a real counter process and reads a real file. Its Orbit
project content is fictional; it does not claim to run a build or health check.

For the local persistence clip, launch the desktop with a fresh config directory
to keep your regular workspace and hosts out of the recording:

```sh
CTL_CONFIG_DIR="$(mktemp -d /tmp/ctl-demo-config.XXXXXX)" pnpm desktop:dev
```

This profile shares the normal terminal daemon. Create and import only the
named demo session. To isolate the daemon and its sessions as well, set
`CTL_DATA_DIR` to a fresh directory for both the desktop launcher and the CLI.

Prerequisites:

- A working native desktop app and matching `ctmux` CLI/daemon.
- A dedicated SSH host saved as **demo-remote**, with compatible remote components
  and `/bin/bash`. Use an account with no personal files in view.
- A saved VPN named **Demo VPN**, a running Docker-compatible engine, and valid
  demo credentials. Either supported provider can be used; authenticate off camera.
- The SSH host's selected method must use **Demo VPN** in **Connect through**.
  Its SSH endpoint must require that route. Verify this in rehearsal; an unrelated
  direct or system VPN route would not demonstrate ctl's VPN routing.
- `ffmpeg` on PATH for exports. Capture uses macOS's built-in `screencapture`.

The app's VPN proxy routes selected hosts, not system-wide traffic. Disconnect
releases one daemon's interest; another daemon may keep a shared container alive.
For a clean disconnected starting state, stop use from other demo clients and
wait until the UI actually reports disconnected. Do not stop unrelated services.

## Prepare the sample workspace

From the repository root, in an operator terminal outside the recording area:

```sh
pnpm demo prepare
```

Each call creates a fresh directory under `/tmp` and prints the exact command to
create **demo-local**. Run that command off camera, using the same ctmux daemon as
the desktop. If `demo-local` already exists, reuse it or explicitly terminate that
demo session before creating its replacement; the kit never terminates sessions.
In the app choose **+ → Attach session → Local**, import **demo-local**, and open it.
The prompt should read `orbit $` with no personal startup output.

Prepare the remote fixture with the VPN connected. These commands use the existing
saved host and may prompt for authentication; run them off camera:

```sh
demo_remote_dir=$(ctl -H demo-remote exec -- mktemp -d /tmp/ctl-demo-XXXXXX)
ctl scp -r demos/workspace/. "demo-remote:$demo_remote_dir/"
ctl -H demo-remote ctmux new -ds demo-remote -c "$demo_remote_dir" -- /bin/sh "$demo_remote_dir/launch.sh"
```

Import **demo-remote** through **Attach session**, choosing the saved remote host.
Confirm `sh inspect.sh` prints a clickable README link and that Cmd-click opens
the Markdown preview from the remote host. Keep the local and remote sessions
visible in the sidebar. Clear both terminals with `clear` after rehearsal.

The clean shell uses only system commands, suppresses personal startup files,
and disables history. The file link carries the actual temporary file location
so preview does not depend on shell cwd reporting. Do not show shell setup or
remote preparation in the finished clips.

## Demo 1 — Leave and come back

**Message:** Your session keeps running when you close its tab.
**Target length:** 20 seconds. **Starting state:** local shell selected, clear
terminal, remote session available as a destination while the local tab is closed.

| Time | Operator action | Evidence / optional caption |
| --- | --- | --- |
| 0–2s | Hold on the local prompt. | “Start something that keeps running.” |
| 2–5s | Type `sh progress.sh` and press Enter. | Heartbeats begin at 001. |
| 5–8s | Let several heartbeats appear; note the last visible number. | Establish that this is live output. |
| 8–10s | Close the local tab with Cmd-W; select the remote tab if needed. | “Close the tab. The session stays alive.” |
| 10–13s | Hold on the other session. | The counter continues while detached. |
| 13–17s | Reopen **demo-local** from the session tree. | Retained output includes the missing interval and new heartbeats continue. |
| 17–20s | Hold on the live counter. | “Pick up where you left off.” |

Use **close tab**, not **Terminate session**. The evidence is the same session
with monotonically increasing heartbeat numbers, not a restarted counter. Keep
the detach/return sequence continuous; do not edit out the detached interval.

**Reset:** Reopen the local session, press Ctrl-C to stop only the sample counter,
then run `clear`. The next `sh progress.sh` starts at 001. Restore the initial tabs.

## Demo 2 — Local and remote, together

**Message:** Work across hosts and preview remote files in the same workspace.
**Target length:** 25 seconds. **Starting state:** local tab selected, both sessions
clear, remote connected, VPN already active, preview closed.

| Time | Operator action | Evidence / optional caption |
| --- | --- | --- |
| 0–3s | Hold on **demo-local**, with both host entries visible. | “Local and remote, in one workspace.” |
| 3–6s | Select **demo-remote**. | Remote host/session selection is visible. |
| 6–10s | Type `sh inspect.sh` and press Enter. | A real remote read checks the sample README. |
| 10–14s | Cmd-click the printed `README.md` link. | “Open a remote file straight from your terminal.” |
| 14–20s | Hold on the rendered release notes beside the terminal. | Text is readable; the originating remote session remains visible. |
| 20–23s | Close the preview and switch back to **demo-local**. | Local prompt remains available. |
| 23–25s | Hold. | End on the combined workspace. |

**Reset:** Close the preview, run `clear` in each shell, select the local tab.
Do not include connection setup or authentication in this clip.

## Demo 3 — Connect through VPN

**Message:** Connect a saved VPN and start working on a private host.
**Target length:** 30 seconds, allowing longer real connection time when needed.
**Starting state:** VPN panel open, **Demo VPN** genuinely disconnected, and
the remote host disconnected with no reusable SSH master from rehearsal.

Before each take, disconnect the demo host using its host action, then disconnect
the demo VPN. Confirm the selected host method still points to that VPN. Starting
with an existing SSH transport can hide whether the new VPN route is actually used.

| Time | Operator action | Evidence / optional caption |
| --- | --- | --- |
| 0–3s | Hold on the saved disconnected VPN. | “Your private hosts, within reach.” |
| 3–12s | Click **Connect** and wait for **Connected**. | Show the real connection state transition. |
| 12–16s | Open **Sessions**, select **demo-remote**, and connect if prompted. | The saved host uses the selected VPN route. |
| 16–22s | Once the remote prompt is ready, run `sh inspect.sh`. | A real command succeeds on the private host. |
| 22–27s | Hold on the result with the VPN activity indicator visible. | “Connect. Reach your host. Start working.” |
| 27–30s | Hold on the usable remote shell. | End on the outcome. |

Wait for actual states rather than racing the timing table. Record up to 90 seconds
if needed. The supplied exporter trims only the start/end; it does not remove
connection waits or change playback speed. If a later edit compresses a long wait,
make the cut visible and label it “Connection wait shortened.”

**Reset:** Clear the remote terminal while still connected; disconnect the demo
host, disconnect its VPN, and wait for confirmed disconnected status. If another
daemon retains the container, release that demo client's interest first. Keep
saved credentials and Tailscale identity so subsequent takes avoid sign-in.

## Capture and export

Find the app rectangle once using macOS Screenshot's selection overlay, then
cancel the overlay. Coordinates are screen points, not Retina pixels. Confirm the
rectangle with a short framing take; capture includes anything overlapping it.
The rectangle below is an example: substitute the measured app bounds on your
display. Use the same rectangle for all final takes.

```sh
pnpm demo record persistence --take rehearsal-01 --rect 100,100,1280,800 --seconds 5
pnpm demo record persistence --take take-01 --rect 100,100,1280,800 --seconds 45
pnpm demo record remote --take take-01 --rect 100,100,1280,800 --seconds 45
pnpm demo record vpn --take take-01 --rect 100,100,1280,800 --seconds 90
```

Run one command per take after its reset. There is a five-second countdown to
focus the app before capture starts. Audio is disabled; clicks are shown. If
macOS requests Screen Recording access, grant it to the terminal running the
command, restart it if required, and make a new framing take. Interrupted or
failed takes retain their directory; choose a new take name rather than overwrite.

Open each `source.mov` in `.demo-recordings/<demo>/<take>/`, review it, and choose
trim points relative to that movie. For example, if the action begins at second 2:

```sh
pnpm demo export persistence --take take-01 --start 2 --duration 20
pnpm demo export remote --take take-01 --start 2 --duration 25
pnpm demo export vpn --take take-01 --start 2 --duration 30
```

Exports contain a silent H.264 `demo.mp4` (1280 × 800, 30 fps, fast-start), its
first-frame `poster.png`, and an `edit.json` with trim values. Aspect ratio is
preserved with padding when necessary. Source capture settings are saved in
`capture.json`. Captions in the shot lists are editorial suggestions, not burned
into the videos. Prefer nearby page text for these short silent clips.

For Linux capture, use an operator-controlled recorder with a fixed app rectangle
and no audio. Store the movie as `source.mov` in the same ignored take directory
and supply `capture.json` with `{"capture_seconds": 45}` using the actual recording
length; the export command is platform-independent. Windows native desktop
support is pending, so these scripts target macOS/Linux demo hosts.

All raw and exported media and local capture metadata remain ignored by Git.
Keep approved assets local until a hosting destination is chosen. Keep the
recordings and this shot list together when handing them to the landing-page work.

## Review and cleanup

- Watch each full source and export: no personal account, host, address, path,
  notification, or credential should appear, including inside previews.
- Verify the persistence counter advances across detachment; the remote preview
  comes from the selected remote session; the VPN clip starts disconnected and
  reaches the private host through the configured route.
- Check text at the intended landing-page size, pointer motion, framing, start/end
  pauses, and the poster. Remove failed takes from the final asset selection.
- In the future page, use descriptive adjacent text and a play/pause control.
  Respect reduced motion by showing the poster until the viewer presses play.
- After filming, stop the sample counter with Ctrl-C and explicitly terminate only
  the two named demo sessions when finished. Disconnect only the demo host/VPN.
  Remove only the temporary directories printed during setup after their sessions
  have ended. Preparation does not register an automatic cleanup job.
