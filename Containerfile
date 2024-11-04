FROM rust:alpine AS builder

# Add static linking dependencies
RUN apk add --no-cache musl-dev

WORKDIR /app

# Cache dependencies
RUN cargo init --name tapo-exporter && rm Cargo.toml

COPY Cargo.toml .
COPY Cargo.lock .

RUN cargo build --release

# Copy source code and build
RUN rm -rf src target/release/tapo-exporter target/release/deps/tapo_exporter*
COPY src src

RUN cargo build --release

FROM scratch

WORKDIR /app

COPY --from=builder /app/target/release/tapo-exporter .
CMD ["./tapo-exporter"]