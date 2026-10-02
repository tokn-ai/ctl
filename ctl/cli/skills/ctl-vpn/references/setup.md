# OpenConnect setup

Read this for `ctl vpn create` settings or image errors. Runtime operations
are in the parent [SKILL.md](../SKILL.md), available with `ctl skill ctl-vpn`.

## Image precondition

ctld expects the image `localhost/ctl-openconnect:local` with the current shared
heartbeat protocol. It checks compatibility before sending credentials.
From an available ctl source checkout, build or rebuild it with:

```sh
./docker/openconnect/run.sh build
```

That helper uses `docker build`. ctld can use Docker or Podman at runtime; ensure
the compatible image is available in the engine ctld selects. The source helper
is not part of an installed ctl binary. If the checkout is unavailable, obtain
the matching source/image instead of assuming this relative path exists.
An image error calls for building the image; it does not call for replacing the
VPN's credentials or disabling certificate verification.

## Saved OpenConnect settings

Run `ctl vpn create` in an interactive terminal and choose OpenConnect. The
questionnaire asks for a display name, gateway address, username, masked password,
optional authentication method, and optional IPv4 connectivity-check target. Creation
saves the profile privately without starting ctld or a container. Use
`CTL_VPNS_PATH` to select a catalog other than `~/.tokn/ctl/vpns.json`. Cancel
with Ctrl-C or Esc to leave the catalog unchanged.

A gateway address uses HTTPS; an HTTPS URL may include its port and path. The
authentication method is the gateway's Array login `method` field. Server
certificate verification remains enabled. Enter passwords literally in the
masked prompt; never put a secret into command arguments or diagnostic output.

Start the saved profile with `ctl vpn start NAME_OR_ID`. The optional target
requests an SSH host-key handshake on port 22 after startup. Leaving it empty or
a failing probe does not prevent the VPN/proxy from running; it does not restrict
proxy traffic to that target. The proxy follows the container's routing table,
including a full-tunnel default route if supplied by the VPN.

ctld sends the saved configuration privately over attached stdin. The container
generates its environment file in tmpfs; that file disappears when the container
exits. Release all interested daemons before restarting with changed routing
settings.
