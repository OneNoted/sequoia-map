FROM docker.io/library/rust:1.96.1-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 AS rust-base
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

FROM rust-base AS workspace-deps
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY shared/Cargo.toml shared/Cargo.toml
COPY wasm/Cargo.toml wasm/Cargo.toml
COPY browser-map/Cargo.toml browser-map/Cargo.toml
COPY client/Cargo.toml client/Cargo.toml
COPY claims-client/Cargo.toml claims-client/Cargo.toml
COPY server/Cargo.toml server/Cargo.toml
# Keep dependency artifacts in ordinary layers: registry exports do not retain cache mounts.
RUN mkdir -p shared/src wasm/src browser-map/src client/src claims-client/src server/src \
    && touch shared/src/lib.rs wasm/src/lib.rs browser-map/src/lib.rs \
    && echo 'fn main() {}' > client/src/main.rs \
    && echo 'fn main() {}' > claims-client/src/main.rs \
    && echo 'fn main() {}' > server/src/main.rs

### Client tooling is independent of the application dependency manifests.
FROM rust-base AS client-tools

ARG TARGETARCH
ARG BINARYEN_VERSION=126
ARG BINARYEN_ARCH=
ARG TAILWINDCSS_VERSION=4.2.0

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
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
# The repository tool builds both clients: Cargo, wasm-bindgen (a library pinned to the
# application's version), wasm-opt, Tailwind, the pages, compressed copies and the map's size
# budget. Its own workspace and lockfile keep its dependencies out of the application's.
WORKDIR /app
COPY .cargo/config.toml .cargo/config.toml
COPY tools/xtask/ tools/xtask/
RUN cargo build --locked --manifest-path tools/xtask/Cargo.toml

FROM client-tools AS client-build

ARG CARGO_BUILD_JOBS=2
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
COPY --from=workspace-deps /app/ /app/
RUN cargo build --release --locked --target wasm32-unknown-unknown -p sequoia-client -p sequoia-claims-client \
    && cargo clean --release --locked --target wasm32-unknown-unknown \
       -p sequoia-shared -p sequoia-map-engine -p sequoia-browser-map \
       -p sequoia-client -p sequoia-claims-client
COPY shared/ shared/
COPY wasm/ wasm/
COPY browser-map/ browser-map/
COPY client/ client/
COPY claims-client/ claims-client/
RUN cargo xtask web --release

### Build the server
FROM workspace-deps AS server-build

ARG CARGO_BUILD_JOBS=2
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
WORKDIR /app
RUN cargo build --release --locked --bin sequoia-server \
    && cargo clean --release --locked -p sequoia-server -p sequoia-shared
COPY shared/ shared/
COPY wasm/ wasm/
COPY server/ server/
# The backend key names the server's source closure. A static client release records the key
# it was built against and is only activated on a server with the same key;
# `cargo xtask release key` computes the same value from a commit.
RUN find Cargo.toml Cargo.lock shared server -type f -print0 | LC_ALL=C sort -z \
    | xargs -0 sha256sum | sha256sum | cut -c1-16 > /app/backend-key

RUN cargo build --release --locked --bin sequoia-server && \
    install -Dm755 /app/target/release/sequoia-server /app/sequoia-server && \
    strip /app/sequoia-server

### Stage 3: Runtime
FROM docker.io/library/debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8

ARG SOURCE_COMMIT=

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && dpkg --compare-versions "$(dpkg-query -W -f='${Version}' ca-certificates)" ge 20250419~deb12u2 \
    && rm -rf /var/lib/apt/lists/*
RUN groupadd --system --gid 10001 sequoia \
    && useradd --system --uid 10001 --gid 10001 --create-home --home-dir /home/sequoia sequoia

WORKDIR /app

COPY --from=server-build /app/sequoia-server /app/sequoia-server
COPY --from=server-build /app/backend-key /app/backend-key
COPY --from=server-build /app/server/migrations /app/server/migrations
COPY --from=server-build /app/server/static /app/server/static
COPY --from=client-build /app/client/dist /app/client/dist
COPY --from=client-build /app/claims-client/dist /app/claims-client/dist
# A named volume mounted here starts with this directory's ownership.
RUN mkdir -p /srv/client-releases && chown -R sequoia:sequoia /app /srv/client-releases
# The commit the embedded clients were built from, whatever SOURCE_COMMIT a platform sets.
RUN printf '%s\n' "${SOURCE_COMMIT}" > /app/source-commit

ENV RUST_LOG=info
ENV SOURCE_COMMIT=${SOURCE_COMMIT}
EXPOSE 3000
USER sequoia

CMD ["/app/sequoia-server"]
