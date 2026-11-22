# Build a static musl binary, then ship it alone in an empty image.
FROM rust:1.100.0-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src

# Cache dependencies separately from the sources.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && touch src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY migrations ./migrations
COPY src ./src
RUN touch src/main.rs src/lib.rs && cargo build --release --locked \
    && strip target/release/hermes-sidecar \
    && mkdir /data

FROM scratch
LABEL org.opencontainers.image.licenses="GPL-3.0-or-later"
LABEL org.opencontainers.image.description="Lightweight Rust sidecar API next to a self-hosted Hermes Agent instance"
COPY --from=build /src/target/release/hermes-sidecar /hermes-sidecar
# Mount point of the database volume. A new named volume takes this owner, so the sidecar can
# write there. It runs as the Hermes user, which it needs anyway to read Hermes's images.
COPY --from=build --chown=10000:10000 /data /data
USER 10000:10000
EXPOSE 8080
ENTRYPOINT ["/hermes-sidecar"]
