### Stage 1: Build the client (WASM via Trunk)
FROM rust:1.88-bookworm AS client-build

ARG TARGETARCH
ARG BINARYEN_VERSION=126
ARG BINARYEN_ARCH=
ARG TAILWINDCSS_VERSION=4.2.0

RUN apt-get update && apt-get install -y --no-install-recommends brotli gzip ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY .ci/binaryen/ /tmp/binaryen-assets/
COPY .ci/tailwindcss/ /tmp/tailwindcss-assets/
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
    if [ -f /tmp/binaryen-assets/binaryen.tar.gz ] && [ -f /tmp/binaryen-assets/binaryen.tar.gz.sha256 ]; then \
        cp /tmp/binaryen-assets/binaryen.tar.gz /tmp/binaryen.tar.gz; \
        cp /tmp/binaryen-assets/binaryen.tar.gz.sha256 /tmp/binaryen.tar.gz.sha256; \
    else \
        curl --http1.1 --retry 8 --retry-delay 2 --retry-all-errors --continue-at - --connect-timeout 20 --max-time 120 --speed-limit 1024 --speed-time 30 -fsSLo /tmp/binaryen.tar.gz "https://github.com/WebAssembly/binaryen/releases/download/version_${BINARYEN_VERSION}/binaryen-version_${BINARYEN_VERSION}-${RESOLVED_BINARYEN_ARCH}.tar.gz"; \
        curl --http1.1 --retry 8 --retry-delay 2 --retry-all-errors --connect-timeout 20 --max-time 60 -fsSLo /tmp/binaryen.tar.gz.sha256 "https://github.com/WebAssembly/binaryen/releases/download/version_${BINARYEN_VERSION}/binaryen-version_${BINARYEN_VERSION}-${RESOLVED_BINARYEN_ARCH}.tar.gz.sha256"; \
    fi; \
    EXPECTED_SHA="$(awk '{print $1}' /tmp/binaryen.tar.gz.sha256)"; \
    echo "${EXPECTED_SHA}  /tmp/binaryen.tar.gz" | sha256sum -c -; \
    tar -xzf /tmp/binaryen.tar.gz -C /tmp; \
    install -m 0755 "/tmp/binaryen-version_${BINARYEN_VERSION}/bin/wasm-opt" /usr/local/bin/wasm-opt; \
    rm -rf /tmp/binaryen.tar.gz /tmp/binaryen.tar.gz.sha256 "/tmp/binaryen-version_${BINARYEN_VERSION}"; \
    wasm-opt --version
RUN set -eux; \
    TARGET_ARCH="${TARGETARCH:-}"; \
    if [ -z "${TARGET_ARCH}" ]; then \
        TARGET_ARCH="$(dpkg --print-architecture)"; \
    fi; \
    if [ -f /tmp/tailwindcss-assets/tailwindcss ] && [ -f /tmp/tailwindcss-assets/tailwindcss.sha256 ]; then \
        echo "$(cat /tmp/tailwindcss-assets/tailwindcss.sha256)  /tmp/tailwindcss-assets/tailwindcss" | sha256sum -c -; \
        install -m 0755 /tmp/tailwindcss-assets/tailwindcss /usr/local/bin/tailwindcss; \
    else \
        case "${TARGET_ARCH}" in \
            amd64|x86_64) \
                TAILWIND_SUFFIX="linux-x64"; \
                EXPECTED_SHA="8f65e2d21c675f1e8d265219979d17d10634c1f553a2f583265b7edb28726432" ;; \
            arm64|aarch64) \
                TAILWIND_SUFFIX="linux-arm64"; \
                EXPECTED_SHA="376fd4da2c29eb81ae0638cd2f84a4304af92532f2f1576555f41bdb44c185da" ;; \
            *) \
                echo "Unsupported architecture '${TARGET_ARCH}' for Tailwind auto-selection."; \
                exit 1 ;; \
        esac; \
        curl --proto '=https' --tlsv1.2 --http1.1 --retry 8 --retry-delay 2 --retry-all-errors --connect-timeout 20 --max-time 120 --speed-limit 1024 --speed-time 30 -fsSLo /tmp/tailwindcss "https://github.com/tailwindlabs/tailwindcss/releases/download/v${TAILWINDCSS_VERSION}/tailwindcss-${TAILWIND_SUFFIX}"; \
        echo "${EXPECTED_SHA}  /tmp/tailwindcss" | sha256sum -c -; \
        install -m 0755 /tmp/tailwindcss /usr/local/bin/tailwindcss; \
        rm -f /tmp/tailwindcss; \
    fi; \
    tailwindcss --help >/dev/null
RUN rustup target add wasm32-unknown-unknown
RUN --mount=type=cache,id=sequoia-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=sequoia-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo install trunk --locked

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY shared/ shared/
COPY client/ client/
COPY claims-client/ claims-client/
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

WORKDIR /app/claims-client
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
COPY claims-client/Cargo.toml claims-client/Cargo.toml
RUN mkdir -p claims-client/src && echo 'fn main() {}' > claims-client/src/main.rs

RUN --mount=type=cache,id=sequoia-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=sequoia-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=sequoia-server-target,target=/tmp/target-cache,sharing=locked \
    CARGO_TARGET_DIR=/tmp/target-cache cargo build --release --bin sequoia-server && \
    install -Dm755 /tmp/target-cache/release/sequoia-server /app/sequoia-server && \
    strip /app/sequoia-server

### Stage 3: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
RUN groupadd --system --gid 10001 sequoia \
    && useradd --system --uid 10001 --gid 10001 --create-home --home-dir /home/sequoia sequoia

WORKDIR /app

COPY --from=server-build /app/sequoia-server /app/sequoia-server
COPY --from=server-build /app/server/migrations /app/server/migrations
COPY --from=server-build /app/server/static /app/server/static
COPY --from=client-build /app/client/dist /app/client/dist
COPY --from=client-build /app/claims-client/dist /app/claims-client/dist
RUN chown -R sequoia:sequoia /app

ENV RUST_LOG=info
EXPOSE 3000
USER sequoia

CMD ["/app/sequoia-server"]
