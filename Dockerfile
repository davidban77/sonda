# syntax=docker/dockerfile:1

# Multi-arch Dockerfile for sonda
#
# Supports linux/amd64 and linux/arm64 via docker buildx.
# Uses TARGETARCH (set automatically by buildx) to select the correct
# Rust target triple and cross-compilation toolchain.
#
# Usage:
#   docker build -t sonda .                                              # native arch
#   docker buildx build --platform linux/amd64,linux/arm64 -t sonda .   # multi-arch

# Stage 1: Build static binaries with musl
FROM rust:latest AS builder

# TARGETARCH is set by docker buildx (amd64, arm64, etc.)
ARG TARGETARCH

# Install cross-compilation toolchain based on target architecture.
# For amd64: musl-tools provides the native musl-gcc wrapper.
# For arm64: we use gcc-aarch64-linux-gnu as the linker and download
# musl headers/libs from the official musl.org release (not musl.cc).
RUN apt-get update && \
    apt-get install -y musl-tools && \
    if [ "${TARGETARCH}" = "arm64" ]; then \
      apt-get install -y gcc-aarch64-linux-gnu; \
    fi && \
    rm -rf /var/lib/apt/lists/*

# Set up Rust target and cross-compilation config
RUN case "${TARGETARCH}" in \
      amd64) echo "x86_64-unknown-linux-musl" > /tmp/rust-target ;; \
      arm64) echo "aarch64-unknown-linux-musl" > /tmp/rust-target ;; \
      *) echo "Unsupported architecture: ${TARGETARCH}" && exit 1 ;; \
    esac && \
    RUST_TARGET=$(cat /tmp/rust-target) && \
    rustup target add "${RUST_TARGET}" && \
    if [ "${TARGETARCH}" = "arm64" ]; then \
      mkdir -p /root/.cargo && \
      printf '[target.aarch64-unknown-linux-musl]\nlinker = "aarch64-linux-gnu-gcc"\n' \
        >> /root/.cargo/config.toml && \
      echo 'CC_aarch64_unknown_linux_musl=aarch64-linux-gnu-gcc' > /tmp/cross-env && \
      echo 'AR_aarch64_unknown_linux_musl=aarch64-linux-gnu-ar' >> /tmp/cross-env && \
      echo 'CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc' >> /tmp/cross-env; \
    else \
      touch /tmp/cross-env; \
    fi

WORKDIR /build

# Copy manifests first for better layer caching
COPY Cargo.toml Cargo.lock ./
COPY sonda-core/Cargo.toml sonda-core/Cargo.toml
COPY sonda/Cargo.toml sonda/Cargo.toml
COPY sonda-server/Cargo.toml sonda-server/Cargo.toml

# Create dummy source files so cargo can fetch and cache dependencies
RUN mkdir -p sonda-core/src sonda/src sonda-server/src && \
    echo "pub fn dummy() {}" > sonda-core/src/lib.rs && \
    echo "fn main() {}" > sonda/src/main.rs && \
    echo "fn main() {}" > sonda-server/src/main.rs

RUN RUST_TARGET=$(cat /tmp/rust-target) && \
    if [ -s /tmp/cross-env ]; then export $(cat /tmp/cross-env); fi && \
    cargo build --release --target "${RUST_TARGET}" --features remote-write,kafka,otlp -p sonda -p sonda-server 2>/dev/null || true

# Copy real source and build
COPY sonda-core/ sonda-core/
COPY sonda/ sonda/
COPY sonda-server/ sonda-server/

# Touch source files to invalidate the dummy build cache
RUN touch sonda-core/src/lib.rs sonda/src/main.rs sonda-server/src/main.rs

RUN RUST_TARGET=$(cat /tmp/rust-target) && \
    if [ -s /tmp/cross-env ]; then export $(cat /tmp/cross-env); fi && \
    cargo build --release --target "${RUST_TARGET}" --features remote-write,kafka,otlp -p sonda -p sonda-server

# Copy binaries to a known location regardless of target triple
RUN RUST_TARGET=$(cat /tmp/rust-target) && \
    mkdir -p /out && \
    cp "target/${RUST_TARGET}/release/sonda" /out/sonda && \
    cp "target/${RUST_TARGET}/release/sonda-server" /out/sonda-server

# Stage 2: Minimal runtime image
FROM scratch

COPY --from=builder /out/sonda /sonda
COPY --from=builder /out/sonda-server /sonda-server

EXPOSE 8080

ENTRYPOINT ["/sonda-server"]
