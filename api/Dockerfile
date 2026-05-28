# syntax=docker/dockerfile:1.7
#
# Build the Go API (issue #64). Multi-stage:
#   - dep-cache stage: go.sum-only COPY → `go mod download` cached
#     until a dependency actually changes
#   - builder stage:   full source COPY + static build
#   - runtime stage:   gcr.io/distroless/static-debian12 (~5 MB +
#                      the binary itself)
#
# Build context **must** be the workspace root so the relative paths
# (`api/...`) resolve. Compose wires this with `context: ..`.
#
# Static binary (`CGO_ENABLED=0`) — runs on the `static` distroless
# variant with no glibc, just ca-certs + tzdata.

# ─── dep cache: pin module graph independently of source edits ───────
FROM golang:1.25-alpine AS deps
WORKDIR /build
COPY api/go.mod api/go.sum ./
RUN go mod download

# ─── builder ─────────────────────────────────────────────────────────
FROM golang:1.25-alpine AS builder
WORKDIR /build
# Re-use the warm module cache from the deps stage.
COPY --from=deps /go/pkg /go/pkg
COPY --from=deps /build/go.mod /build/go.sum ./
COPY api/ ./
# Static build:
#   CGO_ENABLED=0   — no glibc, runs on distroless/static
#   -trimpath       — strip absolute paths (smaller + reproducible)
#   -ldflags="-s -w"— strip DWARF + symbol table (smaller image)
RUN CGO_ENABLED=0 GOOS=linux \
    go build \
      -trimpath \
      -ldflags="-s -w" \
      -o /out/volx-api \
      ./cmd/api

# ─── runtime ─────────────────────────────────────────────────────────
FROM gcr.io/distroless/static-debian12 AS runtime
COPY --from=builder /out/volx-api /app/volx-api
# Fiber listens on API_BIND (default 127.0.0.1:8080). Compose sets
# 0.0.0.0:8080 so the published port works.
EXPOSE 8080
ENTRYPOINT ["/app/volx-api"]
