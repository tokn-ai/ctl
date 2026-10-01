FROM debian:trixie-slim

LABEL io.ctl.vpn.protocol="1"

RUN apt-get update \
  && apt-get install --yes --no-install-recommends \
    ca-certificates \
    iproute2 \
    microsocks \
    openconnect \
    openssh-client \
    vpnc-scripts \
  && rm -rf /var/lib/apt/lists/*

COPY docker/openconnect/entrypoint.sh /usr/local/bin/openconnect-entrypoint
COPY docker/openconnect/ssh-handshake.sh /usr/local/bin/ssh-handshake
COPY docker/openconnect/vpn-network.sh /usr/local/bin/vpn-network
COPY docker/openconnect/healthcheck.sh /usr/local/bin/vpn-healthcheck
COPY docker/vpn/heartbeat.sh /run/ctl/heartbeat.sh
COPY docker/vpn/watchdog.sh /run/ctl/watchdog.sh
RUN chmod 0755 /usr/local/bin/openconnect-entrypoint \
    /usr/local/bin/ssh-handshake /usr/local/bin/vpn-network \
    /usr/local/bin/vpn-healthcheck

EXPOSE 1080/tcp
HEALTHCHECK --interval=15s --timeout=3s --start-period=70s --retries=2 \
  CMD ["/usr/local/bin/vpn-healthcheck"]

ENTRYPOINT ["/usr/local/bin/openconnect-entrypoint"]
