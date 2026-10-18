#!/usr/bin/env bash
# Owns an isolated loopback sshd on an ephemeral GitHub runner only.
set -euo pipefail
if [[ "${GITHUB_ACTIONS:-}" != true || "${RUNNER_OS:-}" == Windows ]]; then
  echo 'This fixture is only for ephemeral Unix GitHub runners.' >&2
  exit 1
fi
if [[ "$RUNNER_OS" == Linux ]]; then
  sudo apt-get install --yes --no-install-recommends openssh-server
  sudo mkdir -p /run/sshd
fi
root=$(mktemp -d /tmp/ctl-caller-ssh.XXXXXX)
port=22379
ssh_user=$(id -un)
config="$HOME/.ssh/config"
mkdir -p "$HOME/.ssh"
had_config=false
if [[ -f "$config" ]]; then cp "$config" "$root/original-config"; had_config=true; fi
cleanup() {
  result=$?
  if [[ $result != 0 && -f "$root/server.log" ]]; then sudo tail -60 "$root/server.log" >&2; fi
  ssh -S "$root/master" -O exit ctl-caller-ci >/dev/null 2>&1 || true
  if [[ -f "$root/sshd.pid" ]]; then sudo kill "$(sudo cat "$root/sshd.pid")" 2>/dev/null || true; fi
  if $had_config; then cp "$root/original-config" "$config"; else rm -f "$config"; fi
  rm -rf "$root"
}
trap cleanup EXIT
for key in client wrong-client host; do ssh-keygen -q -t ed25519 -N '' -f "$root/$key"; done
cat > "$root/sshd_config" <<EOF
Port $port
ListenAddress 127.0.0.1
ListenAddress ::1
PidFile $root/sshd.pid
HostKey $root/host
AuthorizedKeysFile $root/client.pub
AllowUsers $ssh_user
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
AllowTcpForwarding no
AllowAgentForwarding no
EOF
sudo /usr/sbin/sshd -t -f "$root/sshd_config"
sudo /usr/sbin/sshd -f "$root/sshd_config" -E "$root/server.log"
host_key=$(cut -d ' ' -f 1,2 "$root/host.pub")
printf '[127.0.0.1]:%s %s\n[::1]:%s %s\n' "$port" "$host_key" "$port" "$host_key" > "$root/known_hosts"
cat > "$config" <<EOF
Host ctl-caller-ci ctl-caller-untrusted
  HostName 127.0.0.1
  User $ssh_user
  Port $port
  IdentityFile $root/client
  IdentitiesOnly yes
  UserKnownHostsFile $root/known_hosts
  StrictHostKeyChecking yes
  ControlMaster auto
  ControlPath $root/master
Host ctl-caller-untrusted
  HostKeyAlias never-trusted-caller-host
Host ::1
  UserKnownHostsFile $root/known_hosts
  StrictHostKeyChecking yes
EOF
if $had_config; then cat "$root/original-config" >> "$config"; fi
chmod 600 "$config"
# First prove that a working master and configured identity exist.
ssh -M -N -f ctl-caller-ci
ssh ctl-caller-ci echo MASTER_READY
ctl=./target/debug/ctl
set +e
printf 'CALLER_STDIN\n' | "$ctl" --destination ctl-caller-ci --identity-file "$root/client" exec -- sh -c 'cat; exit 23' > "$root/output"
code=$?
set -e
[[ $code == 23 ]] || { echo 'Remote exit status was not preserved' >&2; exit 1; }
grep -q CALLER_STDIN "$root/output"
set +e
"$ctl" --destination ctl-caller-ci --identity-file "$root/wrong-client" exec -- true
code=$?
set -e
[[ $code == 255 ]] || { echo 'Wrong supplied key reused another authentication method' >&2; exit 1; }
set +e
"$ctl" --destination ctl-caller-untrusted --identity-file "$root/client" exec -- true 2> "$root/untrusted-error"
code=$?
set -e
[[ $code == 255 ]] || { echo 'Unknown host key was accepted' >&2; exit 1; }
grep -qi 'host key verification failed' "$root/untrusted-error"
"$ctl" --destination "$ssh_user@::1" --port "$port" --identity-file "$root/client" exec -- echo IPV6_CALLER_AUTHENTICATED
set +e
printf 'echo CALLER_SHELL\nexit 19\n' | "$ctl" --destination ctl-caller-ci --identity-file "$root/client" shell --plain > "$root/shell-output"
code=$?
set -e
[[ $code == 19 ]] || { echo 'Plain shell exit status was not preserved' >&2; exit 1; }
grep -q CALLER_SHELL "$root/shell-output"
