FROM docker.io/library/debian:trixie-slim

RUN apt-get update && apt-get install --yes --no-install-recommends \
  ca-certificates curl build-essential pkg-config libxkbcommon-dev libwayland-dev \
  xvfb x11-apps x11-utils xdotool openbox grim ffmpeg dbus dbus-x11 \
  python3 python3-gi python3-pyatspi gir1.2-gtk-3.0 libxtst6 fonts-dejavu-core \
  && rm -rf /var/lib/apt/lists/*

# Sway 1.11 introduced the window-capture protocols exercised by this fixture.
RUN printf '%s\n' 'deb http://deb.debian.org/debian forky main' \
    > /etc/apt/sources.list.d/ctream-compositor.list \
  && printf '%s\n' 'Package: *' 'Pin: release n=forky' 'Pin-Priority: 100' \
    > /etc/apt/preferences.d/ctream-compositor \
  && apt-get update \
  && apt-get install --yes --no-install-recommends -t forky sway \
  && dpkg --compare-versions "$(dpkg-query -W -f='${Version}' sway)" ge 1.11 \
  && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup.sh \
  && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.99.0 \
  && rm /tmp/rustup.sh \
  && useradd --create-home --uid 1000 ctream-test \
  && mkdir -p /run/user/1000 /tmp/.X11-unix \
  && chown ctream-test:ctream-test /run/user/1000 \
  && chmod 700 /run/user/1000 \
  && chmod 1777 /tmp/.X11-unix

ENV PATH="/root/.cargo/bin:${PATH}" CARGO_TARGET_DIR=/build
WORKDIR /workspace
ENTRYPOINT ["bash", "/workspace/docker/ctream/test.sh"]
