# syntax=docker/dockerfile:1.7
#
# Build the ingestion binary (issue #64). Multi-stage with cargo-chef
# for dep-only caching: a change in `crates/ingestion/src/**` re-uses
# the cached `cargo build --release` of every workspace dependency,
# turning per-edit builds from ~6 min into ~30 s.
#
# Build context **must** be the workspace root so the Cargo workspace
# (`Cargo.toml` + `crates/`) is visible to the planner. Compose
# wires this with `context: ..` (the build is invoked from
# `crates/ingestion/Dockerfile`).
#
# Final image: gcr.io/distroless/cc-debian12 → ~30 MB. Runs as
# `nonroot:nonroot` by default. No shell — debug with `docker
# compose exec` against a sibling container if you need one.

# ─── chef base ───────────────────────────────────────────────────────
# Pinned MSRV (workspace `rust-version = "1.89"`). Bumping past 1.89
# requires the same bump in `Cargo.toml` + CI matrix.
FROM rust:1.89-slim-bookworm AS chef
WORKDIR /build
# `pkg-config` + `ca-certificates` cover the small set of build-time
# system deps used by the rustls / metrics-exporter-prometheus stack.
# OpenSSL is not needed — every TLS path in the workspace uses
# rustls + ring.
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config ca-certificates \
 && rm -rf /var/lib/apt/lists/*
# Pin cargo-chef so a transitive upstream change cannot break the
# planner-recipe wire format mid-CI.
RUN cargo install cargo-chef --locked --version 0.1.71

# ─── planner: hash the manifest into a recipe ────────────────────────
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

# ─── builder: build deps from the recipe, then the bin ───────────────
FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
# Builds every workspace dependency into the build cache. Only the
# `recipe.json` is needed at this step — source changes do not bust
# the layer cache here.
RUN cargo chef cook --release --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --bin volx-ingestion

# ─── runtime: distroless, single binary ──────────────────────────────
FROM gcr.io/distroless/cc-debian12 AS runtime
COPY --from=builder /build/target/release/volx-ingestion /app/volx-ingestion
# Prometheus exporter port (METRICS_BIND default 127.0.0.1:9100; in
# compose the binary is reconfigured to bind 0.0.0.0:9100 via env).
EXPOSE 9100
ENTRYPOINT ["/app/volx-ingestion"]
