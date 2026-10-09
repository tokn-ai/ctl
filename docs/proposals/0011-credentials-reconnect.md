# Proposal 0011: Credential inventory and scoped reconnect approval

- Status: Implemented
- Created: 2026-10-09
- Implementation PRs: [#47](https://github.com/tokn-ai/ctl/pull/47),
  [#53](https://github.com/tokn-ai/ctl/pull/53),
  [#77](https://github.com/tokn-ai/ctl/pull/77),
  [#82](https://github.com/tokn-ai/ctl/pull/82),
  [#88](https://github.com/tokn-ai/ctl/pull/88)

## Summary

Desktop Credentials and `ctl passwords` inspect owned credential metadata
without revealing secrets. Signed macOS helpers verify identity passphrases and
perform explicit secret mutations. Successful SSH authentication can retain
scoped Keychain authorization for bounded reconnects without caching secrets.

## Motivation

Users need to inspect and remove saved credentials, understand changed key files,
and reconnect after network loss without repeated authorization prompts. These
features must preserve the difference between metadata, authorization, and
actual credential access.

## Design

SSH passwords and identity passphrases use the shared signed-helper Keychain
namespace. Inventory returns attributes and explicit unknown/unavailable states;
it never turns denied or incomplete discovery into an empty successful list.
The desktop refresh is passive. Importing older metadata is explicit; CLI
attribute discovery may authorize access and refresh its display cache.

A saved identity passphrase is bound to a canonical key path and exact file
contents, verified by a temporary isolated OpenSSH agent. Connection preparation
discovers public identities without reading secrets. Only a signature request
unlocks the required saved key. Changed files invalidate reuse; a neighbouring
public key is a hint, not authorization. Native fallback remains available for
unsupported SSH configuration.

VPN credential identity includes the logical route and remote trust pin, while
excluding the local broker socket. Desktop and CLI brokers can share saved
items without sharing in-memory approval contexts. Legacy lookups remain exact;
opaque historical items are not guessed into another route.

After successful authentication, the broker can retain authorization contexts
for at most 24 hours from their original approval. Reads do not extend the
window. Contexts bind the account, endpoint, route, effective SSH configuration,
host trust, and exact secret selector; key approval also binds file contents.
Secrets are read for authentication and temporary owned buffers are zeroized.
Decrypted keys are retained only within the connection attempt.

Lock, sleep, logout, session changes, broker restart, explicit disconnect, and
owned-secret mutations revoke approval. Updated one-shot helpers publish a
shared nonsecret revision before mutation. Background reconnect uses quiet
master establishment; it never opens authentication or save UI. Missing approval
returns authentication-required for explicit interaction. `accept-new` host-key
policy disables retained approval for new connections.

## Invariants

1. Inventory and history never expose passwords, passphrases, or private key bytes.
2. Metadata does not authorize secret reuse or prove that a key is unchanged.
3. Failed authentication grants no reusable approval.
4. Approval is bounded, process-local, scoped, and revocable; secrets are not its cache.
5. Forgetting credentials does not remove key files or terminate existing connections.
6. Background work cannot request interactive credential access.

## Protocol impact

After helper 1.1.2/build 2, explicit clear adds `ctld_helper` 1.1.3/build 3;
attribute discovery adds 1.1.4/build 4. Shared mutation revocation adds
1.1.5/build 5, retaining 1.0.1 and 1.1.2–1.1.4. New clients require 1.1.5
before sending secret mutations. Quiet master establishment advances `ctld`
from 1.1.13/build 13 to 1.1.14/build 14, retaining 1.0.12 and 1.1.13.
Older brokers support passive status rather than quiet creation. The lifecycle
contract remains 1.0.1/build 1. Older writers must be replaced before relying
on cross-process approval revocation.

## Out of scope

Secret reveal/copy UI, persistent authorization contexts, private-key storage,
non-macOS Keychain emulation, and guaranteed prompt-free reconnection.

## Unresolved questions

None for the implemented macOS boundary.

## Detailed specifications

- [Credentials, identity verification, and approval](../credentials.md)
- [Connection transitions](../connection-state.md)
- [Protocol versions and mutation compatibility](../protocol-versioning.md)
