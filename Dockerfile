# Stage 1: Build
FROM rust:alpine AS build

# Install musl toolchain and add the musl target
RUN apk add --no-cache musl-dev
RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /app

# Copy manifests for dependency caching
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/

# Build the binary as a static musl binary
RUN cargo build --release --target x86_64-unknown-linux-musl --package clausura-cli

# Stage 2: Runtime
FROM alpine:latest

RUN apk add --no-cache ca-certificates git

LABEL org.opencontainers.image.source="https://github.com/liuyanghejerry/Clausura"

COPY --from=build /app/target/x86_64-unknown-linux-musl/release/clausura /usr/local/bin/clausura

WORKDIR /workspace

ENTRYPOINT ["/usr/local/bin/clausura"]
CMD ["--help"]
