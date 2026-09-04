# Proposal 0004: Windows SSH gateways

- Status: Implemented
- Created: 2026-09-04

## Summary

Extend Proposal 0002 to Windows clients and Windows SSH hosts while preserving
its routing, authentication, and process ownership boundaries.

## Motivation

The Windows rmux daemon already owns ConPTY sessions. Users need to reach those
sessions through SSH and keep them alive when a remote connection closes.

## Design

`ctl --host HOST --remote-platform windows rmux ...` selects the fixed
`ctl-agent.exe connect` command. The default Unix platform continues to select
`exec ctl-agent connect`, regardless of the client's operating system. Windows hosts
currently require OpenSSH's default cmd.exe shell.

`ctl-agent` relays binary stdio to the authenticated user's fixed rmux named pipe.
If absent, it starts the absolute-path companion `rmuxd.exe`, detached from the
console and with job breakaway so OpenSSH channel cleanup cannot kill it.
Failure to obtain permitted breakaway is an explicit startup error.

## Invariants

1. OpenSSH owns authentication and host verification; no forwarding is enabled.
2. Platform selection accepts an enum, never an arbitrary remote command.
3. rmuxd owns every process and PTY in its sessions; ctl-agent owns only the relay.
4. SSH disconnect must not terminate a daemon-owned session.
5. The remote gateway exposes only the fixed data endpoint, never maintenance.
6. Local Windows routing continues to use owner-restricted named pipes.

## Out of scope

PowerShell/custom SSH shells, desktop remote-platform selection, remote task
routing, and Windows shell metadata.

## Unresolved questions

None for the CLI gateway boundary. Native CI verifies authenticated loopback
SSH through the Windows service, auto-start, attachment, disconnect/reconnect,
resize, final output, exit status, and CLI routing. Different-machine network
setups and custom SSH shells require additional validation.

## Detailed specifications

- [ctl SSH transport](../ctl-protocol.md)
- [Windows implementation and CI](../windows-ci.md)
