# Published protocol contracts

Product releases, internal protocol builds, and published wire contracts have
separate meanings. Product releases such as `0.1.0` identify shipped software;
they do not imply a wire change. Each protocol has an independent integer build
and an independent published `major.minor.build` version.

- The build increases for internal protocol revisions. Some builds never ship.
- The minor increases for compatible additions. The last number reuses the
  build of the published contract, so gaps are intentional.
- The major increases for an explicitly announced breaking change. Every later
  implementation in one major must support **every earlier published contract**
  in that major, across minor versions. There is no rolling support window.

For example, `1.0.13` is the first published ctmux contract. Build 14 may remain
unpublished; `1.1.15` can be the next published contract. An implementation of
`1.1.15` must also implement `1.0.13`. An announced `2.0.21` breaking release can
drop major-1 support. The internal build counter does not reset with the major.
These are project contract versions, not ordinary SemVer patch promises.

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

The remote VPN addition publishes ctld `1.1.13` (build 13) and helper `1.1.2`
(build 2), retaining their initial contracts. A broker channel selecting ctld
`1.0.12` supports the original SSH and local VPN routes; remote VPN route steps
require `1.1.13`. Proxy helpers must explicitly advertise helper `1.1.2` before
remote VPN routes are passed to them. Other helper operations retain `1.0.1`
behavior. The independent remote VPN channel negotiates `1.0.1` before identity
and credentials, using a stable marker rather than changing the marker per build.

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

## Publishing the next contract

Keep the named initial contract constants immutable. Increasing `PROTOCOL_BUILD`
does not automatically publish another version: the advertised latest contract
and supported set are separate constants. Internal development must continue
implementing the advertised contracts; a breaking prototype cannot claim an old
contract simply because its major has not changed.

Before publishing a compatible minor, add its named contract, retain every
earlier published entry in that major, implement any required codec/behavior
adapters, and gate new operations or fields by the negotiated contract. Tests
must exercise old-client/new-server and new-client/old-server behavior using
historical messages, alongside unsupported selections and malformed offers.
Do not advertise a contract until its implementation and compatibility tests
exist. Resource bounds on metadata are transport constraints, never a policy
allowing old contracts to be removed.
