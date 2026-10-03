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
or authentication. `-H`, `--method`, and `--remote-platform` are rejected for
`skill`; it always reads the local binary's bundled documents.

## Choose the operation

| Need | Command or child skill |
| --- | --- |
| Install the signed macOS connection helper for this CLI version | `ctl setup`; [setup reference](references/setup.md) |
| Run once, stream output, preserve exit status | `ctl exec -- PROGRAM ARGS...`; guidance below |
| Ordinary SSH login or file copy | `ctl ssh` / `ctl scp`; guidance below |
| Inspect or edit saved hosts and connection methods | [ctl-host](../ctl-host/SKILL.md), `ctl skill ctl-host` |
| Create, attach, inspect, split, or terminate persistent terminals | [ctl-session](../ctl-session/SKILL.md), `ctl skill ctl-session` |
| Manage named background/interactive processes or save reusable recipes | [ctl-task](../ctl-task/SKILL.md), `ctl skill ctl-task` |
| Expose a remote TCP service on local loopback | [ctl-port](../ctl-port/SKILL.md), `ctl skill ctl-port` |
| Manage OpenConnect or Tailscale containers locally or over SSH | [ctl-vpn](../ctl-vpn/SKILL.md), `ctl skill ctl-vpn` |

## Target and command discovery

Without `-H`/`--host`, native execution, sessions, and registered tasks target
the local machine. For remote work, use `ctl -H HOST ...` consistently on every
operation. HOST first selects a saved host by stable ID or name, then falls
back to an OpenSSH alias or destination. Ambiguous saved names fail; use the ID.
`--method NAME_OR_ID` selects a saved host's connection method; otherwise its
preferred method is used. A failed selected route does not choose another route.

Host catalog commands take a positional host and reject `-H`. SSH/SCP also take
their destination positionally. `ctl setup` and `ctl taskd restart` are local.
VPN list/start/stop also accept `-H`; VPN profile create/remove remain local.

Check `ctl --version`, `ctl --help`, and the relevant native subcommand's
`--help` when availability or flags are uncertain. There is no global `--json`.
Read [setup and troubleshooting](references/setup.md) with
`ctl skill --file references/setup.md` when ctl, a helper, authentication, or a
service protocol is unavailable. If ctl is unavailable, use the linked file.
Do not invent commands for features described only in proposals.

## One-off execution

```sh
ctl exec -- uname -a
ctl -H work exec -- uname -a
ctl -H work exec -- sh -c 'cd /srv/app && cargo test >test.log 2>&1'
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
`-H` with these compatibility commands. `user@work` explicitly overrides the
saved account. Unknown destinations retain normal OpenSSH lookup and semantics.

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
