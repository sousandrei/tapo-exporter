ARG TARGET=x86_64-unknown-linux-musl

FROM rust:alpine AS builder

ARG TARGET

# Add the musl toolchain and an inspection tool for the final binary.
RUN apk add --no-cache file gcc musl-dev \
    && rustup target add ${TARGET}

ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-alpine-linux-musl-gcc
ENV RUSTFLAGS="-C target-feature=+crt-static"

WORKDIR /app

# Cache dependencies
RUN cargo init --name tapo-exporter && rm Cargo.toml

COPY Cargo.toml .
COPY Cargo.lock .

RUN cargo build --release --target ${TARGET}

# Copy source code and build
RUN rm -rf src target/${TARGET}/release/tapo-exporter target/${TARGET}/release/deps/tapo_exporter*
COPY src src

RUN cargo build --release --target ${TARGET}
RUN file target/${TARGET}/release/tapo-exporter \
    | grep -Eq "statically linked|static-pie linked"

FROM scratch

ARG TARGET

WORKDIR /app

COPY --from=builder /app/target/${TARGET}/release/tapo-exporter .
CMD ["./tapo-exporter"]
