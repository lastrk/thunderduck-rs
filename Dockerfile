# syntax=docker/dockerfile:1
# Keep the `-bookworm` variant: the runtime stage below is debian:bookworm-slim,
# and a builder on a newer Debian would link against a glibc the runtime does
# not have — that fails at container START, not at build time.
# Must stay >= 1.88 (ar_archive_writer uses let-chains) and in step with
# rust-toolchain.toml, which is authoritative.
#
# `--platform=$BUILDPLATFORM` pins the builder stage to the *host's* native
# arch regardless of which `--platform` buildx is asked to produce. Combined
# with `ARG TARGETARCH` + `--target <triple>` below, this cross-compiles the
# arm64 leg from an amd64 host instead of running an emulated (QEMU) arm64
# container — QEMU-emulated compilation of DuckDB's C++ amalgamation is what
# made multi-platform builds here take ~2h and time out.
FROM --platform=$BUILDPLATFORM rust:1.97.1-bookworm AS builder
ARG TARGETARCH
WORKDIR /app

# Map Docker's TARGETARCH to a Rust target triple and, when cross-compiling
# (building the arm64 leg on this amd64 builder), install the arm64 cross
# linker. `rust_target` is written to a file rather than an ENV so later RUN
# layers can read it without invalidating on an ARG change.
RUN case "$TARGETARCH" in \
      amd64) echo x86_64-unknown-linux-gnu > /rust_target ;; \
      arm64) echo aarch64-unknown-linux-gnu > /rust_target ;; \
      *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac && \
    rustup target add "$(cat /rust_target)" && \
    if [ "$TARGETARCH" = "arm64" ]; then \
      apt-get update && \
      apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu g++-aarch64-linux-gnu && \
      rm -rf /var/lib/apt/lists/* ; \
    fi

ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++

# Deps-only layer: manifests, build scripts, protos and the vendored extension
# binaries change far less often than crate source. Building here first means
# an unchanged Cargo.lock reuses this layer (including the DuckDB C++ compile)
# from BuildKit's registry cache on every subsequent push, even though the
# real source (COPY'd below) changes on every push.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/core/Cargo.toml crates/core/build.rs crates/core/
COPY crates/connect-server/Cargo.toml crates/connect-server/build.rs crates/connect-server/
COPY crates/connect-server/proto crates/connect-server/proto
COPY extensions/vendored extensions/vendored
RUN mkdir -p crates/core/src crates/connect-server/src && \
    echo "" > crates/core/src/lib.rs && \
    echo "fn main() {}" > crates/connect-server/src/main.rs && \
    cargo build --locked --release --features bundled --target "$(cat /rust_target)" -p thunderduck-connect-server && \
    rm -rf crates/core/src crates/connect-server/src

# `--locked` is load-bearing: it makes the build use the committed Cargo.lock
# and FAIL LOUDLY if the lock is missing or stale, instead of silently
# re-resolving the graph and picking up whatever was published since.
COPY . .
RUN cargo build --locked --release --features bundled --target "$(cat /rust_target)" -p thunderduck-connect-server && \
    cp target/"$(cat /rust_target)"/release/thunderduck-connect-server /tmp/thunderduck-connect-server

FROM debian:bookworm-slim
COPY --from=builder /tmp/thunderduck-connect-server /usr/local/bin/thunderduck-connect-server
EXPOSE 15002
ENTRYPOINT ["/usr/local/bin/thunderduck-connect-server"]
