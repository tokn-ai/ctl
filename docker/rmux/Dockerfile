# syntax=docker/dockerfile:1

FROM rust:1.97-slim-bookworm AS builder

WORKDIR /workspace
COPY . .
RUN --mount=type=cache,id=rmux-cargo-registry,target=/usr/local/cargo/registry \
  --mount=type=cache,id=rmux-cargo-target,target=/workspace/target \
  cargo build --locked --release -p ctl-agent -p ctl -p rmuxd -p taskd \
  && install -D -m 0755 /workspace/target/release/ctl-agent /out/ctl-agent \
  && install -D -m 0755 /workspace/target/release/ctl /out/ctl \
  && install -D -m 0755 /workspace/target/release/rmuxd /out/rmuxd \
  && install -D -m 0755 /workspace/target/release/taskd /out/taskd

FROM debian:bookworm-slim AS runtime

ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
  && apt-get install --yes --no-install-recommends \
    ca-certificates \
    openssh-server \
    tini \
  && rm -rf /var/lib/apt/lists/* \
  && rm -f /etc/ssh/ssh_host_* \
  && useradd --create-home --uid 1000 --shell /bin/sh rmux \
  && usermod --password x rmux

ENV RMUX_RUNTIME_DIR=/run/rmux \
  TASKD_RUNTIME_DIR=/run/taskd \
  TASKD_DATA_DIR=/var/lib/taskd

COPY --from=builder /out/ctl-agent /usr/local/bin/ctl-agent
COPY --from=builder /out/ctl /usr/local/bin/ctl
COPY --from=builder /out/rmuxd /usr/local/bin/rmuxd
COPY --from=builder /out/taskd /usr/local/bin/taskd
COPY docker/rmux/entrypoint.sh /usr/local/libexec/rmux-entrypoint
COPY docker/rmux/forced-command.sh /usr/local/libexec/rmux-forced-command
COPY docker/rmux/sshd_config /etc/ssh/sshd_config

RUN chmod 0755 \
  /usr/local/bin/ctl-agent \
  /usr/local/bin/ctl \
  /usr/local/bin/rmuxd \
  /usr/local/bin/taskd \
  /usr/local/libexec/rmux-entrypoint \
  /usr/local/libexec/rmux-forced-command

EXPOSE 22
VOLUME ["/etc/ssh/host_keys", "/var/lib/taskd"]

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
  CMD ["/usr/sbin/sshd", "-t", "-f", "/etc/ssh/sshd_config"]

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/libexec/rmux-entrypoint"]
