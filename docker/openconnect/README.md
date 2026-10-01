# OpenConnect with SOCKS5

Use `ctl vpn` to manage an Array Networks VPN and a SOCKS5 TCP proxy through
ctld. ctld runs them in one Linux container and publishes container port 1080
on a randomly assigned Mac localhost port. Outgoing connections follow the
container's routing table: VPN destinations use its VPN routes, and other
destinations use their existing routes. A full-tunnel VPN also supplies the
default route. The proxy does not bind outgoing connections to an interface or
limit them to a test target.

## Manage in the desktop app

Build the image once with `./docker/openconnect/run.sh build`, then open the
**VPN** sidebar in rmux. Add a named connection with its server, username, and
password, save it, and choose **Connect**. Authentication method and the optional
connectivity-check target are under advanced settings. The panel supports editing
and deleting saved connections, disconnecting, and copying the SOCKS5 endpoint.
Rebuild existing images for the shared heartbeat watchdog with
`./docker/openconnect/run.sh build`. ctld verifies the image's heartbeat protocol
before creating a container or sending credentials, then starts that inspected
image by its immutable ID. An incompatible image returns a rebuild instruction
instead of failing during authentication. Containers using the old lifetime
protocol must be recreated.

The app saves connections in a private JSON file. At connection time, ctld sends
the selected configuration over attached stdin to the container, which creates
a private environment file in tmpfs. That generated file disappears when the
container exits; the app never asks users to manage `.env` files. Closing rmux
leaves the connection running under ctld. Compatible containers started by another ctld also appear in the panel.
Connect acquires this daemon's heartbeat interest; Disconnect releases only that
interest. A shared container remains visible while another daemon uses it. Each saved connection combines
its settings and status in one item, with its VPN server, username, and local
SOCKS5 endpoint. Only active connections without a matching saved profile get a
temporary item. An indicator on the VPN tab
shows connection activity while other panels are open.

## CLI configuration

Copy `docker/openconnect/.env.example` to `.env` at the repository root if the
file does not exist, then edit it:

```dotenv
VPN_URL=https://your-vpn-host
VPN_USERNAME=your-username
VPN_PASSWORD=your-password
# VPN_AUTH_METHOD=your-method-name
# TARGET_IP=
```

There is no port setting: Docker or Podman assigns the localhost port.
`TARGET_IP` is optional and requests an SSH host-key handshake on port 22 after
startup. Omitting it, leaving it empty, or failing that probe does not prevent
the VPN and proxy from running.

`VPN_URL` accepts a URL or a hostname/IP with an optional port and path. A bare
address uses HTTPS. OpenConnect's Array login form has a `method` field; set
`VPN_AUTH_METHOD` if the gateway needs a named method. Otherwise an empty value
is sent. Server certificate verification remains enabled.

Use literal, single-line values without shell quotes or escaping `$`, `#`, or
spaces. The file is parsed as data, never evaluated as shell code. It is ignored
by Git and image builds and set to mode `0600` by the helper. ctld reads a private,
bounded snapshot and sends it over attached stdin to a mode-0600 file in container
tmpfs, just like saved desktop connections.
The password goes to OpenConnect through a separate stdin pipe, without being
included in command arguments or container environment metadata.

## Manage with ctl

Build the image once, then use the current ctl and ctld binaries:

```sh
./docker/openconnect/run.sh build
ctl vpn start --env-file .env
ctl vpn status
ctl vpn stop VPN_ID
```

`ctl` talks to ctld and starts the daemon automatically if needed. Starting the
VPN waits for its tunnel interface, routes, DNS, and SOCKS listener to become
ready, then prints a table with the VPN state, server, username, and randomly
assigned SOCKS5 endpoint. Start, status, and stop accept `--json` for scripts.
Status JSON is a snapshot with `connections`, `supports_multiple`, and optional
`discovery_warnings` when the container inventory could not be checked. Each
connection includes `vpn_id`, `vpn_url`, `username`, `endpoint`, `container_name`,
`running`, `connection_id`, `state`, immutable `container_id`, `shared_container`,
and `locally_connected`. The table's USE column distinguishes this ctld from a
shared container discovered without local heartbeat interest. Inventory accepts
only the current heartbeat protocol and user namespace. Start and stop JSON return the affected
connection. Saved connections use their profile ID as `vpn_id`; file-based starts
receive a stable ID derived from the canonical settings path. The VPN server is its HTTPS origin; credentials,
paths, queries, and fragments are omitted. `state` is `stopped`, `starting`,
`connected`, or `stopping`; CLI-started connections have a null `connection_id`.
The ready proxy endpoint is a `socks5h://127.0.0.1:PORT` URL.

