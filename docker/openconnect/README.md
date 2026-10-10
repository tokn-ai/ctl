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
**VPN** sidebar in ctmux. Add a named connection with its server, username, and
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
container exits. Closing ctmux
leaves the connection running under ctld. Compatible containers started by another ctld also appear in the panel.
Connect acquires this daemon's heartbeat interest; Disconnect releases only that
interest. A shared container remains visible while another daemon uses it. Each saved connection combines
its settings and status in one item, with its VPN server, username, and local
SOCKS5 endpoint. Only active connections without a matching saved profile get a
temporary item. An indicator on the VPN tab
shows connection activity while other panels are open.

## Create a profile from the CLI

Run `ctl vpn create` in an interactive terminal and choose OpenConnect. The
questionnaire asks for a display name, gateway address, username, masked password,
optional authentication method, and optional IPv4 connectivity-check target. It saves
the profile privately to `~/.tokn/ctl/vpns.json`, shared with the desktop VPN page.
`CTL_VPNS_PATH` selects another catalog. Creation does not start ctld, build an
image, or connect a VPN. Ctrl-C or Esc cancels without changing the catalog.

A bare gateway address uses HTTPS; an HTTPS URL may include its port and path.
OpenConnect's Array login form has a `method` field; provide the optional
authentication method if the gateway requires it. Server certificate verification
remains enabled. Enter passwords literally in the masked prompt.

Docker or Podman assigns the localhost port. The optional connectivity-check
target requests an SSH host-key handshake on port 22 after startup. Omitting it
or failing that probe does not prevent the VPN and proxy from running.

At startup, ctld sends the saved configuration privately over attached stdin to
a mode-0600 environment file in container tmpfs. The password goes to OpenConnect
through a separate stdin pipe, without being included in command arguments or
container environment metadata.

## Manage with ctl

Build the image once, then use the current ctl and ctld binaries:

```sh
./docker/openconnect/run.sh build
ctl vpn create
ctl vpn start NAME_OR_ID
ctl vpn list
ctl vpn stop NAME_OR_ID
```

Start talks to ctld and starts the daemon automatically if needed. Starting the
VPN waits for its tunnel interface, routes, DNS, and SOCKS listener to become
ready, then prints a table with the VPN state, server, username, and randomly
assigned SOCKS5 endpoint. It reuses a compatible container or recreates a missing
one.
An exact stable profile ID takes precedence over a unique exact name. The saved
catalog defaults to `~/.tokn/ctl/vpns.json`; `CTL_VPNS_PATH` selects another file.
Omitting the start selector opens a profile picker in an interactive terminal;
scripts must supply a selector.

List combines saved profiles with runtime connections and compatible shared
containers. Its table shows name, provider, state, server or tailnet, username,
SOCKS5 endpoint, and VPN ID. Saved profiles stay visible after container removal.
An unmatched saved profile is disconnected only when runtime inventory is
complete; otherwise it is unavailable. List never starts ctld or renews a heartbeat.

Create, start, list, stop, and remove accept `--json`. Create remains interactive;
its JSON output contains only saved metadata and its prompts stay on stderr.
List JSON is a snapshot
with sanitized merged `entries`, raw runtime `connections`, capability fields,
and optional `discovery_warnings` when the container inventory could not be
checked. If the saved catalog cannot be read, runtime entries remain available
with `profile_warnings`. Each runtime connection includes `vpn_id`, `vpn_url`,
`username`, `endpoint`, `container_name`, `running`, `connection_id`, `state`,
immutable `container_id`, `shared_container`, and `locally_connected`. The table's
USE column appears only when a displayed
VPN has `locally_connected: false`. It shows `owned` when the selected daemon
holds heartbeat interest and `shared` for a container discovered without local
interest. The JSON fields are unchanged. Inventory accepts only the current
heartbeat protocol and user namespace. Start and stop JSON return the affected
connection. Saved connections use their profile ID as `vpn_id`; runtime-only
connections retain their runtime IDs. The VPN server is its HTTPS origin;
credentials, paths, queries, and fragments are omitted. `state` is `stopped`,
`starting`, `connected`, or `stopping`; older runtime-only connections may have a
null `connection_id`.
The ready proxy endpoint is a `socks5h://127.0.0.1:PORT` URL.

