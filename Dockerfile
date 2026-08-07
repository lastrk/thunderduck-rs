# syntax=docker/dockerfile:1
FROM rust:1.85-bookworm AS builder
WORKDIR /app
COPY . .
# `--locked` is load-bearing: it makes the build use the committed Cargo.lock
# and FAIL LOUDLY if the lock is missing or stale, instead of silently
# re-resolving the graph and picking up whatever was published since.
RUN cargo build --locked --release --features bundled -p thunderduck-connect-server

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/thunderduck-connect-server /usr/local/bin/thunderduck-connect-server
EXPOSE 15002
ENTRYPOINT ["/usr/local/bin/thunderduck-connect-server"]
