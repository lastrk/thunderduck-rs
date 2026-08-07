# syntax=docker/dockerfile:1
# Keep the `-bookworm` variant: the runtime stage below is debian:bookworm-slim,
# and a builder on a newer Debian would link against a glibc the runtime does
# not have — that fails at container START, not at build time.
# Must stay >= 1.88 (ar_archive_writer uses let-chains) and in step with
# rust-toolchain.toml, which is authoritative.
FROM rust:1.97.1-bookworm AS builder
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
