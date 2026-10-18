---
name: ctl
description: "Choose a ctl workflow, run one-off local or SSH commands, copy files, or troubleshoot ctl setup. Route saved-host, persistent-session, task, port-forward, and VPN operations to their focused ctl skills."
---

# ctl

Use the `ctl` CLI to operate local or SSH-authorized work. This is the parent
skill: choose the operation here, then read only the relevant child skill.
Each child also supports direct invocation independently.

Read the bundled parent with `ctl skill`; retrieve only the selected child or
reference using the commands below. `ctl skill --list` lists bundled name/file
pairs. `--file PATH` selects a file within the chosen skill; its default is
`SKILL.md`. Short names `host`, `session`, `task`, `port`, and `vpn` alias their
`ctl-` names. Reading needs only the binary: no checkout, daemon, configuration,
or authentication. `-H`, `--destination`, `--method`, and `--remote-platform`
are rejected for `skill`; it always reads the local binary's bundled documents.

## Choose the operation

| Need | Command or child skill |
| --- | --- |
| Install the signed macOS connection helper for this CLI version | `ctl setup`; [setup reference](references/setup.md) |
| Run once, stream output, preserve exit status | `ctl exec -- PROGRAM ARGS...`; guidance below |
| Ordinary SSH login or file copy | `ctl ssh` / `ctl scp`; guidance below |
| Inspect or edit saved hosts and connection methods | [ctl-host](../ctl-host/SKILL.md), `ctl skill ctl-host` |
| Inspect or remove locally saved SSH passwords and key passphrases | `ctl passwords`; guidance below |
| Create, attach, inspect, split, or terminate persistent terminals | [ctl-session](../ctl-session/SKILL.md), `ctl skill ctl-session` |
| Manage named background/interactive processes or save reusable recipes | [ctl-task](../ctl-task/SKILL.md), `ctl skill ctl-task` |
| Expose a remote TCP service on local loopback | [ctl-port](../ctl-port/SKILL.md), `ctl skill ctl-port` |
| Manage OpenConnect or Tailscale containers locally or over SSH | [ctl-vpn](../ctl-vpn/SKILL.md), `ctl skill ctl-vpn` |

## Target and command discovery

Without `-H`/`--host` or `--destination`, native execution, sessions, and
registered tasks target the local machine. For remote work, use `ctl -H HOST ...`
for a saved host by stable ID or name, or `ctl --destination DESTINATION ...`
for a temporary SSH connection. Put `--destination` before the subcommand.
It accepts an IPv4/IPv6 address, DNS name, or SSH-config alias with an optional
`user@` prefix, uses OpenSSH configuration, and bypasses the saved catalog even
when names match. It does not save a host definition. Unknown `--host` values
fail with a hint to use `--destination`; ambiguous saved names fail, so use the
ID. Keep the same target selector on every operation.
`--method NAME_OR_ID` selects a saved host's connection method; otherwise its
preferred method is used. A failed selected route does not choose another route.
`--destination` conflicts with `--host` and `--method`. `--remote-platform`
works with either target selector.

For application-owned authentication, temporary `shell`/`exec` targets also accept
`--user`, `--port`, and exactly one of `--identity-file`, `--password-fd` (Unix),
`--password-socket` (Unix), or `--password-pipe` (Windows), before the subcommand. An explicit credential
requires a fresh connection with only that credential; it bypasses ctld,
Keychain, saved credentials, masters, configured keys and identity agents.
Without a credential, the ordinary managed authentication policy is unchanged.
Host-key verification remains enabled. The key must work without an agent or
interactive passphrase; proxy authentication is not supported in this mode.
Never put a password in arguments, environment variables or a file. A calling
application must send it over the separate inherited pipe/socket FD (N >= 3),
or connect to ctl's owner-only `\\.\pipe\ctl-password-UNIQUE` Windows endpoint.
For Unix socket handoff, create an owned mode-0700 directory and pass a short
socket path within it; ctl creates and cleans up the endpoint. This transport
works with Bun's `node:net` without inherited-FD ownership dependencies.
Write 1..4096 UTF-8 bytes with no NUL, CR or LF, preserving spaces, then close
the writer within five seconds. Standard input remains remote command input.
Credential-source conflicts are errors. Do not create a saved host or rewrite
SSH config to perform caller-owned temporary operations.

