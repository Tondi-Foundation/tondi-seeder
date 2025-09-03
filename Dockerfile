# Multi-stage build Dockerfile
FROM rust:1.75-slim as builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Set working directory
WORKDIR /usr/src/tondi_seeder

# Copy Cargo files
COPY Cargo.toml Cargo.lock ./

# Create virtual target to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release
RUN rm -rf src

# Copy source code
COPY src ./src

# Build application
RUN cargo build --release

# Runtime image
FROM debian:bullseye-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl1.1 \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN groupadd -r tondi_seeder && useradd -r -g tondi_seeder tondi_seeder

# Create necessary directories
RUN mkdir -p /app/data /app/logs && \
    chown -R tondi_seeder:tondi_seeder /app

# Copy binary file
COPY --from=builder /usr/src/tondi_seeder/target/release/tondi_seeder /usr/local/bin/

# Set permissions
RUN chmod +x /usr/local/bin/tondi_seeder

# Switch to non-root user
USER tondi_seeder

# Set working directory
WORKDIR /app

# Expose ports - using the new default ports from config
EXPOSE 5354 3737 8080

# Health check - using the correct gRPC port
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:3737/health || exit 1

# Default command
ENTRYPOINT ["tondi_seeder"]

# Default arguments - using the new default ports
CMD ["--help"]
