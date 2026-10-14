FROM rust:1.99-bookworm AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools file \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-unknown-linux-musl
# ring passes -m64, which Debian's musl-gcc rejects. The wrapper drops that flag.
COPY <<'EOF' /usr/local/bin/musl-cc
#!/bin/bash
args=()
for arg in "$@"; do
  if [ "$arg" != "-m64" ]; then
    args+=("$arg")
  fi
done
exec musl-gcc "${args[@]}"
EOF
RUN chmod +x /usr/local/bin/musl-cc
WORKDIR /src
ENV CC_x86_64_unknown_linux_musl=/usr/local/bin/musl-cc
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && printf 'fn main() {}\n' > src/main.rs \
    && printf '\n' > src/lib.rs \
    && cargo build --release --locked --target x86_64-unknown-linux-musl \
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
    && cargo build --release --locked --target x86_64-unknown-linux-musl \
    && cp target/x86_64-unknown-linux-musl/release/marian /marian \
    && file /marian | grep -Eq "statically linked|static-pie linked"

FROM scratch
ARG MARIAN_VERSION
LABEL org.opencontainers.image.version="${MARIAN_VERSION}"
COPY --from=build /marian /marian
USER 65532:65532
ENV PORT=8080 \
    MARIAN_DB=/data/marian.db
EXPOSE 8080
ENTRYPOINT ["/marian"]
