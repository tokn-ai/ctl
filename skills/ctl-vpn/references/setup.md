# OpenConnect setup

Read this for `ctl vpn start` configuration or image errors. Runtime operations
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

## Private literal settings

Use an existing configured private file when supplied. For a new file, store
the user's actual settings locally using these keys:

```dotenv
VPN_URL=https://vpn.example.com
VPN_USERNAME=alice
VPN_PASSWORD=replace-with-local-secret
# VPN_AUTH_METHOD=method-name
# TARGET_IP=10.0.0.20
```

The parser accepts only `VPN_URL`, `VPN_USERNAME`, `VPN_PASSWORD`,
`VPN_AUTH_METHOD`, and `TARGET_IP`. URL, username, and a nonempty password are
required. The optional authentication method is the gateway's Array login
`method` field. A bare gateway address uses HTTPS; an HTTPS URL may include its
port and path. Certificate verification remains enabled.

Values are literal single lines: no shell expansion, shell quotes, escapes,
whitespace trimming, or inline comments. For example, `$`, `#`, and spaces in a
password are copied directly after `VPN_PASSWORD=`. Blank lines and lines
starting with `#` are ignored; duplicate keys use the last value. Do not source
the file or put a secret into command arguments or diagnostic output.

The file must be regular, private, valid UTF-8, and at most 48 KiB. Carriage
returns and NUL are rejected, so use LF line endings. Set its permissions before
starting:

```sh
chmod 600 /absolute/path/to/company.env
ctl vpn start --env-file /absolute/path/to/company.env --json
```

`TARGET_IP` optionally requests an SSH host-key handshake on port 22 after
startup. Leaving it empty or a failing probe does not prevent the VPN/proxy from
running; it does not restrict proxy traffic to that target. The proxy follows
the container's routing table, including a full-tunnel default route if supplied
by the VPN. ctld sends a private bounded snapshot over attached stdin and the
container stores it in tmpfs; editing the original file requires releasing and
starting the connection again to apply new settings.
