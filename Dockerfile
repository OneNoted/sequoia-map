### Stage 1: Build the client (WASM via Trunk)
FROM rust:1.88-bookworm AS client-build

ARG TARGETARCH
ARG BINARYEN_VERSION=126
ARG BINARYEN_ARCH=

RUN apt-get update && apt-get install -y --no-install-recommends brotli gzip ca-certificates curl && rm -rf /var/lib/apt/lists/*
RUN set -eux; \
    TARGET_ARCH="${TARGETARCH:-}"; \
    if [ -z "${TARGET_ARCH}" ]; then \
        TARGET_ARCH="$(dpkg --print-architecture)"; \
    fi; \
    RESOLVED_BINARYEN_ARCH="${BINARYEN_ARCH:-}"; \
    if [ -z "${RESOLVED_BINARYEN_ARCH}" ]; then \
        case "${TARGET_ARCH}" in \
            amd64|x86_64) RESOLVED_BINARYEN_ARCH="x86_64-linux" ;; \
            arm64|aarch64) RESOLVED_BINARYEN_ARCH="aarch64-linux" ;; \
            *) \
                echo "Unsupported architecture '${TARGET_ARCH}' for Binaryen auto-selection."; \
                echo "Set --build-arg BINARYEN_ARCH=<binaryen-archive-suffix> to override."; \
                exit 1 ;; \
        esac; \
    fi; \
    echo "Using Binaryen archive architecture: ${RESOLVED_BINARYEN_ARCH} (target=${TARGET_ARCH})"; \
    curl -fsSLo /tmp/binaryen.tar.gz "https://github.com/WebAssembly/binaryen/releases/download/version_${BINARYEN_VERSION}/binaryen-version_${BINARYEN_VERSION}-${RESOLVED_BINARYEN_ARCH}.tar.gz"; \
    curl -fsSLo /tmp/binaryen.tar.gz.sha256 "https://github.com/WebAssembly/binaryen/releases/download/version_${BINARYEN_VERSION}/binaryen-version_${BINARYEN_VERSION}-${RESOLVED_BINARYEN_ARCH}.tar.gz.sha256"; \
    EXPECTED_SHA="$(awk '{print $1}' /tmp/binaryen.tar.gz.sha256)"; \
    echo "${EXPECTED_SHA}  /tmp/binaryen.tar.gz" | sha256sum -c -; \
    tar -xzf /tmp/binaryen.tar.gz -C /tmp; \
    install -m 0755 "/tmp/binaryen-version_${BINARYEN_VERSION}/bin/wasm-opt" /usr/local/bin/wasm-opt; \
    rm -rf /tmp/binaryen.tar.gz /tmp/binaryen.tar.gz.sha256 "/tmp/binaryen-version_${BINARYEN_VERSION}"; \
    wasm-opt --version
RUN rustup target add wasm32-unknown-unknown
RUN --mount=type=cache,id=sequoia-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=sequoia-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo install trunk --locked

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY shared/ shared/
COPY client/ client/
# Need a stub server crate so workspace resolves
COPY server/Cargo.toml server/Cargo.toml
RUN mkdir -p server/src && echo 'fn main() {}' > server/src/main.rs

WORKDIR /app/client
RUN --mount=type=cache,id=sequoia-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=sequoia-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=sequoia-client-target,target=/app/target,sharing=locked \
    --mount=type=cache,id=sequoia-trunk-cache,target=/app/.trunk,sharing=locked \
    trunk build --release
RUN find dist -type f -name '*_bg.wasm' -exec wasm-opt -Oz {} -o {} \;
RUN find dist -type f \( -name '*.wasm' -o -name '*.js' -o -name '*.css' -o -name '*.html' -o -name '*.json' -o -name '*.svg' \) -exec sh -c 'brotli -f -q 11 "$1" -o "$1.br"; gzip -f -k -9 "$1"' _ {} \;

### Stage 2: Build the server
FROM rust:1.88-bookworm AS server-build

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY shared/ shared/
COPY server/ server/
# Need a stub client crate so workspace resolves
COPY client/Cargo.toml client/Cargo.toml
RUN mkdir -p client/src && echo 'fn main() {}' > client/src/main.rs

RUN --mount=type=cache,id=sequoia-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=sequoia-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=sequoia-server-target,target=/tmp/target-cache,sharing=locked \
    CARGO_TARGET_DIR=/tmp/target-cache cargo build --release --bin sequoia-server && \
    install -Dm755 /tmp/target-cache/release/sequoia-server /app/sequoia-server && \
    strip /app/sequoia-server

### Stage 3: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=server-build /app/sequoia-server /app/sequoia-server
COPY --from=server-build /app/server/migrations /app/server/migrations
COPY --from=client-build /app/client/dist /app/client/dist

ENV RUST_LOG=info
EXPOSE 3000

CMD ["/app/sequoia-server"]
