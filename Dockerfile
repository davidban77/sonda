# syntax=docker/dockerfile:1

# Multi-arch Dockerfile for sonda
#
# Supports linux/amd64 and linux/arm64 via docker buildx.
# Uses TARGETARCH (set automatically by buildx) to select the correct
# Rust target triple and cross-compilation toolchain.
#
# BUILD_SOURCE selects where the binaries come from:
#   builder  (default) compiles the workspace from the build context.
#   prebuilt copies already-built binaries out of the build context, laid
#            out as dist/<TARGETARCH>/{sonda,sonda-server}. The release
#            workflow uses this to ship the exact binaries it published as
#            tarball assets, so the image and the tarballs cannot diverge.
#
# FEATURES selects the cargo feature set the builder stage compiles with. The
# default is the set the release publishes, so `docker build .` produces what
# ships; anything else has to be typed.
#
# Usage:
#   docker build -t sonda .                                              # native arch
#   docker buildx build --platform linux/amd64,linux/arm64 -t sonda .   # multi-arch
#   docker buildx build --build-arg BUILD_SOURCE=prebuilt -t sonda .    # released binaries
#   docker build --build-arg FEATURES=remote-write,kafka,otlp -t sonda . # plus OTLP

ARG BUILD_SOURCE=builder

# Stage 1: Build static binaries with musl
#
# Pinned to BUILDPLATFORM. Everything below installs a cross-compilation
# toolchain and builds for TARGETARCH; without the pin, buildx runs the whole
# stage under QEMU for a foreign target platform, emulating the very compile
# the cross toolchain exists to avoid.
FROM --platform=$BUILDPLATFORM rust:latest AS builder

# TARGETARCH and BUILDARCH are set by docker buildx (amd64, arm64, etc.)
ARG TARGETARCH
ARG BUILDARCH
ARG FEATURES=remote-write,kafka

# Install cross-compilation toolchain based on target architecture.
# For amd64: musl-tools provides the native musl-gcc wrapper.
# For arm64: we use gcc-aarch64-linux-gnu as the linker and download
# musl headers/libs from the official musl.org release (not musl.cc).
#
# musl-gcc only wraps the build host's own gcc, so it cannot produce amd64
# objects on an arm64 host — an amd64 target from an arm64 machine needs the
# same treatment the arm64 target already gets, in the other direction.
RUN apt-get update && \
    apt-get install -y musl-tools && \
    if [ "${TARGETARCH}" = "arm64" ]; then \
      apt-get install -y gcc-aarch64-linux-gnu; \
    elif [ "${TARGETARCH}" = "amd64" ] && [ "${BUILDARCH}" != "amd64" ]; then \
      apt-get install -y gcc-x86-64-linux-gnu; \
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
    elif [ "${TARGETARCH}" = "amd64" ] && [ "${BUILDARCH}" != "amd64" ]; then \
      mkdir -p /root/.cargo && \
      printf '[target.x86_64-unknown-linux-musl]\nlinker = "x86_64-linux-gnu-gcc"\n' \
        >> /root/.cargo/config.toml && \
      echo 'CC_x86_64_unknown_linux_musl=x86_64-linux-gnu-gcc' > /tmp/cross-env && \
      echo 'AR_x86_64_unknown_linux_musl=x86_64-linux-gnu-ar' >> /tmp/cross-env && \
      echo 'CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-gnu-gcc' >> /tmp/cross-env; \
    else \
      touch /tmp/cross-env; \
    fi

WORKDIR /build

# Copy manifests first for better layer caching
COPY Cargo.toml Cargo.lock ./
COPY sonda-core/Cargo.toml sonda-core/Cargo.toml
COPY sonda/Cargo.toml sonda/Cargo.toml
COPY sonda-server/Cargo.toml sonda-server/Cargo.toml
COPY sonda-wasm/Cargo.toml sonda-wasm/Cargo.toml

# Create dummy source files so cargo can fetch and cache dependencies
RUN mkdir -p sonda-core/src sonda/src sonda-server/src sonda-wasm/src && \
    echo "pub fn dummy() {}" > sonda-core/src/lib.rs && \
    echo "fn main() {}" > sonda/src/main.rs && \
    echo "fn main() {}" > sonda-server/src/main.rs && \
    echo "pub fn dummy() {}" > sonda-wasm/src/lib.rs

RUN RUST_TARGET=$(cat /tmp/rust-target) && \
    if [ -s /tmp/cross-env ]; then export $(cat /tmp/cross-env); fi && \
    cargo build --release --target "${RUST_TARGET}" --features "${FEATURES}" -p sonda -p sonda-server 2>/dev/null || true

# Copy real source and build.
#
# `packs/` is source as far as the build is concerned: sonda-core embeds those
# files with `include_str!("../../../packs/…")`, so the crate does not compile
# without them in the context. It is not only the compose mount.
COPY packs/ packs/
COPY sonda-core/ sonda-core/
COPY sonda/ sonda/
COPY sonda-server/ sonda-server/
COPY sonda-wasm/ sonda-wasm/

# Touch source files to invalidate the dummy build cache
RUN touch sonda-core/src/lib.rs sonda/src/main.rs sonda-server/src/main.rs sonda-wasm/src/lib.rs

RUN RUST_TARGET=$(cat /tmp/rust-target) && \
    if [ -s /tmp/cross-env ]; then export $(cat /tmp/cross-env); fi && \
    cargo build --release --target "${RUST_TARGET}" --features "${FEATURES}" -p sonda -p sonda-server

# Copy binaries to a known location regardless of target triple
RUN RUST_TARGET=$(cat /tmp/rust-target) && \
    mkdir -p /out && \
    cp "target/${RUST_TARGET}/release/sonda" /out/sonda && \
    cp "target/${RUST_TARGET}/release/sonda-server" /out/sonda-server

# UID 65532 = upstream "nonroot" convention (distroless/nonroot, chainguard)
RUN echo 'sonda:x:65532:65532::/:' > /tmp/passwd.sonda

# Stage 1 (alternative): binaries built elsewhere, keyed on TARGETARCH so one
# multi-platform build picks the right one per leg.
#
# Presents the same interface as `builder` — /out/sonda, /out/sonda-server and
# the passwd line — so the runtime stage is identical for either source. There
# is deliberately no fallback to compiling: a missing dist/ must fail the build
# at COPY rather than quietly rebuild the workspace.
FROM --platform=$BUILDPLATFORM scratch AS prebuilt

ARG TARGETARCH

COPY --chmod=0755 dist/${TARGETARCH}/sonda /out/sonda
COPY --chmod=0755 dist/${TARGETARCH}/sonda-server /out/sonda-server

# UID 65532 = upstream "nonroot" convention (distroless/nonroot, chainguard)
COPY <<EOF /tmp/passwd.sonda
sonda:x:65532:65532::/:
EOF

FROM ${BUILD_SOURCE} AS chosen

# Stage 2: Minimal runtime image
FROM scratch

COPY --from=chosen /out/sonda /sonda
COPY --from=chosen /out/sonda-server /sonda-server
COPY --from=chosen /tmp/passwd.sonda /etc/passwd

USER 65532:65532

EXPOSE 8080

ENTRYPOINT ["/sonda-server"]
