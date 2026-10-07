# Published protocol contracts

Product releases, internal protocol builds, and published wire contracts have
separate meanings. Product releases such as `0.1.0` identify shipped software;
they do not imply a wire change. Each protocol has an independent integer build
and an independent published `major.minor.build` version.

- The patch/build increases when the protocol changes during development.
  Some builds never ship, so gaps are intentional. Implementation changes that
  leave the protocol unchanged do not require a new contract.
- The minor identifies a release cycle. After the current contract is released
  or frozen, the first protocol revision for the next cycle advances the minor
  once and increments the build. Further revisions in that cycle increment only
  the patch/build. Releasing freezes the final version without another bump.
- The major increases for an explicitly announced breaking change. Every later
  implementation in one major must support **every earlier published contract**
  in that major, across minor versions. There is no rolling support window.

For example, after ctld `1.0.12` is frozen, the next release cycle starts with
`1.1.13`. Another development protocol change produces `1.1.14`. Releasing that
cycle freezes `1.1.14`; the first protocol change for the following cycle produces
`1.2.15`. An implementation of `1.1.14` must still implement the earlier published
`1.0.12` contract. An announced `2.0.21` breaking release can drop major-1 support.
The build counter does not reset with the minor or major. These are project
contract versions, not ordinary SemVer patch promises.

## First published contracts

No earlier integer-only development protocol was published. This first contract
includes the negotiation envelope; it does not promise compatibility with those
unpublished wire formats.

| Protocol name | Internal build | First published contract |
| --- | ---: | --- |
| `ctmux` | 13 | `1.0.13` |
| `ctmux_control` | 1 | `1.0.1` |
| `ctld` | 12 | `1.0.12` |
| `ctld_lifecycle` | 1 | `1.0.1` |
| `ctld_helper` | 1 | `1.0.1` |
| `task` | 4 | `1.0.4` |
| `task_control` | 2 | `1.0.2` |
| `ctl_identity` | 3 | `1.0.3` |
| `ctl_maintenance` | 2 | `1.0.2` |
| `ctl_remote_vpn` | 1 | `1.0.1` |

The remote VPN addition starts the next development cycle with ctld `1.1.13`
(build 13) and helper `1.1.2` (build 2), retaining their frozen initial contracts.
During that same development cycle, helper `1.1.3` (build 3) adds explicit
clearing of saved SSH credentials and identity passphrases. It retains support
for both `1.0.1` and `1.1.2`; clients using the new clear operation require the
updated helper.
Helper `1.1.4` (build 4) adds the `discover` credential request and `discovered`
response for authoritative, attribute-only saved-password discovery. Its entries
include saved SSH credentials and identity passphrases, retain identifiable
items with unknown metadata, and report scan completeness and specific warnings
separately. No secret values are returned. Discovery requires `1.1.4`; helpers
implementing earlier contracts reject the new request. Existing requests and
responses are unchanged, and `1.0.1`, `1.1.2`, and `1.1.3` remain supported.
Further protocol changes before release increment only their patch/build.
A broker channel selecting ctld `1.0.12` supports the original SSH and local VPN
routes; remote VPN route steps
require `1.1.13`. Proxy helpers must explicitly advertise helper `1.1.2` before
remote VPN routes are passed to them. Other helper operations retain `1.0.1`
behavior. The independent remote VPN channel negotiates `1.0.1` before identity
and credentials, using a stable marker rather than changing the marker per build.

Quiet SSH establishment advances ctld from `1.1.13` (build 13) to `1.1.14`
(build 14), retaining `1.0.12` and `1.1.13`. The additive `ensure_master_quiet`
request may reuse a master or create one without Keychain authentication UI,
OpenSSH confirmation, password entry, or credential-save UI. If user approval is
required, it returns the existing `authentication_required` response. Ordinary
`ensure_master` retains its interactive behavior. Clients require negotiated
`1.1.14` before sending the quiet request; with earlier brokers, background
reconnects send only passive `master_status` and require explicit interaction
when a fresh SSH connection is needed. This broker addition does not change the
independent helper or lifecycle contracts, or the product release version.

Reconnect authorization revocation advances `ctld_helper` from `1.1.4` (build 4)
to `1.1.5` (build 5). Credential `forget`/`clear` and identity `save`/`forget`
retain their existing request and response shapes, but every owned-secret
mutation publishes a shared nonsecret revision before modifying Keychain. This
also revokes approvals retained by a separately running updated broker. New
clients require explicit helper `1.1.5` support before sending a mutating
request, including any supplied passphrase; passive metadata and discovery
operations retain their earlier contract requirements. Updated helpers retain
`1.0.1`, `1.1.2`, `1.1.3`, and `1.1.4`, and provide revocation for old clients'
mutating requests too. Legacy helper binaries and brokers cannot implement this
cross-process policy; replace them before relying on reconnect approval reuse.
The independent `ctld_lifecycle` contract remains `1.0.1` (build 1), and the
product release version remains separate.

Shared ctmux pane zoom advances its open development cycle from `1.1.14` to
`1.1.15` (build 15), retaining `1.0.13` and `1.1.14`. Zoom commands and shared
zoom fields/events require the negotiated `1.1.15` contract. Older clients keep
the ordinary split layout and new clients report zoom unavailable on older
daemons.

