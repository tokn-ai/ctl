# Components and updates

Open **About ctmux** to inspect components on this computer and saved remote
hosts. **Running** identifies the existing process; **Installed** identifies the
selected executable. Build IDs distinguish development binaries that share a
product version. Different builds do not establish which one is newer.

Refresh reuses existing authenticated SSH connections and checks service owners
without attaching a terminal, starting a daemon or VPN, or installing files.
An offline saved host stays visible as **Not checked**, with unknown running
versions. Saved identity metadata is never presented as a live process version.
A legacy numeric protocol is labeled **legacy**, independently of published
contract versions.

For a saved remote host:

1. Use **Check host** to authenticate through its preferred connection method
   and route, then refresh component status. This does not open a terminal.
2. Use **Update…** to install the verified remote bundle for this account. ctl
   verifies the saved account before uploading, activates an immutable bundle,
   and verifies that the installed agent reports the selected bundle afterward.
   A compatible cached development bundle may differ from the desktop's source
   revision; the result describes the bundle actually installed.
3. If **Restart required** appears, choose **Restart** on the terminal daemon.
   ctl prepares the installed replacement and reports the impact before asking
   for confirmation. Restart ends every terminal owned by that remote account,
   including terminals in other windows and clients. Canceling keeps sessions.

Installation preserves running daemons and sessions. Existing SSH channels keep
an older agent process until **Reconnect** applies the installed agent; remote
terminal sessions survive that reconnect. The inspection agent itself is an
on-demand process, not evidence that existing channels were updated.

For this computer, update the desktop application or rebuild its local helpers,
then use the separate confirmed restart actions. Remote broker and task-daemon
status is displayed alongside the terminal daemon; their remote restart actions
are not exposed. A legacy owner without cooperative restart support requires a
manual restart after its work can be ended.

If activation cannot be verified, ctl reports that installation may have
completed and asks you to check the host. It does not retry a destructive action
or stop a daemon automatically.
