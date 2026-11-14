# OpenVPN with SOCKS5

Build the local OpenVPN client image with Docker or Podman.

```sh
./docker/openvpn/run.sh build
```

This builds `localhost/ctl-openvpn:local` from Debian Trixie with the distro's
OpenVPN 2.6 package. Build the image on the machine where `ctld` will run the
VPN. Its Linux container engine needs access to `/dev/net/tun` and support for
nftables. Docker Desktop and Podman Machine use their Linux VM for both.

In the desktop VPN editor, add **OpenVPN**, select an `.ovpn` file, and save
the profile. From the CLI, run `ctl vpn create` and select **OpenVPN** to
import a file into the same private profile catalog. Provide a username and
password when the profile needs them. Credentials already present in an
`<auth-user-pass>` block are accepted.

```sh
ctl vpn start NAME_OR_ID
ctl vpn list
ctl vpn stop NAME_OR_ID
```

A profile must be a self-contained TLS client configuration for a TUN tunnel,
at most 24 KiB. Embed certificates, keys, and other authentication material
in the profile. OpenVPN profiles with executable hooks, external file
references, TAP, or interactive authentication need to be exported again in a
supported form. The profile catalog can contain passwords and private keys.
Keep both the catalog and the original profile private.

The desktop can use the saved profile as a **Connect through** route. When the
VPN is ready, `ctl vpn list --json` shows its loopback SOCKS5 endpoint. SOCKS5
supports TCP and resolves names in the VPN container, including DNS servers
pushed by the VPN. The proxy allows outgoing traffic only where the container
routes it through the tunnel. For split tunnels, the VPN routes must cover the
destination and any DNS servers used for resolution.

Stopping a VPN releases this daemon's heartbeat. A second daemon using the
profile can keep the shared container running. After the last heartbeat expires,
the container removes itself. A tunnel error stops the container so a subsequent
start can reconnect. A negotiated OpenVPN-capable daemon is required to start
the profile or view its runtime status.
