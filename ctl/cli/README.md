# ctl-cli

Installs the `ctl` command for local and remote terminal sessions, managed tasks,
SSH, file copies, port forwards, and VPN connections.

## Select a target

Use `-H`/`--host NAME_OR_ID` for a saved ctl host. Unknown names fail with a
hint to use `--destination`; ambiguous names require the stable ID.
Place `--destination DESTINATION` before the subcommand for a temporary SSH
connection to an IPv4/IPv6 address, DNS name, or SSH-config alias, optionally
prefixed with `user@`:

```sh
ctl --destination 192.168.1.1 shell
ctl --destination user@mymachine.example.com exec -- uname -a
ctl --destination my-ssh-alias ctmux list
```

Temporary destinations use OpenSSH configuration, bypass the saved host catalog
even when names match, and do not save a host definition. `--destination`
conflicts with `--host` and `--method`; `--method` chooses a saved host's
connection method. `--remote-platform` works with either target selector.
Unix clients connecting to Windows destinations retain SSH endpoint configuration
and use a private master because configured master startup requires a POSIX shell.
Windows clients use direct SSH for remote commands. Their local `port` commands
use ctld-owned listeners and separate native SSH connections, preserving system
SSH configuration and requiring authentication already available without prompts.
The selectors apply to shells, execution, ctmux, registered tasks, managed ports,
remote VPN operations, and remote component maintenance. Omit both selectors
for local operations where supported. `ssh` and `scp` take their destinations
positionally and retain saved-host-first lookup followed by ordinary OpenSSH
lookup; they reject both ctl target selectors.

## Caller-owned SSH credentials

Applications can select an explicit endpoint and credential for temporary
`shell` and `exec` calls. Put these options before the subcommand:

```sh
ctl --destination operator@192.0.2.10 --port 2222 --identity-file ./instance-key exec -- uname -a
ctl --destination 'operator@::1' --port 2222 --identity-file ./instance-key shell --plain
```

`--user` and `--port` override the temporary endpoint. Without an explicit
credential, authentication retains the ordinary ctl-managed policy. Supplying
`--identity-file`, `--password-fd`, `--password-socket`, or `--password-pipe` instead selects a fresh,
caller-owned connection: no existing master, configured identities, identity
agent, alternative authentication method, saved credential or Keychain fallback
can rescue an incorrect supplied credential. Credential sources conflict.
The private key must be usable without an agent or a passphrase prompt. ctl does
not offer passphrase or PIN prompts, or retry with a different credential.

ctl resolves the SSH alias's endpoint and host-key settings, then uses an isolated
OpenSSH configuration. Host-key verification and known-hosts policy remain in
effect. When OpenSSH requires first-use confirmation, ctl displays its full host
authenticity and fingerprint prompt on the caller's controlling terminal and
reads the confirmation there: `/dev/tty` on Unix, or `CONIN$`/`CONOUT$` on Windows.
This works independently of redirected command stdin. The supplied password is
never used to answer a host-trust prompt. OpenSSH records an accepted key in its
configured known-hosts files as usual; changed-key, revoked-key, and strict
host-key policies retain OpenSSH's normal behavior.

Without a controlling terminal, a connection that needs host-trust confirmation
fails without prompting. For unattended use, provision trusted host keys in the
configured known-hosts files beforehand. Already trusted hosts and configured
noninteractive host-key policies continue to work. Host-trust confirmation is
separate from credential delivery: only that first-use trust prompt can read the
terminal; passwords use the supplied credential channel, and command stdin
remains available for application data.
Background channels cannot prompt while ctl's terminal UI owns input. An explicit
reconnect can ask for confirmation after the UI releases the terminal.

Default known-hosts paths retain profile names containing spaces. Ambiguous
custom known-hosts path lists are rejected instead of changing which files are trusted.
Configured proxy authentication is rejected in this mode; use a directly
reachable destination. This mode neither creates saved hosts nor edits SSH
configuration. Persistent `shell` sessions still require the remote ctl-agent
and ctmuxd; `shell --plain` and `exec` need only SSH.

