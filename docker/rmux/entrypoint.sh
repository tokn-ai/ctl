#!/bin/sh
set -eu

authorized_keys_source="${RMUX_AUTHORIZED_KEYS_FILE:-/run/secrets/rmux_authorized_keys}"
authorized_keys_target="/home/rmux/.ssh/authorized_keys"
host_key="/etc/ssh/host_keys/ssh_host_ed25519_key"

if [ ! -r "$authorized_keys_source" ]; then
  echo "rmux container: authorized keys are not readable at $authorized_keys_source" >&2
  exit 64
fi

install -d -m 0700 -o rmux -g rmux \
  /home/rmux/.ssh /run/rmux /run/taskd /var/lib/taskd
install -d -m 0700 /etc/ssh/host_keys /run/sshd
install -m 0600 -o rmux -g rmux "$authorized_keys_source" "$authorized_keys_target"

if ! ssh-keygen -l -f "$authorized_keys_target" >/dev/null; then
  echo "rmux container: authorized keys file contains no valid SSH public key" >&2
  exit 65
fi

if [ ! -f "$host_key" ]; then
  ssh-keygen -q -t ed25519 -N "" -f "$host_key"
fi

chmod 0600 "$host_key"
chmod 0644 "$host_key.pub"

exec /usr/sbin/sshd -D -e -f /etc/ssh/sshd_config
