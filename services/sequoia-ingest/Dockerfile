FROM public.ecr.aws/docker/library/rust:1.88-bookworm AS build

WORKDIR /app
COPY Cargo.toml ./
COPY services/sequoia-ingest/Cargo.toml services/sequoia-ingest/Cargo.lock services/sequoia-ingest/
COPY shared/Cargo.toml shared/Cargo.toml
RUN mkdir -p shared/src services/sequoia-ingest/src \
    && touch shared/src/lib.rs \
    && echo 'fn main() {}' > services/sequoia-ingest/src/main.rs

WORKDIR /app/services/sequoia-ingest
RUN cargo build --release --locked --bin sequoia-ingest \
    && cargo clean --release --locked -p sequoia-ingest -p sequoia-shared
COPY shared/ /app/shared/
COPY services/sequoia-ingest/src/ src/
RUN cargo build --release --locked --bin sequoia-ingest && \
    install -Dm755 target/release/sequoia-ingest /app/sequoia-ingest && \
    strip /app/sequoia-ingest

FROM public.ecr.aws/docker/library/debian:bookworm-slim

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