On Unix, pass `--password-fd N` with an inherited readable **pipe or socket** at
FD N >= 3. File descriptors 0, 1 and 2 remain command stdin/stdout/stderr.
The parent writes exactly one password and closes its writing end. ctl claims
and closes the inherited descriptor before starting any subprocess, reads a
bounded payload until EOF, and rejects a missing EOF after five seconds. Regular
files are rejected. Example using Node's process API (Bun callers can use the
socket alternative below without depending on extra-FD stream ownership):

```ts
import { spawn } from "node:child_process";
import type { Writable } from "node:stream";

function execute(password: string, command_input: Uint8Array) {
  const child = spawn("ctl", ["--destination", "operator@192.0.2.10",
    "--password-fd", "3", "exec", "--", "cat"], {
    stdio: ["pipe", "inherit", "inherit", "pipe"],
  });
  (child.stdio[3] as Writable).end(Buffer.from(password, "utf8"));
  child.stdin!.end(command_input);
  return child;
}
```

For Bun or other socket-oriented Unix callers, use `--password-socket PATH`.
The caller creates a short, private directory with mode 0700; ctl verifies its
ownership and permissions, creates the Unix socket, and removes only its own
socket on completion or failure. The parent connects with `node:net`, sends the
password and closes the connection. It owns cleanup of the empty directory.
This is a socket endpoint, not a file containing the credential. Standard stdin
remains available throughout. Existing endpoints and public/symlink directories
are rejected.

On Windows, use `--password-pipe \\.\pipe\ctl-password-UNIQUE_NAME` instead.
ctl creates an exclusive local-only pipe with an owner-only DACL and
non-inheritable handles. The parent connects using `node:net` (also supported by
Bun), writes the password and closes the connection. Connect within five
seconds; transient not-found errors while ctl starts can be retried. Numeric
Unix descriptors are explicitly rejected on Windows. See the Bun handoff
fixture in `tests/fixtures/caller_handoff.mts` for both platform transports.

Passwords are 1..4096 UTF-8 bytes without NUL, CR or LF. Leading/trailing spaces
are preserved; no trimming or newline terminator is supported. Never put a
password in argv, environment variables or a file. ctl's internal askpass bridge
delivers the supplied password without a terminal password prompt. The password
buffer exists only in memory for this invocation, including persistent-shell
reconnections, and is zeroized when the invocation ends. It is never returned to
the caller, logged, audited, or saved. Terminal interaction, command input/output
and exit status remain independent of the password channel.

## Persistent shells

```sh
ctl -H work shell                         # create a new persistent shell
ctl -H work shell --resume                # choose existing work; create if empty
ctl -H work ctmux --resume                # the same picker through ctmux
ctl -H work shell --session development   # attach by name, or create that name
ctl -H work ctmux rename-session -t development build
```

Inside the shell, **Ctrl+B s/w** or `:list-sessions` opens the session manager.
It can open, create, rename, or terminate sessions on the selected host.
For a new session, leave the name empty and press Enter for an automatic name.
Termination confirms the exact session and affects all its panes and viewers;
**Ctrl+B d** only detaches this client. Renaming requires ctmux 1.1.20.
`--resume` opens the manager without attaching or creating when sessions exist.
`ctl shell --resume` cannot be combined with `--session`, `--plain`, or `--cwd`.
`ctl ctmux --resume` cannot be combined with another ctmux subcommand.

Shell startup lets SSH authentication and gateway/VPN preparation finish under
their existing connection policy. Each ctmux exchange then gets its own
five-second deadline; initial pane checkpoints and lease confirmation have a
separate deadline. Quiet runtime connections remain bounded. Timeout messages
identify the endpoint, operation, and phase instead of only saying
`deadline has elapsed`.