Storage schema versions are separate. Changing a protocol contract does not
rename or migrate an on-disk schema.

## Negotiation and release mapping

The first handshake or control request contains an offer:

```json
{
  "protocol": {
    "build": 15,
    "version": "1.1.15",
    "supported_versions": ["1.0.13", "1.1.15"]
  }
}
```

The server selects the highest **explicitly implemented** shared contract and
returns it as `protocol_version`. The client verifies that selection. Equal
majors alone are insufficient, and an unpublished build is never inferred from
a numeric range. If no contract is shared, the connection fails before the
requested operation. New clients use old-server messages and semantics when an
older contract is selected; new servers preserve old-client messages and
semantics. Optional features must be gated by that selected contract.

Daemon status and session/task handshakes return actual advertisements, separately
from the selected contract. A server can advertise `1.1.15` while one connection
selects `1.0.13`; diagnostics must retain both facts.

Every component's `--component-info` output declares the release mapping:
`build.version` is the product release, while each entry of `protocols` contains
`name`, integer `build`, published `version`, and `supported_versions`. Signed
ctld distribution manifests include that same protocol map and verify it against
the executable after signature verification. `ctld --protocol-version` prints
its published IPC contract; `ctld --protocol-build` prints the internal integer.

Source revision, source fingerprint, archive checksum, and signature identify
and verify a binary. They do not determine wire compatibility. Compatible
helpers from different product releases may be reused when their advertised
contracts intersect the client's supported set. Verified replacement checks
still pin the complete inspected binary and its metadata.

Remote bundle schema 2 records the complete `--component-info` metadata for each
shipped component. Standalone CLI repair can reuse a local or cached bundle from
a different product release or source revision after verifying its own identity,
target, archive and binary checksums, and agreement with the archived manifest.
Eligibility requires explicit shared service contracts and compatible contracts
between the gateway, task daemon, ctmux daemon, and ctld VPN broker. Agent and task executable
metadata includes their consumed companion contracts; running daemon diagnostics
continue to describe the actual owner serving a connection. Reuse never restarts
that owner. Schema-1 bundles lack compatibility advertisements and remain eligible
only for the exact clean client revision. New downloads also retain exact-source
selection; a dirty or unidentified development client can reuse a verified
compatible bundle but cannot download an inferred matching build.

## Discovery and SSH framing

The default ctld socket or Windows pipe is keyed by the protocol major, such as
`ctld-v1.sock`, so changing an internal build or compatible minor does not hide an
existing owner. Ctmux and task discovery endpoints are already stable. Explicit
socket overrides retain their meaning.

Identified SSH channels use the stable `ctl-ssh-identity\n` marker. The agent
then writes a bounded identity-contract offer; the client sends its selected
`protocol_version`, and the agent connects or starts the companion daemon before
writing environment identity and relaying the selected service. Negotiation
precedes opening or starting that service; identity confirms it is ready.
The framing marker does not change for compatible builds or minors.

Unpublished v2/v3 agents remain eligible for the explicit repair offer. A
read-only legacy identity probe verifies the pinned environment before upload;
it sends no service requests and promises no legacy contract compatibility.

## Developing and releasing contracts

Keep the named initial contract constants immutable. Increasing `PROTOCOL_BUILD`
does not automatically publish another version: the advertised latest contract
and supported set are separate constants. Internal development must continue
implementing the advertised contracts; a breaking prototype cannot claim an old
contract simply because its major has not changed.

When the protocol changes, add its named development contract, retain every
earlier published entry in that major, implement any required codec/behavior
adapters, and gate new operations or fields by the negotiated contract. Advance
the minor only when opening a cycle after the previous contract is frozen;
additional changes within the open cycle advance only the patch/build. Tests
must exercise old-client/new-server and new-client/old-server behavior using
historical messages, alongside unsupported selections and malformed offers.
Do not advertise a contract until its implementation and compatibility tests
exist.

Every PR description must include a protocol statement. For each affected named
contract, record the previous and new contract versions and internal builds,
describe changes to operations, fields, or negotiation behavior, and state which
earlier contracts remain supported. Identify an announced breaking change
explicitly, and keep product release versions separate from protocol versions.
If the protocol is unchanged, write `Protocol changes: none.`

At release, freeze the final development contract and record the exact protocol
map in the release metadata. Do not increment the minor again at this point.
Resource bounds on metadata are transport constraints, never a policy allowing
earlier published contracts to be removed.

Component inspection opens the next maintenance development cycle at `1.1.3`
(build 3), retaining the frozen initial `1.0.2` contract for published-owner
restart preparation. Further changes in this cycle advance only the build,
for example `1.1.4`; release freezes the final version without another bump.
The new contract supports a fixed `inspect_components` request and reports
historical numeric ctmux control metadata separately from published protocols.
Numeric-owner restart preparation is available only under `1.1.3`; it pins the
successful historical control stream
and requires the same separate confirmation as a published owner. This is an
explicit maintenance bridge, not a claim that numeric session protocol 13
implements published session contract `1.0.13`.

Every PR description must state its protocol changes, or **None** when there
are no changes. Include the previous and new contracts and the compatibility
impact. Keep PR titles brief; put protocol details in the description.
