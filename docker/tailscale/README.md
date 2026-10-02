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
**Connect through** step; ctmux retains the profile ID and resolves the current
SOCKS5 port whenever it opens a transport. Subnet routes are optional; an exit node
is not selected. Authentication in the container is separate from any Tailscale
installation on the Mac.

Browser authentication can remain pending indefinitely. Stop works while sign-in
is pending. `ctld` also checks that the local SOCKS5 listener is available before
reporting a usable endpoint. If Tailscale needs another login, the endpoint is
withdrawn and a new sign-in link becomes available.

Inspect saved profiles and runtime connections with `ctl vpn list`. To reconnect
a saved Tailscale profile after its container is removed, use
`ctl vpn connect NAME_OR_ID`. An exact profile ID takes precedence over a unique
exact name. Connect starts ctld when needed and reuses the retained identity
volume when recreating the container. Follow any returned sign-in link, then run
list to check the current state and SOCKS5 endpoint. List never starts ctld or
renews a heartbeat; a saved profile is unavailable when runtime inventory cannot
be checked. With complete inventory, an unmatched saved profile is disconnected.

For a new CLI-only identity, use `ctl vpn start-tailscale --id my-tailnet` and
reuse that ID on later starts. Connect, start-tailscale, list, and stop support
`--json`. List returns sanitized merged `entries`, raw runtime `connections`,
capability fields, and inventory warnings. CLI-only starts do not create saved
desktop profiles.

The listener check completes a short-lived SOCKS5 UDP association and closes it
without sending any datagrams or contacting a remote host. Completing the request
avoids the `could not read packet header` errors that greeting-only checks produce
in the container logs.

Each profile uses a durable named volume for its Tailscale state. Container and
volume names are derived from a hash of the local owner and saved profile ID;
neither a private hostname nor a username is embedded in those names. Docker's
atomic container-name reservation prevents two daemons from using the same
profile's state in competing containers. Compatible daemons reuse the same
container and SOCKS5 port, renewing independent heartbeats. Protocol, user,
profile, and routing settings are checked before reuse. Cleanup addresses the
immutable container ID and never force-removes a running shared container.

Inventory accepts only the current heartbeat protocol and user namespace.
Containers created before shared heartbeats must be recreated; the persistent
identity volume is retained.

Cancelling an unsaved sign-in releases its local heartbeat interest. If a
container still uses the draft identity, the app reports cleanup pending and
offers Retry cleanup after watchdog expiry. Cleanup removes only that draft's
unused volume and refuses state referenced by another container.
Saved connections keep their state: disconnect releases this daemon's heartbeat
interest and retains the volume and login. The container remains available while
another daemon sends heartbeats, then removes itself after the last heartbeat
expires.
Deleting a profile also leaves its volume untouched. Do not remove that volume
unless you intend to forget its saved device identity. Starting a newly created
profile creates a separate Tailscale device. After the last daemon exits, its
container may take up to 15 seconds to stop.
Until then another daemon can reuse the compatible container without replacing it.
Containers created by older versions must first be disconnected and recreated;
the same identity volume keeps their persistent login.
If the engine is unresponsive during startup cancellation, cleanup is best effort;
retry after it recovers rather than deleting a container owned by another daemon.

The bundled entrypoint installs a watchdog using monotonic container time.
Each connected daemon renews every two seconds through an independent engine
exec request; the container exits after 15 seconds without any heartbeat. Startup
has a 15-second grace period. Status discovery never renews heartbeats, and stdin
EOF or creator CLI exit does not control lifetime. State is preserved on every
shutdown, including daemon exit. Raw Tailscale output stays out of IPC diagnostics; only validated login URLs and
selected status fields are exposed.

See the official [userspace networking documentation](https://tailscale.com/docs/concepts/userspace-networking)
and [persistent container-state settings](https://tailscale.com/docs/features/containers/docker/docker-params).
