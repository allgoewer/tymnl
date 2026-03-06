FROM rust:1-slim AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src

RUN cargo build --release --bin tymnl

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/tymnl /usr/local/bin/tymnl

ENTRYPOINT ["tymnl"]