Each VPN owns a separate container and random SOCKS5 endpoint. Stop accepts an
exact saved profile ID, a unique exact profile name, or a runtime VPN ID from
list. With a current daemon, omitting the selector opens a picker of local
connections in an interactive terminal. In scripts,
an untargeted stop succeeds only when this daemon has zero or one connection;
discovered containers do not make that selection ambiguous. Repeating a start
for the same saved profile reuses its active connection.

To delete a saved profile, use `ctl vpn remove NAME_OR_ID`. Omitting the selector
opens a saved-profile picker. Removal always requires interactive confirmation,
defaulting to No; there is no `--yes` bypass. It deletes the saved catalog entry
and retains any Tailscale identity volume. After confirmation, removal requires
complete runtime inventory and a stopped VPN. Release its heartbeat interests
and wait for container exit first. Remove never starts a daemon or stops a VPN
automatically; unavailable or legacy inventory blocks deletion.

Connection metadata comes from the settings used to start the VPN and stays
unchanged until it stops. An already running older ctld may return no server or
username; the CLI displays `unavailable` until a connection is started by the
updated daemon.

For development from this checkout, the helper's create action builds the CLI
and opens the questionnaire without building an image. Start builds the image
and both Rust binaries, then starts the selected saved profile:

```sh
./docker/openconnect/run.sh create
./docker/openconnect/run.sh start NAME_OR_ID
./docker/openconnect/run.sh list
./docker/openconnect/run.sh stop NAME_OR_ID
```

List, stop, and remove build the CLI only if its binary is missing. To delete a
saved profile through the same helper, run
`./docker/openconnect/run.sh remove NAME_OR_ID` and confirm interactively.

The helper sets `CTLD_BIN` to the matching `target/debug/ctld` binary for daemon
auto-start. Use `CTLD_SOCKET_PATH=/absolute/path/to/ctld.sock` consistently when
using a custom broker socket. The desktop app accepts `CTLD_VPN_SOCKET_PATH`
when its VPN owner should differ from its SSH helper. Signed development keeps its isolated ctld endpoint. Both daemons discover
compatible containers for the same user and container engine.
An older daemon returns `supports_multiple: false` and its existing connection
remains visible. Update and restart that owner before starting simultaneous VPNs
or stopping a connection by ID. An omitted selector uses that owner's untargeted
stop directly, preserving `ctl vpn stop` for its single local connection.

The VPN keeps running after `ctl vpn start` exits because ctld renews its heartbeat.
`ctl vpn stop VPN_ID` and daemon exit release only that daemon's renewal task.
The container removes itself after the last heartbeat expires. Another daemon
can reuse the same profile and random port by connecting to it. List inspection
never renews a heartbeat. Release every interested daemon before changing an
active profile's routing settings.

## Use the current endpoint

This shell example uses Python 3 to read the randomly assigned endpoint from
ctl list. `socks5h` resolves names inside the container using its current DNS
configuration, including VPN-provided DNS servers:

```sh
vpn_id=VPN_ID
endpoint=$(ctl vpn list --json | python3 -c \
  'import json, sys; print(next(v["endpoint"] for v in json.load(sys.stdin)["connections"] if v["vpn_id"] == sys.argv[1]))' "$vpn_id")
curl --proxy "$endpoint" https://example.com
```

For SSH on macOS, use the same endpoint with the system netcat:

```sh
socks_address=${endpoint#socks5h://}
ssh -o "ProxyCommand=nc -X 5 -x $socks_address %h %p" USER@HOST
```

When using the checkout helper, replace `ctl vpn list --json` with
`./docker/openconnect/run.sh list --json` to query through the same binaries.
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

Use a runtime connection's `container_name` from `ctl vpn list --json` with
Docker to inspect logs or run the optional connectivity probe while the VPN is
running:

```sh
docker logs --follow CONTAINER_NAME
docker exec CONTAINER_NAME /usr/local/bin/vpn-healthcheck
docker exec CONTAINER_NAME /usr/local/bin/ssh-handshake
```

Health checks require an up, addressed VPN interface, a SOCKS listener, and at
least five seconds since the latest successful tunnel setup. Reconnection clears
readiness and restarts this monotonic stability window, so rapid tunnel flapping
cannot advertise a healthy connection. These checks remain independent of
`TARGET_IP` and do not prove reachability of every destination.
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
