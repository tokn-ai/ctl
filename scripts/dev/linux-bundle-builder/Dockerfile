FROM node:24-bookworm-slim AS node
FROM rust:1.99.0-slim-bookworm

RUN apt-get update && apt-get install --yes --no-install-recommends \
  binutils ca-certificates git musl-tools && rm -rf /var/lib/apt/lists/*
COPY --from=node /usr/local/bin/node /usr/local/bin/node
RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /workspace
