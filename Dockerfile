# Build stage - Pinning deps to avoid nightly
FROM rust:1.84-slim as builder

# Force rebuild with audio deps - 2026-01-17
WORKDIR /app

# Install dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    libasound2-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy all source files
COPY . .

# Build for release
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    libasound2 \
    && rm -rf /var/lib/apt/lists/*

# Copy the binary from builder
COPY --from=builder /app/target/release/jarvis-property-upload /app/jarvis-property-upload

# Copy static files
COPY --from=builder /app/static /app/static

# Create uploads directory
RUN mkdir -p /app/uploads

# Expose port
EXPOSE 8080

# Set environment variables
ENV RUST_LOG=info
ENV SERVER_HOST=0.0.0.0
ENV SERVER_PORT=8080

# Run the binary
CMD ["/app/jarvis-property-upload"]
