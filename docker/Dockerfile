FROM rust:latest

# C compiler needed by native deps (ring, etc.)
RUN apt-get update && apt-get install -y --no-install-recommends gcc libc6-dev && rm -rf /var/lib/apt/lists/*
