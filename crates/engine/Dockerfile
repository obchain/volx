# syntax=docker/dockerfile:1.7
#
# Build the engine binary (issue #64). Same multi-stage cargo-chef
# pattern as `crates/ingestion/Dockerfile` — see that file's header
# for the shared rationale (context-from-workspace-root, cargo-chef
# version pin, distroless final).

# ─── chef base ───────────────────────────────────────────────────────
FROM rust:1.89-slim-bookworm AS chef
WORKDIR /build
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config ca-certificates \
 && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked --version 0.1.71

# ─── planner ─────────────────────────────────────────────────────────
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

# ─── builder ─────────────────────────────────────────────────────────
FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --bin volx-engine

# ─── runtime ─────────────────────────────────────────────────────────
FROM gcr.io/distroless/cc-debian12 AS runtime
COPY --from=builder /build/target/release/volx-engine /app/volx-engine
# Prometheus exporter port (METRICS_BIND default 127.0.0.1:9101; in
# compose the binary is reconfigured to bind 0.0.0.0:9101 via env).
EXPOSE 9101
ENTRYPOINT ["/app/volx-engine"]
