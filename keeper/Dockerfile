# syntax=docker/dockerfile:1.7
#
# VolX keeper — off-chain oracle pusher + conditional-order executor.
#
# tsx runs the TypeScript directly (no compile step), so this is a single
# Node stage: install deps, copy source, run `pnpm start`. The build context
# is the repo root (compose wires `context: ..`) only for path symmetry with
# the Rust/Go services — this Dockerfile only needs `keeper/`.
#
# Contract addresses are injected via env (ORACLE_ADDRESS / PERP_ADDRESS) in
# the prod compose, so the image does NOT bundle contracts/deployments.

FROM node:22-alpine
WORKDIR /app

# Pin pnpm to match keeper's CI (10.6.0 honours `onlyBuiltDependencies`).
RUN corepack enable && corepack prepare pnpm@10.6.0 --activate

# Dep layer: lockfile-only copy → install cached until deps change.
COPY keeper/package.json keeper/pnpm-lock.yaml keeper/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile

# Source.
COPY keeper/tsconfig.json ./
COPY keeper/src ./src

# Long-running poll loop; the process supervises itself and exits non-zero
# on fatal config errors so the orchestrator's restart policy can react.
ENTRYPOINT ["pnpm", "start"]