Each VPN owns a separate container and random SOCKS5 endpoint. Use `ctl vpn stop VPN_ID` to release one local connection. Without an ID, stop
succeeds only when this daemon has zero or one connection; discovered containers
do not make that selection ambiguous. Repeating a start for the
same saved connection or settings path reuses its active connection.

Connection metadata comes from the settings used to start the VPN and stays
unchanged until it stops. An already running older ctld may return no server or
username; the CLI displays `unavailable` until a connection is started by the
updated daemon.

For development from this checkout, the helper builds the image and both Rust
binaries, then invokes ctl with the root `.env`:

```sh
./docker/openconnect/run.sh start
./docker/openconnect/run.sh status
./docker/openconnect/run.sh stop
```

The helper sets `CTLD_BIN` to the matching `target/debug/ctld` binary for daemon
auto-start. Use `CTLD_SOCKET_PATH=/absolute/path/to/ctld.sock` consistently when
using a custom broker socket. The desktop app accepts `CTLD_VPN_SOCKET_PATH`
when its VPN owner should differ from its SSH helper. Signed development keeps its isolated ctld endpoint. Both daemons discover
compatible containers for the same user and container engine.
An older daemon returns `supports_multiple: false` and its existing connection
remains visible. Update and restart that owner before starting simultaneous VPNs
or stopping a connection by ID. Explicit `ctl vpn stop` can stop its current VPN.

The VPN keeps running after `ctl vpn start` exits because ctld renews its heartbeat.
`ctl vpn stop VPN_ID` and daemon exit release only that daemon's renewal task.
The container removes itself after the last heartbeat expires. Another daemon
can reuse the same profile and random port by connecting to it. Status inspection
never renews a heartbeat. Release every interested daemon before changing an
active profile's routing settings.

## Use the current endpoint

This shell example uses Python 3 to read the randomly assigned endpoint from
ctl status. `socks5h` resolves names inside the container using its current DNS
configuration, including VPN-provided DNS servers:

```sh
vpn_id=VPN_ID
endpoint=$(ctl vpn status --json | python3 -c \
  'import json, sys; print(next(v["endpoint"] for v in json.load(sys.stdin)["connections"] if v["vpn_id"] == sys.argv[1]))' "$vpn_id")
curl --proxy "$endpoint" https://example.com
```

For SSH on macOS, use the same endpoint with the system netcat:

```sh
socks_address=${endpoint#socks5h://}
ssh -o "ProxyCommand=nc -X 5 -x $socks_address %h %p" USER@HOST
```

When using the checkout helper, replace `ctl vpn status --json` with
`./docker/openconnect/run.sh status --json` to query through the same binaries.
The proxy supports TCP, including SSH and HTTPS; it does not provide SOCKS5 UDP
relay. Configure each application to use the proxy. It does not install routes
on the Mac itself.

## Inspect and lifetime

Startup errors identify recognized gateway DNS, network, authentication,
certificate, image, and container-engine failures. For authentication problems,
check both the server address and **Advanced options → Authentication method**
against the settings provided for the VPN. Diagnostics return fixed messages;
ctld does not retain or return raw container output, which may contain private
gateway details. Unrecognized failures retain a generic exit or timeout message.

Use a connection's `container_name` from `ctl vpn status --json` with Docker to inspect logs or run the optional
connectivity probe while the VPN is running:

```sh
docker logs --follow CONTAINER_NAME
docker exec CONTAINER_NAME /usr/local/bin/vpn-healthcheck
docker exec CONTAINER_NAME /usr/local/bin/ssh-handshake
```

Health checks track VPN setup and the SOCKS listener independently of `TARGET_IP`.
A manually requested SSH probe returns an error on failure. Host-key fingerprints
are observations; the probe does not authenticate an SSH user or add keys to
`known_hosts`.

Every interested ctld sends a heartbeat every two seconds through an independent
engine exec request addressed to the immutable container ID. The watchdog uses
monotonic container time and exits after 15 seconds without any daemon heartbeat.
Stdin is used only for the initial configuration; closing it or losing the creator's
Docker CLI does not stop a shared container. Startup has a 15-second grace period. Cleanup gives child processes up to three seconds to terminate
before killing them. The container is removed when it exits.

This setup works with Docker or Docker-compatible Podman on macOS. It requires
`/dev/net/tun` and `NET_ADMIN`, without privileged mode. ctld disables SELinux
labeling for this container because the Podman VM otherwise denies access to the
mapped TUN device. Array support in OpenConnect is experimental and currently
documents basic username/password authentication.
