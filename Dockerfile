# syntax=docker/dockerfile:1

# ---- Stage 1: build a fully static binary (musl) ----
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev pkgconfig
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY web ./web
ENV CARGO_NET_RETRY=10 \
    CARGO_TERM_COLOR=never
RUN cargo build --release --target x86_64-unknown-linux-musl

# ---- Stage 2: collect CA certificates for upstream HTTPS ----
FROM alpine:3.20 AS ca
RUN apk add --no-cache ca-certificates

# ---- Stage 3: minimal runtime ----
FROM scratch
COPY --from=ca /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /build/target/x86_64-unknown-linux-musl/release/cliproxyapi-quota-dashboard /app/server
EXPOSE 8080
ENTRYPOINT ["/app/server"]
