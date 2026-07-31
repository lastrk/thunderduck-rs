# syntax=docker/dockerfile:1
FROM rust:1.85-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --features bundled -p thunderduck-connect-server

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/thunderduck-connect-server /usr/local/bin/thunderduck-connect-server
EXPOSE 15002
ENTRYPOINT ["/usr/local/bin/thunderduck-connect-server"]
