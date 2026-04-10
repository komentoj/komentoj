FROM rust:slim-bookworm AS builder

WORKDIR /app

# Cache dependencies separately from source
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs \
    && cargo build --release \
    && rm -rf src

COPY src ./src
COPY migrations ./migrations
# Touch main.rs so cargo knows it changed
RUN touch src/main.rs && cargo build --release

# ── Runtime ────────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/komentoj /usr/local/bin/komentoj

EXPOSE 8080
ENV KOMENTOJ_CONFIG=/config/config.toml

CMD ["komentoj"]
