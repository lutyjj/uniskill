FROM rust:slim

# C compiler needed by native deps (ring, etc.)
RUN apt-get update && apt-get install -y --no-install-recommends gcc libc6-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY rust-toolchain.toml ./
RUN rustup toolchain install --profile minimal --no-self-update
