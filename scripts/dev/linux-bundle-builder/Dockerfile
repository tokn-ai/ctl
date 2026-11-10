FROM node:24-bookworm-slim AS node
FROM rust:1.99.0-slim-bookworm

RUN apt-get update && apt-get install --yes --no-install-recommends \
  binutils ca-certificates git make musl-tools perl && rm -rf /var/lib/apt/lists/*
COPY --from=node /usr/local/bin/node /usr/local/bin/node
ARG CTL_BUILD_TARGET=x86_64-unknown-linux-musl
RUN rustup target add "${CTL_BUILD_TARGET}"

WORKDIR /workspace
