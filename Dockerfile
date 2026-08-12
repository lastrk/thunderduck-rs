# syntax=docker/dockerfile:1
# No compilation happens in this Dockerfile. `cargo build` runs as a plain
# shell step in .nu/workflows/publish.yaml (cross-compiling both
# x86_64-unknown-linux-gnu and aarch64-unknown-linux-gnu natively, without
# QEMU, on that step's cache-warmed rust:1.97.1-bookworm container) and drops
# the two binaries into dist/. This stage just packages the one matching
# TARGETARCH — cheap enough per platform that no BuildKit layer caching is
# needed here.
#
# Keep the `-bookworm` variant in step with that build step's image, so the
# binary copied in below links against a glibc this runtime actually has.
FROM debian:bookworm-slim
ARG TARGETARCH
COPY dist/thunderduck-connect-server-linux-$TARGETARCH /usr/local/bin/thunderduck-connect-server
EXPOSE 15002
ENTRYPOINT ["/usr/local/bin/thunderduck-connect-server"]
