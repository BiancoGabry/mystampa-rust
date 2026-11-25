# ---- build ----
FROM rust:1-slim-trixie AS builder
WORKDIR /src

# Dependencies first, cached until Cargo.toml/Cargo.lock change
COPY Cargo.toml Cargo.lock build.rs ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release --locked --no-default-features \
    && rm -rf src target/release/deps/mystampa-* target/release/.fingerprint/mystampa-* target/release/mystampa*

COPY src ./src
COPY resources ./resources
RUN touch src/main.rs && cargo build --release --locked --no-default-features

# ---- runtime ----
FROM debian:trixie-slim AS runner
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 1001 --home-dir /app mystampa \
    && mkdir -p /app/logs && chown -R mystampa /app

WORKDIR /app
COPY --from=builder /src/target/release/mystampa /app/mystampa

# Local time for log timestamps and the daily log file
ENV TZ=Europe/Rome
ENV LOG_DIR=/app/logs

USER mystampa
# API_URL and API_KEY come from .env (see docker-compose.yml)
CMD ["/app/mystampa"]
