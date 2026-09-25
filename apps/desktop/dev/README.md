# Browser visual preview

From `apps/desktop`, run `pnpm exec vite --host 127.0.0.1` and open
`http://127.0.0.1:1430/preview.html`.

This renders the real application and xterm components with a sample workspace
using the official Tauri IPC and window mocks. It creates no shell processes,
SSH connections, port forwards, or files. Changes only affect memory and reset
on reload. Terminal typing echoes input; it does not execute commands.

Use `?view=tasks` or `?view=ports` to open those sidebar views directly.
The Tasks variant also selects the sample web-development task and its output.
The normal `index.html` entry and production build do not import these fixtures.

The preview covers navigation, dialogs, task editing, lease controls, tab
switching, and port toggles. Remote installation and real connection recovery
require the native app. Sample command output demonstrates the visual layout;
it is not a validation result from this checkout.