Host catalog commands take a positional host and reject both target selectors.
Their own `host update --destination` and `host method update --destination`
options edit a saved endpoint. SSH/SCP take their destination positionally and
also reject the target selectors. `ctl setup`, `ctl passwords`, and
`ctl taskd restart` are local. VPN list/start/stop and remote component maintenance
accept either selector; VPN profile create/remove remain local.

Check `ctl --version`, `ctl --help`, and the relevant native subcommand's
`--help` when availability or flags are uncertain. There is no global `--json`.
Read [setup and troubleshooting](references/setup.md) with
`ctl skill --file references/setup.md` when ctl, a helper, authentication, or a
service protocol is unavailable. If ctl is unavailable, use the linked file.
Do not invent commands for features described only in proposals.

## Saved SSH passwords

On macOS, `ctl passwords` or `ctl passwords list` lists saved SSH credentials and
identity-file passphrases directly from Keychain. `ctl passwords show ID` shows
metadata only; neither command reads secret values. Listing may request Keychain
authorization and automatically refreshes the metadata cache. Unknown metadata
remains visible, and access or inventory-limit failures are explicit.
Human-readable tables use compact `p-` and `k-` IDs
with names, accounts, targets or key paths, and state. Use the printed ID with
`show` or `remove`; `--json` retains full IDs for machine-readable metadata. An
incomplete discovery is reported explicitly; cache completion markers do not
determine which saved entries are listed.

`ctl passwords remove ID` removes one saved entry after terminal confirmation;
omit the ID to choose an entry interactively. A full ID from JSON, a unique
full-ID prefix, or an exact unique name also works. `ctl passwords clear` previews
known entries and clears all owned SSH credentials and passphrases after confirmation,
including unindexed older copies. Listing/show require helper contract `1.1.4`;
remove/clear require revocation-aware `1.1.5`. Both
actions preserve key files, host/VPN settings, and never-save preferences, and
require an interactive terminal. These commands have no create, update, import,
reveal, or copy action; omit `-H`, `--destination`, `--method`, and `--remote-platform`.

## One-off execution

```sh
ctl exec -- uname -a
ctl -H work exec -- uname -a
ctl -H work exec -- sh -c 'cd /srv/app && cargo test >test.log 2>&1'
ctl --destination user@mymachine.example.com exec -- uname -a
```

`exec` streams stdin/stdout/stderr, allocates no remote PTY, and preserves the
executed command's exit status. Local execution launches argv directly; Unix
remote execution quotes each argument individually. Use an explicit shell for
variables, pipelines, redirection, globbing, or changing the remote directory.
Keep remote shell expressions quoted from the local shell. Prefer argv when
passing data; do not interpolate untrusted values into shell source.

`ctl shell --plain` opens an ordinary local or SSH shell. For a persistent
terminal use [ctl-session](../ctl-session/SKILL.md), `ctl skill ctl-session`;
for managed lifecycle and logs use [ctl-task](../ctl-task/SKILL.md),
`ctl skill ctl-task`.

## OpenSSH login and file copy

```sh
ctl ssh work
ctl ssh work 'uname -a'
ctl scp ./file.txt work:/tmp/
ctl scp work:/tmp/file.txt ./
ctl --method VPN ssh work
ctl --method VPN scp ./file.txt work:/tmp/
ctl ssh -G work
```

Put ctl-specific flags before `ssh` or `scp`; all arguments after the subcommand
belong to the system OpenSSH program, including its help options. Do not use
`-H` or ctl's `--destination` with these compatibility commands. `user@work`
explicitly overrides the saved account. Unknown destinations retain normal
OpenSSH lookup and semantics.

Compatible Unix saved-host operations reuse the exact ctld-authenticated
connection, including its selected VPN/SOCKS route. Explicit connection/config
options such as SSH `-i`, `-p`, `-F`, `-J`, `-S`, or transport-related `-o`
options use OpenSSH directly with saved defaults. Explicit proxy options
override the saved route. An explicit SCP `-S` retains the user's transport.
`ctl ssh -G HOST` resolves settings without SSH authentication or VPN startup.

Plain shells, exec, SSH, and SCP need only the remote SSH service. Persistent
sessions and registered tasks need remote ctl companions; the CLI does not
automatically install them. A restricted service-only SSH account may support
ctmux/tasks while rejecting ordinary commands, file transfers, or forwarding.
