# Stage 1: Build static binaries with musl
FROM rust:latest AS builder

RUN rustup target add x86_64-unknown-linux-musl
RUN apt-get update && apt-get install -y musl-tools && rm -rf /var/lib/apt/lists/*

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

RUN cargo build --release --target x86_64-unknown-linux-musl -p sonda -p sonda-server 2>/dev/null || true

# Copy real source and build
COPY sonda-core/ sonda-core/
COPY sonda/ sonda/
COPY sonda-server/ sonda-server/

# Touch source files to invalidate the dummy build cache
RUN touch sonda-core/src/lib.rs sonda/src/main.rs sonda-server/src/main.rs

RUN cargo build --release --target x86_64-unknown-linux-musl -p sonda -p sonda-server

# Stage 2: Minimal runtime image
FROM scratch

COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/sonda /sonda
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/sonda-server /sonda-server

EXPOSE 8080

ENTRYPOINT ["/sonda-server"]
