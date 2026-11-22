# Build a static musl binary, then ship it alone in an empty image.
FROM rust:1.100.0-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src

# Cache dependencies separately from the sources.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && touch src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY src ./src
RUN touch src/main.rs src/lib.rs && cargo build --release --locked \
    && strip target/release/hermes-sidecar

FROM scratch
LABEL org.opencontainers.image.licenses="GPL-3.0-or-later"
LABEL org.opencontainers.image.description="Lightweight Rust sidecar API next to a self-hosted Hermes Agent instance"
COPY --from=build /src/target/release/hermes-sidecar /hermes-sidecar
USER 65534:65534
EXPOSE 8080
ENTRYPOINT ["/hermes-sidecar"]
