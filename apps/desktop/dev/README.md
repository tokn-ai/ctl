# Browser visual preview

From `apps/desktop`, run `pnpm exec vite --host 127.0.0.1` and open
`http://127.0.0.1:1430/preview.html`.

This renders the real application and xterm components with a sample workspace
using the official Tauri IPC and window mocks. It creates no shell processes,
SSH connections, port forwards, or files. Changes only affect memory and reset
on reload. Terminal typing echoes input; it does not execute commands.

Use `?view=tasks`, `?view=ports`, or `?view=vpn` to open those sidebar views directly.
The VPN preview uses sample connections and simulated status; it never starts a container
or saves credentials to disk. Add `&vpn=connected` for a saved connection or
`&vpn=external` for a CLI connection, including when previewing another sidebar
view to check the VPN activity indicator. Use `&vpn=tailscale-sign-in` for browser
sign-in pending, `&vpn=tailscale-connected` for an authenticated tailnet, or
`&vpn=legacy` to inspect provider capability warnings. The preview's **Sign in**
button simulates a successful login in memory without opening a browser or
contacting Tailscale. Disconnect and reconnect keep that simulated identity.

Add a new Tailscale connection to preview enrollment before saving. It starts,
simulates browser sign-in, displays the sample account and tailnet, and only
adds the profile to the list after **Save connection**. The optional `enrollment`
query parameter holds useful states: `starting`, `waiting`, `browser-error`, or
`failure`. For example, `?view=vpn&enrollment=waiting` keeps the browser sign-in
step visible, with an **Open browser** retry. All enrollment state is in memory;
Cancel removes the draft and never touches a real container or account.
The Tasks variant also selects the sample web-development task and its output.
The normal `index.html` entry and production build do not import these fixtures.

Open **About rmux** from the bottom info button or command palette to inspect
sample component versions. The page uses fictional observations and its
**Restart ctld** action only changes memory. Add `?about=partial` to show one
unavailable component while other versions remain visible, or
`?about=restart-error` to exercise a failed restart and subsequent refresh.
Use `?vpn=multiple` to include active VPNs in the restart confirmation.

The preview covers navigation, dialogs, task editing, lease controls, tab
switching, and port toggles. Remote installation and real connection recovery
require the native app. Sample command output demonstrates the visual layout;
it is not a validation result from this checkout.
