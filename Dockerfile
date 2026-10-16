FROM rust:1.99.1-bookworm AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools cmake perl make pkg-config \
    && rm -rf /var/lib/apt/lists/*
# ring and OpenSSL pass -m64, which Debian's musl-gcc rejects. The compiler
# already matches the host, so the wrapper drops the flag.
COPY ci/musl-cc /usr/local/bin/musl-cc
RUN chmod +x /usr/local/bin/musl-cc \
    && arch="$(uname -m)" \
    && case "$arch" in \
         x86_64) target=x86_64-unknown-linux-musl ;; \
         aarch64|arm64) target=aarch64-unknown-linux-musl ;; \
         *) echo "unsupported architecture $arch" >&2; exit 1 ;; \
       esac \
    && rustup target add "$target" \
    && printf '%s\n' "$target" > /musl-target
WORKDIR /src
ENV CC_x86_64_unknown_linux_musl=/usr/local/bin/musl-cc \
    CC_aarch64_unknown_linux_musl=/usr/local/bin/musl-cc
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && printf 'fn main() {}\n' > src/main.rs \
    && printf '\n' > src/lib.rs \
    && target="$(cat /musl-target)" \
    && cargo build --release --locked --target "$target" --features static-libpq \
    && rm -rf src
COPY build.rs ./
COPY src ./src
COPY templates ./templates
COPY migrations ./migrations
COPY static ./static
# A branch build passes the commit hash. A tag build passes the version number.
ARG MARIAN_VERSION
RUN if [ -z "$MARIAN_VERSION" ]; then \
      echo "Pass --build-arg MARIAN_VERSION. A branch uses the commit hash. A tag uses the version number." >&2; \
      exit 1; \
    fi
ENV MARIAN_VERSION=$MARIAN_VERSION
RUN find src templates migrations static build.rs -exec touch {} + \
    && target="$(cat /musl-target)" \
    && cargo build --release --locked --target "$target" --features static-libpq \
    && cp "target/$target/release/marian" /marian \
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
USER 65532:65532
ENV PORT=8080 \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
EXPOSE 8080
ENTRYPOINT ["/marian"]
