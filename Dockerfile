# syntax=docker/dockerfile:1
FROM rust:1-slim-bookworm AS chef
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential cmake clang pkg-config \
    && rm -rf /var/lib/apt/lists/* \
    && cargo install cargo-chef --locked
WORKDIR /build

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --bin loan-risk-engine

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home riskengine
WORKDIR /app
COPY --from=builder /build/target/release/loan-risk-engine /app/loan-risk-engine
COPY rules /app/rules
ENV RULES_DIR=/app/rules PORT=8000
USER riskengine
EXPOSE 8000
ENTRYPOINT ["/app/loan-risk-engine"]