An inconclusive SSH control-master check (`ssh_status_unknown`), including a
control-command timeout, keeps background checks and pane recovery retrying.
These checks remain passive: uncertainty does not start authentication, prepare
a VPN, or replace a master. Authentication, invalid configuration, and explicit
disconnects still suspend recovery until the corresponding user action.
For a disconnected pane, the footer offers **Enter to reconnect**. This retries
the existing session and permits SSH authentication, including Touch ID when
required by ctld. The TUI temporarily releases the host terminal for password,
confirmation, and gateway/VPN preparation, then restores the same session.
Rejected or canceled authentication returns to the disconnected TUI until the
next explicit retry. Background recovery stays quiet. Reconnecting never creates
a replacement shell.

## Install

Once the packages are published:

```sh
# macOS: install the matching signed helper from its published GitHub release.
cargo install --locked ctl-cli ctmuxd ctl-taskd
ctl setup

# Other Unix and Windows clients.
cargo install --locked ctl-cli ctld ctmuxd ctl-taskd
```

Cargo installs only the selected packages' executables. `ctld`, `ctmuxd`, and
`ctl-taskd` are companion daemons and must be installed separately from `ctl-cli`.
Keep terminal/task companion versions aligned with the CLI. These daemons should
share the CLI's binary directory; `ctl setup` installs the macOS helper under
`~/.tokn/ctl/components/ctld/` after verifying the complete signed app bundle.
A downloaded official release updates `current`; caching a selected checkout or
embedded app leaves shared selections unchanged. Setup never restarts a daemon.

For remote sessions and tasks, the controlled machine needs `ctl-agent`,
`ctmuxd`, and `ctl-taskd`; remote VPNs also need `ctld`. Interactive Unix connections
offer to repair missing
or recognized older agents with a matching verified bundle, then retry once.
Repair checks any saved machine ID first, shows upload progress and speed, and
preserves running daemons. Ctrl-C cancels it; piped commands never prompt.
Bundles can come from
`CTL_REMOTE_BUNDLES_DIR`, local resources, `~/.tokn/ctl/agent-bundles`, the matching
official release, or an existing verified GitHub bundle artifact (requires `gh`).
Downloaded manifests and archives are cached at
`~/.tokn/ctl/agent-bundles/<revision>/<target>/` and verified again before reuse.
Schema-2 bundles include all four components and record their product/source identity and explicit
protocol support. Compatible local or cached bundles can be reused across CLI
releases and source revisions, including by development clients. Reuse checks
all service and companion contracts, the target, archive and binary checksums,
and agreement between the bundle-set and archived metadata. The exact current
source cache is checked first, followed by compatible entries in stable revision
order; this order does not imply which build is newest. Schema-1 bundles remain
eligible only for the exact clean client build. Downloads still require a clean,
identified CLI and match that source revision. Compatible cache entries work
offline and can serve any host with that platform.
Interrupted downloads never publish a partial entry; damaged cache entries are
downloaded again. Explicit bundle overrides still reject invalid contents.
Source developers can prepare their exact bundles with `pnpm bundles:sync` from
`apps/desktop`. Windows remote companions must be installed manually. On macOS,
Touch ID-protected credential storage
requires the signed, provisioned `ctld` helper; Cargo installation alone does
not supply its Keychain entitlement. Setup uses the same selected helper as
status, startup, and restart. With no eligible local, shared, or bundled helper,
macOS setup downloads the release matching this CLI's version and architecture;
that release must be published.
`CTLD_BIN` remains an explicit override. Ordinary macOS debug builds first prefer
the provisioned signed helper for their checkout, and signed development CLI
builds prefer their matching embedded helper. Shared discovery then prefers
a verified compatible shared `ctld.app`, then its own bundled helper, then a
nearby desktop bundle. Without an eligible signed app, macOS selects the official
release for explicit setup; it does not implicitly choose an unsigned sibling
or `PATH` executable. Use `CTLD_BIN` to explicitly choose a manually managed
helper. Other Unix platforms and Windows retain sibling/`PATH` discovery. Packaged desktops
prefer their own bundled helper after any explicit complete-bundle selection.
Shared apps must match the native architecture and advertise a shared ctld,
lifecycle, and helper contract. Remote VPN routes require
ctld `1.1.13` and helper `1.1.2`. Their build identity is checked
against their own installation manifest; their release version, commit, and
fingerprint need not equal the CLI's.

