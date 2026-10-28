FROM docker.io/library/rust:1.96.1-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 AS build
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && dpkg --compare-versions "$(dpkg-query -W -f='${Version}' ca-certificates)" ge 20250419~deb12u2 \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_USE_CURL=1
RUN case "$(uname -m)" in \
        x86_64) rustup_sha=5be191e8c70a5007074254cb74d78dbfdd25cd5a61c27de7b8e5410122ec24f4 ;; \
        aarch64) rustup_sha=a9c168dbea81a84671c6b7a5c1795cb0eb699020110d107ddfca427bd797e697 ;; \
        *) exit 1 ;; \
    esac \
    && curl --proto '=https' --tlsv1.2 -fsSLo /rustup-init "https://static.rust-lang.org/rustup/archive/1.29.2/$(uname -m)-unknown-linux-gnu/rustup-init" \
    && echo "$rustup_sha  /rustup-init" | sha256sum -c - \
    && chmod +x /rustup-init && /rustup-init -y --no-modify-path --default-toolchain none \
    && rm /rustup-init \
    && rustup --version | grep -E '^rustup 1\.29\.2 '

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

FROM docker.io/library/debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && dpkg --compare-versions "$(dpkg-query -W -f='${Version}' ca-certificates)" ge 20250419~deb12u2 \
    && rm -rf /var/lib/apt/lists/*
RUN groupadd --system --gid 10001 sequoia \
    && useradd --system --uid 10001 --gid 10001 --create-home --home-dir /home/sequoia sequoia

WORKDIR /app
COPY --from=build /app/sequoia-ingest /app/sequoia-ingest
RUN mkdir -p /data && chown -R sequoia:sequoia /app /data

ENV RUST_LOG=info
EXPOSE 3010
USER sequoia

CMD ["/app/sequoia-ingest"]
