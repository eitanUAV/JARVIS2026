# syntax=docker/dockerfile:1

# ---------- Build stage ----------
FROM rust:1.84-slim AS builder

WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Dependencies are built from the manifests alone and cached as their own
# layer, so editing src/ no longer rebuilds the entire dependency tree.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release \
    && rm -rf src

COPY src ./src
# Cargo skips work when mtimes look unchanged; touch to force the real build.
RUN touch src/main.rs && cargo build --release

# ---------- Runtime stage ----------
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl3 \
        curl \
    && rm -rf /var/lib/apt/lists/*

# Run unprivileged: a container that writes uploads should not do so as root.
RUN useradd --system --create-home --uid 10001 jarvis

WORKDIR /app

COPY --from=builder /app/target/release/jarvis-property-upload /app/jarvis-property-upload
COPY static /app/static
COPY properties.json /app/properties.json

# NOTE: this path must be backed by a persistent volume in production.
# A container filesystem is discarded on every redeploy, taking uploads with it.
RUN mkdir -p /app/uploads && chown -R jarvis:jarvis /app

USER jarvis

ENV RUST_LOG=info \
    SERVER_HOST=0.0.0.0 \
    SERVER_PORT=8080 \
    UPLOAD_DIR=/app/uploads

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${PORT:-${SERVER_PORT:-8080}}/api/health" || exit 1

CMD ["/app/jarvis-property-upload"]
