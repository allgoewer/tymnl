FROM rust:1-slim AS builder

RUN apt-get update && apt-get install -y musl-tools ca-certificates && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src

RUN cargo build --locked --release 

FROM debian:trixie-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends gosu ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && printf '#!/bin/sh\nset -e\ngroupadd -g "${PGID:-1000}" app 2>/dev/null || true\nuseradd -u "${PUID:-1000}" -g "${PGID:-1000}" -M app 2>/dev/null || true\nexec gosu app "$@"\n' > /entrypoint.sh \
    && chmod +x /entrypoint.sh

WORKDIR /app

COPY --from=builder /app/target/release/tymnl /app/tymnl

ENV PUID=1000
ENV PGID=1000

ENTRYPOINT ["/entrypoint.sh"]
CMD ["/app/tymnl", "-c", "/config/tymnl.yml", "serve"]