Official macOS CLI downloads carry the complete matching signed `ctld.app`
inside the `ctl` executable. They install it under the same managed directory
when a command needs the helper and no compatible shared app is selected, or
when `ctl setup` selects that embedded app, without downloading it. Running
compatible daemons and passive status reads are preserved. A dedicated
[build/release command](https://github.com/tokn-ai/ctl/blob/main/docs/ci-bundles.md#build-a-cli-with-ctld-embedded)
compiles and bundles the helper; ordinary Cargo installation does not embed it.

For local macOS development, use the shared Xcode provisioning flow from a checkout:

```sh
pnpm ctld:provision
# Choose your team in Xcode and build the provisioning target once.
pnpm ctld:build
cargo run -p ctl-cli -- passwords
```

The command discovers and refreshes the provisioning profile, signs `ctld.app`,
and publishes it under Cargo's configured target directory. An unsigned
`target/debug/ctl` pins and verifies that helper in the immutable
`~/.tokn/ctl/components/ctld/development/<archive-sha256>/` cache before launch.
Cleaning Cargo outputs does not remove the running daemon's executable or the
helper used by its askpass, identity-agent, and proxy children.
Its selection belongs to the checkout, including with a shared target directory,
and does not depend on the current working directory. Repeat the command after
helper changes or profile expiry; CLI edits need only a normal Cargo rebuild.
No notarization credentials are needed, and shared selections stay unchanged.

Embedded development helpers use the same immutable cache. `ctl setup` prepares
the helper selected by the shared resolver, including checkout helpers and
`CTLD_BIN` overrides; it does not replace that selection with a published release.
Status inspects the same candidate without preparing it. It shows the source,
path, and archive hash separately from the running build; `--json` includes
`available_selection`. `ctl setup --json` retains its installation fields and
adds `selection` with the same identity. The archive hash identifies signed app
bytes, the source fingerprint identifies compiled inputs, and the checkout ID
identifies a path. None proves a helper was rebuilt after source edits.

Preparing a development helper leaves release `current` unchanged. Setup preserves
all running owners; existing daemons require an explicit restart to use the
selected replacement.

Use `ctl components update` for an explicit update. It defaults to this computer;
`--hosts work,jump-a` updates several saved hosts through their preferred routes,
and `--local` includes this computer in that batch. `--package ctl-agent` retains
the installed daemons; the default `--package full-bundle` installs all four
components from one compatible complete build. Updates preserve running services
and sessions; restart or reconnect separately.

```sh
ctl components update --hosts work,jump-a --package ctl-agent
ctl --host work --method vpn components update
ctl --destination my-ssh-alias components update
ctl components update --hosts work --from /path/to/bundle-set --json
```

`--from` supplies a complete build directory or archive. Use `--local-build` for
four native binaries; local macOS full updates also require `--ctld-package`.
Otherwise updates use the pinned selection for each target. `components list`
shows included and stored complete builds across all targets, with availability
and local/upload selections. Use `--target <triple>` to filter or `--json` for
manifests, availability, selections, and inspection errors. Listing never imports
or selects a build. `components sync` imports a build; `components select` can
choose a listed stored or included build. See the
[component update guide](https://github.com/tokn-ai/ctl/blob/main/docs/component-updates.md)
for source formats, cancellation, and per-host results.

VPN profiles remain in the local catalog. Use `ctl --host GATEWAY vpn start NAME_OR_ID`,
`vpn list`, and `vpn stop NAME_OR_ID` to run and manage a profile on an SSH host.
Use `--destination SSH_DESTINATION` instead of `--host` for a temporary gateway.
The gateway needs updated ctl-agent/ctld components and Docker or Podman. Its
VPN SOCKS5 listener stays on remote loopback; traffic travels through SSH.
Remote VPNs keep running until explicitly stopped, including after SSH disconnects.
Profile creation and removal remain local operations.

An ordered saved route can include `--gateway JUMP_ID --gateway vpn:PROFILE_ID`.
The VPN executes on the preceding SSH gateway. A first VPN step executes locally;
the existing `--vpn PROFILE_ID` option still selects a local VPN before the route.

## Use

```sh
ctl --help
ctl host create
ctl host list
ctl passwords
ctl passwords show ID
ctl passwords remove ID
ctl passwords clear
ctl vpn create
ctl vpn list
ctl vpn start NAME_OR_ID
ctl vpn stop NAME_OR_ID
ctl skill
ctl skill --list
```

`host create` opens a questionnaire and saves a host definition without
connecting or saving credentials. For scripts, use
`ctl host create NAME DESTINATION` with connection flags. With `--json`, prompts
use stderr and the saved host JSON is written to stdout. `host method add` adds an alternate
connection method to an existing host.

`passwords` (or `passwords list`) lists locally saved SSH passwords, legacy SSH
credentials, and identity-file passphrases through the selected signed helper.
`show` displays metadata only. Human-readable tables use compact `p-` and `k-`
IDs alongside names, accounts, targets or key paths, and state. Use the printed
ID, a full ID from JSON, a unique full-ID prefix, or an exact unique name with
`show` and `remove`. `--json` is available for all four actions and retains full
IDs. Listing and showing discover owned items directly from Keychain attributes
without reading secret values or starting a daemon. Keychain can request
authentication to complete discovery. Entries with missing or malformed metadata
remain visible with an `unknown` state; `show` explains what could not be checked.
Discovery completeness is separate from metadata quality. A failed scan reports
its specific reason, and an empty partial list does not claim no credentials are
saved. No metadata import command is needed.

`passwords remove` opens a picker when its selector is omitted and removes one
saved secret after interactive confirmation. `passwords clear` previews discovered
entries and, after confirmation, clears all owned SSH credentials and identity
passphrases, including legacy copies. All password commands require ctld helper
contract `1.1.4` for authoritative discovery; update the selected signed helper
if an older helper rejects it. Both removal actions require an interactive
terminal and have no `--yes` bypass. They retain private key files, hosts, VPN profiles, running
connections, and never-save preferences. These commands manage the local macOS
Keychain, so omit `--host`, `--destination`, `--method`, and `--remote-platform`.

`vpn create` opens a questionnaire and saves an OpenConnect or Tailscale profile
without starting a daemon or container. Password input is masked. `vpn list`
combines saved profiles from `~/.tokn/ctl/vpns.json` (or
`CTL_VPNS_PATH`) with runtime connections and their current SOCKS5 endpoints. It
never starts a daemon. Saved profiles stay visible as disconnected when inventory
is complete, or unavailable when it cannot be checked. `vpn start` connects a
saved profile and recreates its container when missing. Supply its exact stable
ID or unique exact name. `vpn stop` also accepts runtime VPN IDs. Omitting the
start selector opens a picker in an interactive terminal; stop does the same
with a current daemon. An older daemon uses its untargeted single-connection stop
directly. Scripts must supply a start selector; an untargeted stop requires at most one local
connection. All five commands support `--json`; create remains interactive and
returns saved metadata, while list returns a snapshot
with sanitized merged `entries`, raw runtime `connections`, and inventory warnings.
Older daemons can report their local connection, but unmatched saved profiles
remain unavailable because shared-container inventory cannot be verified.

Use `vpn remove NAME_OR_ID` to delete a saved profile. Omitting its selector
opens a profile picker. Removal always requires interactive confirmation,
defaulting to No, and retains Tailscale identity volumes. There is no `--yes`
bypass. After confirmation it requires complete runtime inventory and a stopped
VPN; stop it and wait for container exit first. Remove never starts a daemon or
stops a VPN automatically.

The bundled skills and references are available without a daemon, connection,
or source checkout.

Part of [ctl and ctmux](https://github.com/tokn-ai/ctl). See the
[repository documentation](https://github.com/tokn-ai/ctl#readme) for setup,
platform support, and usage.

Licensed under MIT.
