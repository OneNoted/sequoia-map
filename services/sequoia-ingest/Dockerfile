FROM rust:1.88-bookworm AS build

WORKDIR /app
COPY Cargo.toml ./
COPY services/sequoia-ingest/Cargo.toml services/sequoia-ingest/Cargo.lock services/sequoia-ingest/
COPY shared/ shared/
COPY services/sequoia-ingest/src/ services/sequoia-ingest/src/

WORKDIR /app/services/sequoia-ingest
RUN --mount=type=cache,id=sequoia-ingest-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=sequoia-ingest-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=sequoia-ingest-target,target=/tmp/target-cache,sharing=locked \
    CARGO_TARGET_DIR=/tmp/target-cache cargo build --release --bin sequoia-ingest && \
    install -Dm755 /tmp/target-cache/release/sequoia-ingest /app/sequoia-ingest && \
    strip /app/sequoia-ingest

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
RUN groupadd --system --gid 10001 sequoia \
    && useradd --system --uid 10001 --gid 10001 --create-home --home-dir /home/sequoia sequoia

WORKDIR /app
COPY --from=build /app/sequoia-ingest /app/sequoia-ingest
RUN mkdir -p /data && chown -R sequoia:sequoia /app /data

ENV RUST_LOG=info
EXPOSE 3010
USER sequoia

CMD ["/app/sequoia-ingest"]
