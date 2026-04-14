FROM rust:slim-bookworm AS builder

WORKDIR /app

# Copy the whole workspace. The single-crate dep-caching trick doesn't transfer
# cleanly to a lib + bin workspace, so rely on BuildKit's layer cache
# (`cache-from`/`cache-to` in CI) instead.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release --bin komentoj

# ── Runtime ────────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/komentoj /usr/local/bin/komentoj

EXPOSE 8080
ENV KOMENTOJ_CONFIG=/config/config.toml

CMD ["komentoj"]
