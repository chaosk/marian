FROM rust:1.99.1-bookworm AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools cmake perl make pkg-config \
    && rm -rf /var/lib/apt/lists/*
# ring and OpenSSL pass -m64, which Debian's musl-gcc rejects. The compiler
# already matches the host, so the wrapper drops the flag.
COPY ci/musl-cc /usr/local/bin/musl-cc
RUN chmod +x /usr/local/bin/musl-cc \
    && rustup target add wasm32-unknown-unknown x86_64-unknown-linux-musl \
    && cargo install cargo-leptos --locked --debug
WORKDIR /src
ENV CC_x86_64_unknown_linux_musl=/usr/local/bin/musl-cc
COPY . .
# A branch build passes the commit hash. A tag build passes the version number.
ARG MARIAN_VERSION
RUN if [ -z "$MARIAN_VERSION" ]; then \
      echo "Pass --build-arg MARIAN_VERSION. A branch uses the commit hash. A tag uses the version number." >&2; \
      exit 1; \
    fi
ENV MARIAN_VERSION=$MARIAN_VERSION
# --precompress leaves .br and .gz copies of the wasm, script, and stylesheet
# beside them; the server sends those instead of compressing on each request.
RUN cargo leptos build --release --precompress --bin-target-triple x86_64-unknown-linux-musl --bin-features static-libpq \
    && cp target/x86_64-unknown-linux-musl/release/marian /marian \
    && file /marian | grep -Eq "statically linked|static-pie linked"

FROM debian:bookworm-slim AS certs
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

FROM scratch
ARG MARIAN_VERSION
LABEL org.opencontainers.image.version="${MARIAN_VERSION}"
COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /marian /marian
COPY --from=build /src/target/site /site
USER 65532:65532
ENV PORT=8080 \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt \
    LEPTOS_SITE_ROOT=/site \
    LEPTOS_SITE_PKG_DIR=pkg \
    LEPTOS_OUTPUT_NAME=marian
EXPOSE 8080
ENTRYPOINT ["/marian"]
