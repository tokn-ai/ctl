# Tailscale container

Tailscale is a VPN provider managed through the same `ctl` → `ctld` interface as
OpenConnect. Add Tailscale on the app's VPN page and sign in through your browser
before saving the profile. Approval may also be required in your tailnet's
administration console. Saving keeps the same authenticated device identity;
no auth key is stored in the profile.

The first start downloads the pinned official image
`docker.io/tailscale/tailscale:v1.94.2` if it is missing. Docker or Podman must be
running. There is no local image build step.

The container uses Tailscale's userspace networking and SOCKS5 listener. It does
not need a TUN device, elevated networking capabilities, or host routes. A random
host port is published on loopback only. Select the saved profile in a host's
**Connect through** step; rmux retains the profile ID and resolves the current
SOCKS5 port whenever it opens a transport. Subnet routes are optional; an exit node
is not selected. Authentication in the container is separate from any Tailscale
installation on the Mac.

Browser authentication can remain pending indefinitely. Stop works while sign-in
is pending. `ctld` also checks that the local SOCKS5 listener is available before
reporting a usable endpoint. If Tailscale needs another login, the endpoint is
withdrawn and a new sign-in link becomes available.

Each profile uses a durable named volume for its Tailscale state. Container and
volume names are derived from a hash of the local owner and saved profile ID;
neither a private hostname nor a username is embedded in those names. Docker's
atomic container-name reservation prevents two daemons from using the same
profile's state concurrently. A random lease label is verified before adopting
the container, and cleanup addresses its immutable container ID.

Cancelling an unsaved sign-in stops its container and removes only that draft's
identity volume. Cleanup refuses to remove state referenced by another container.
Saved connections keep their state: stopping one removes its container but retains
the volume and login.
Deleting a profile also leaves its volume untouched. Do not remove that volume
unless you intend to forget its saved device identity. Starting a newly created
profile creates a separate Tailscale device. A container from an owner that was
forcibly killed may take up to 15 seconds to stop before a replacement can start.
If the engine is unresponsive during startup cancellation, cleanup is best effort;
retry after it recovers rather than deleting a container owned by another daemon.

The bundled entrypoint receives heartbeats on stdin and exits on EOF or a missed
heartbeat. It preserves state on every shutdown, including daemon exit. Raw
Tailscale output stays out of IPC diagnostics; only validated login URLs and
selected status fields are exposed.

See the official [userspace networking documentation](https://tailscale.com/docs/concepts/userspace-networking)
and [persistent container-state settings](https://tailscale.com/docs/features/containers/docker/docker-params).
