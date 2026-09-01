# syntax=docker/dockerfile:1

FROM rust:1.97-slim-bookworm AS builder

WORKDIR /workspace
COPY . .
RUN --mount=type=cache,id=rmux-cargo-registry,target=/usr/local/cargo/registry \
  --mount=type=cache,id=rmux-cargo-target,target=/workspace/target \
  cargo build --locked --release -p ctld -p rmuxd \
  && install -D -m 0755 /workspace/target/release/ctld /out/ctld \
  && install -D -m 0755 /workspace/target/release/rmuxd /out/rmuxd

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

COPY --from=builder /out/ctld /usr/local/bin/ctld
COPY --from=builder /out/rmuxd /usr/local/bin/rmuxd
COPY docker/rmux/entrypoint.sh /usr/local/libexec/rmux-entrypoint
COPY docker/rmux/forced-command.sh /usr/local/libexec/rmux-forced-command
COPY docker/rmux/sshd_config /etc/ssh/sshd_config

RUN chmod 0755 \
  /usr/local/bin/ctld \
  /usr/local/bin/rmuxd \
  /usr/local/libexec/rmux-entrypoint \
  /usr/local/libexec/rmux-forced-command

EXPOSE 22
VOLUME ["/etc/ssh/host_keys"]

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
  CMD ["/usr/sbin/sshd", "-t", "-f", "/etc/ssh/sshd_config"]

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/libexec/rmux-entrypoint"]
