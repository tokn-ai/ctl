FROM debian:trixie-slim

RUN apt-get update \
  && apt-get install --yes --no-install-recommends \
    ca-certificates \
    iproute2 \
    microsocks \
    openconnect \
    openssh-client \
    vpnc-scripts \
  && rm -rf /var/lib/apt/lists/*

COPY entrypoint.sh /usr/local/bin/openconnect-entrypoint
COPY ssh-handshake.sh /usr/local/bin/ssh-handshake
COPY vpn-network.sh /usr/local/bin/vpn-network
COPY healthcheck.sh /usr/local/bin/vpn-healthcheck
RUN chmod 0755 /usr/local/bin/openconnect-entrypoint \
    /usr/local/bin/ssh-handshake /usr/local/bin/vpn-network \
    /usr/local/bin/vpn-healthcheck

EXPOSE 1080/tcp
HEALTHCHECK --interval=15s --timeout=3s --start-period=70s --retries=2 \
  CMD ["/usr/local/bin/vpn-healthcheck"]

ENTRYPOINT ["/usr/local/bin/openconnect-entrypoint"]
