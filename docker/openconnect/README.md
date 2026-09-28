# OpenConnect with SOCKS5

Use `ctl vpn` to manage an Array Networks VPN and a SOCKS5 TCP proxy through
ctld. ctld runs them in one Linux container and publishes container port 1080
on a randomly assigned Mac localhost port. Outgoing connections follow the
container's routing table: VPN destinations use its VPN routes, and other
destinations use their existing routes. A full-tunnel VPN also supplies the
default route. The proxy does not bind outgoing connections to an interface or
limit them to a test target.

## Configuration

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
by Git and image builds, mounted read-only, and set to mode `0600` by the helper.
The password goes to OpenConnect through a separate stdin pipe, without being
included in command arguments or container environment metadata.

## Manage with ctl

Build the image once, then use the current ctl and ctld binaries:

```sh
./docker/openconnect/run.sh build
ctl vpn start --env-file .env
ctl vpn status
ctl vpn stop
```

`ctl` talks to ctld and starts the daemon automatically if needed. Starting the
VPN waits for its tunnel interface, routes, DNS, and SOCKS listener to become
ready, then returns JSON with the randomly assigned endpoint. Status is JSON
with `endpoint`, `container_name`, and `running`. The ready endpoint is a
`socks5h://127.0.0.1:PORT` URL.

For development from this checkout, the helper builds the image and both Rust
binaries, then invokes ctl with the root `.env`:

```sh
./docker/openconnect/run.sh start
./docker/openconnect/run.sh status
./docker/openconnect/run.sh stop
```

The helper sets `CTLD_BIN` to the matching `target/debug/ctld` binary for daemon
auto-start. Use `CTLD_SOCKET_PATH=/absolute/path/to/ctld.sock` consistently when
using a custom broker socket.

The VPN keeps running after `ctl vpn start` exits because ctld owns its
container and heartbeat stream. `ctl vpn stop` stops and removes only the VPN
container; the broker remains available for other ctl commands. Exiting ctld
also stops the VPN. Stop and start the VPN after changing its settings.

## Use the current endpoint

This shell example uses Python 3 to read the randomly assigned endpoint from
ctl status. `socks5h` resolves names inside the container using its current DNS
configuration, including VPN-provided DNS servers:

```sh
endpoint=$(ctl vpn status | python3 -c \
  'import json, sys; print(json.load(sys.stdin)["endpoint"])')
curl --proxy "$endpoint" https://example.com
```

For SSH on macOS, use the same endpoint with the system netcat:

```sh
socks_address=${endpoint#socks5h://}
ssh -o "ProxyCommand=nc -X 5 -x $socks_address %h %p" USER@HOST
```

When using the checkout helper, replace `ctl vpn status` with
`./docker/openconnect/run.sh status` to query through the same binaries.
The proxy supports TCP, including SSH and HTTPS; it does not provide SOCKS5 UDP
relay. Configure each application to use the proxy. It does not install routes
on the Mac itself.

## Inspect and lifetime

Use `container_name` from status with Docker to inspect logs or run the optional
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

ctld sends a heartbeat every two seconds through the attached container's stdin.
Closing that stream stops the VPN and proxy promptly. If the stream remains open
after a daemon or transport failure, the container stops after 15 seconds without
a heartbeat. Cleanup gives child processes up to three seconds to terminate
before killing them. The container is removed when it exits.

This setup works with Docker or Docker-compatible Podman on macOS. It requires
`/dev/net/tun` and `NET_ADMIN`, without privileged mode. ctld disables SELinux
labeling for this container because the Podman VM otherwise denies access to the
mapped TUN device. Array support in OpenConnect is experimental and currently
documents basic username/password authentication.
