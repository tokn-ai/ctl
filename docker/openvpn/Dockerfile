FROM debian:trixie-slim

LABEL io.ctl.vpn.protocol="2"

RUN apt-get update \
  && apt-get install --yes --no-install-recommends \
    ca-certificates iproute2 microsocks nftables openvpn util-linux \
  && rm -rf /var/lib/apt/lists/* \
  && mkdir -p /run/ctl/heartbeat /run/openvpn /run/secrets

COPY --chmod=0755 docker/openvpn/entrypoint.sh /usr/local/bin/openvpn-entrypoint
COPY --chmod=0755 docker/openvpn/vpn-network.sh /usr/local/bin/openvpn-network
COPY --chmod=0755 docker/openvpn/healthcheck.sh /usr/local/bin/vpn-healthcheck
COPY ctl/daemon/assets/vpn/heartbeat.sh /run/ctl/heartbeat.sh
COPY ctl/daemon/assets/vpn/watchdog.sh /run/ctl/watchdog.sh

EXPOSE 1080/tcp
HEALTHCHECK --interval=15s --timeout=3s --start-period=70s --retries=2 \
  CMD ["/usr/local/bin/vpn-healthcheck"]

ENTRYPOINT ["/usr/local/bin/openvpn-entrypoint"]
