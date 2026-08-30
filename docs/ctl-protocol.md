# ctl control protocol version 1

`ctl-proto` is the small outer protocol between a remote client and `ctld`.
It is distinct from `rmux-proto`: it authenticates a device/client pair and
selects a service. Once the `rmux` service is selected, the outer protocol is
finished permanently and the connection carries raw `rmux-proto` version 6
frames.

## Transport

The normal transport is TCP reachable through Tailscale. `ctld` must bind an
explicit non-wildcard address; Tailscale ACLs are useful defence in depth but
are not authorization.

TLS is server-authenticated with a per-device self-signed certificate. A
pairing invitation pins that exact certificate and uses a stable synthetic DNS
name (`<device-id>.ctl.invalid`) for TLS verification, independent of a
changing Tailnet address.

The maximum outer-control frame is 64 KiB. A frame is a four-byte unsigned
big-endian JSON payload length followed by a UTF-8 JSON payload. The outer
protocol applies only before the raw service upgrade.

## Pairing and client identity

`ctld init` creates a device ID, TLS private key/certificate, and an empty
authorization registry in owner-only device state. `ctld pair create` creates
a 32-byte random one-time token, persists only `SHA-256(token)`, and produces
an explicitly versioned invitation containing:

- endpoint;
- invitation format version;
- device ID and stable TLS name;
- pinned server certificate;
- client label;
- token and expiry timestamp.

The invitation is a bearer secret. Do not put it in documentation, source
code, logs, or a shell-history example.

`ctl pair` generates an Ed25519 key locally, connects using the invitation's
pinned TLS certificate, and presents the token and public key. `ctld` consumes
the token, binds the expected label to the key, and records the derived stable
client ID. The private key never leaves the client.

Every later connection receives a random challenge. The client signs this
canonical byte sequence with its Ed25519 key:

```text
"ctl-auth-v1\0"
+ u32_be(challenge length) + challenge
+ u32_be(client name length) + client name UTF-8
+ u32_be(client version length) + client version UTF-8
```

Length-prefixing prevents ambiguity caused by concatenating variable-length
fields. `ctld` verifies the signature against its non-revoked authorized-key
registry. Authentication is explicit in the protocol rather than inferred
from Tailnet location.

## Control lifecycle

The server accepts only this order:

```text
client                                                   ctld
------                                                   ----
hello { control_protocol_version, client_name, client_version }
                                         ->
       hello_accepted { device_id, challenge, server_version }
                                         <-

pair { token, public_key, label }       ->  pair_accepted { client_id } <-
  (connection closes)

or

authenticate { public_key, signature }  ->
       authenticated { client_id, capabilities: [rmux_tunnel] }
                                         <-
open_service { rmux, rmux_protocol_version: 6 }
                                         ->
       service_opened { rmux }
                                         <-
<raw rmux-proto version 6 bytes in both directions>
```

Protocol versions must match exactly. A failed authentication, expired or
reused token, wrong label, or invalid request receives a structured outer
error before the connection closes. No local `rmuxd` socket is contacted before
successful authentication and service selection.

## rmux upgrade and reconnect

After `service_opened`, `ctld` does a raw bidirectional copy between its TLS
stream and a fixed same-user local `rmuxd` endpoint. It must not wrap,
deserialize, buffer through an application reader, inspect, or inject
`rmux-proto` frames. In particular, the first raw `rmux` handshake may arrive
immediately after the service response.

Closing the TLS connection closes only the corresponding `rmuxd` attachment;
this releases its input/layout leases. The shell, PTY, raw output journal, and
checkpoints remain owned by `rmuxd`. A reconnect repeats the outer
authentication then attaches with the durable `rmux` session ID and last raw
sequence that its renderer applied. `rmux-proto` version 6 also expires a silent attachment using
heartbeat acknowledgements, so a half-open gateway path cannot pin a lease
indefinitely. See `docs/rmux-protocol.md` for the checkpoint and stream
semantics.

## Current scope and deferred work

Version 1 authorizes only `rmux_tunnel`. It does not expose arbitrary local
sockets, command execution, filesystem operations, jobs, port forwarding,
services, system information, or desktop streaming.

The current `ctld -> rmuxd` implementation uses Unix-domain sockets on macOS
and Linux. Windows will add a local endpoint implementation without changing
this control protocol or `rmux-proto`.
