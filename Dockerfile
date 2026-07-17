# Build Rust API (web/index.html is include_str!'d at compile time)
FROM rust:1.85-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY web ./web
RUN cargo build --release -p acend-api

# Runtime: Node (marginfi compose helper) + API binary
FROM node:20-bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/acend-api /usr/local/bin/acend-api
COPY config ./config
COPY scripts ./scripts
COPY web ./web

WORKDIR /app/scripts
RUN npm ci --omit=dev

WORKDIR /app

# Mainnet defaults — override in Railway Variables if needed
ENV ACEND_PAIRS_CONFIG=config/pairs.mainnet.toml
ENV ACEND_BIDS_CONFIG=config/standing-bids.deploy.json
ENV ACEND_RPC_URL=https://api.mainnet-beta.solana.com
ENV RUST_LOG=info

EXPOSE 8080
CMD ["acend-api"]
