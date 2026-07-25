# Stage 1: Chef (base image with cargo-chef installed)
FROM lukemathwalker/cargo-chef:latest-rust-alpine AS chef

RUN apk add --no-cache musl-dev git
RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /app

# Stage 2: Planner — compute the dependency recipe from the workspace
FROM chef AS planner

COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/

RUN cargo chef prepare --recipe-path recipe.json

# Stage 3: Build — cook dependencies first (cached layer), then build source
FROM chef AS build

COPY --from=planner /app/recipe.json recipe.json

# Build dependencies only; this layer is cached unless Cargo.toml/Cargo.lock change
RUN cargo chef cook --release --target x86_64-unknown-linux-musl --package clausura-cli --recipe-path recipe.json

# Copy the actual source and build the binary as a static musl binary
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/

RUN cargo build --release --target x86_64-unknown-linux-musl --package clausura-cli

# Stage 4: Runtime
FROM alpine:latest

RUN apk add --no-cache ca-certificates git

LABEL org.opencontainers.image.source="https://github.com/liuyanghejerry/Clausura"

COPY --from=build /app/target/x86_64-unknown-linux-musl/release/clausura /usr/local/bin/clausura

WORKDIR /workspace

ENTRYPOINT ["/usr/local/bin/clausura"]
CMD ["--help"]
