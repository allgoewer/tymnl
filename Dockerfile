FROM rust:1-slim AS builder

RUN apt-get update && apt-get install -y musl-tools ca-certificates && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src

RUN rustup target add x86_64-unknown-linux-musl && \
    cargo build --release --target x86_64-unknown-linux-musl --bin tymnl

FROM scratch

COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/tymnl /usr/local/bin/tymnl

ENTRYPOINT ["/usr/local/bin/tymnl"]
