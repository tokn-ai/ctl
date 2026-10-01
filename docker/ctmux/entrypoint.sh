#!/bin/sh
set -eu

authorized_keys_source="${CTMUX_AUTHORIZED_KEYS_FILE:-/run/secrets/ctmux_authorized_keys}"
authorized_keys_target="/home/ctmux/.ssh/authorized_keys"
host_key="/etc/ssh/host_keys/ssh_host_ed25519_key"

if [ ! -r "$authorized_keys_source" ]; then
  echo "ctmux container: authorized keys are not readable at $authorized_keys_source" >&2
  exit 64
fi

install -d -m 0700 -o ctmux -g ctmux \
  /home/ctmux/.ssh /home/ctmux/.tokn/ctl /run/ctmux /run/ctl-taskd /var/lib/ctl-taskd
install -d -m 0700 /etc/ssh/host_keys /run/sshd
install -m 0600 -o ctmux -g ctmux "$authorized_keys_source" "$authorized_keys_target"

if ! ssh-keygen -l -f "$authorized_keys_target" >/dev/null; then
  echo "ctmux container: authorized keys file contains no valid SSH public key" >&2
  exit 65
fi

if [ ! -f "$host_key" ]; then
  ssh-keygen -q -t ed25519 -N "" -f "$host_key"
fi

chmod 0600 "$host_key"
chmod 0644 "$host_key.pub"

exec /usr/sbin/sshd -D -e -f /etc/ssh/sshd_config
