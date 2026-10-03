# syntax=docker/dockerfile:1

FROM rust:1.97-slim-bookworm AS builder

WORKDIR /workspace
COPY . .
RUN --mount=type=cache,id=ctmux-cargo-registry,target=/usr/local/cargo/registry \
  --mount=type=cache,id=ctmux-cargo-target,target=/workspace/target \
  cargo build --locked --release -p ctl-agent -p ctl-cli -p ctmuxd -p ctl-taskd -p ctld \
  && install -D -m 0755 /workspace/target/release/ctl-agent /out/ctl-agent \
  && install -D -m 0755 /workspace/target/release/ctl /out/ctl \
  && install -D -m 0755 /workspace/target/release/ctmuxd /out/ctmuxd \
  && install -D -m 0755 /workspace/target/release/ctl-taskd /out/ctl-taskd \
  && install -D -m 0755 /workspace/target/release/ctld /out/ctld

FROM debian:bookworm-slim AS runtime

ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
  && apt-get install --yes --no-install-recommends \
    ca-certificates \
    openssh-server \
    tini \
  && rm -rf /var/lib/apt/lists/* \
  && rm -f /etc/ssh/ssh_host_* \
  && useradd --create-home --uid 1000 --shell /bin/sh ctmux \
  && usermod --password x ctmux

ENV CTMUX_RUNTIME_DIR=/run/ctmux \
  CTL_TASKD_RUNTIME_DIR=/run/ctl-taskd \
  CTL_TASKD_DATA_DIR=/var/lib/ctl-taskd

COPY --from=builder /out/ctl-agent /usr/local/bin/ctl-agent
COPY --from=builder /out/ctl /usr/local/bin/ctl
COPY --from=builder /out/ctmuxd /usr/local/bin/ctmuxd
COPY --from=builder /out/ctl-taskd /usr/local/bin/ctl-taskd
COPY --from=builder /out/ctld /usr/local/bin/ctld
COPY docker/ctmux/entrypoint.sh /usr/local/libexec/ctmux-entrypoint
COPY docker/ctmux/forced-command.sh /usr/local/libexec/ctmux-forced-command
COPY docker/ctmux/sshd_config /etc/ssh/sshd_config

RUN chmod 0755 \
  /usr/local/bin/ctl-agent \
  /usr/local/bin/ctl \
  /usr/local/bin/ctmuxd \
  /usr/local/bin/ctl-taskd \
  /usr/local/bin/ctld \
  /usr/local/libexec/ctmux-entrypoint \
  /usr/local/libexec/ctmux-forced-command

EXPOSE 22
VOLUME ["/etc/ssh/host_keys", "/var/lib/ctl-taskd", "/home/ctmux/.tokn/ctl"]

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
  CMD ["/usr/sbin/sshd", "-t", "-f", "/etc/ssh/sshd_config"]

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/libexec/ctmux-entrypoint"]
