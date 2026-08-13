# Local build acceleration (`scripts/dev/`)

Faster change-compile-eval loops **inside the devcontainer only**. Nothing here
touches CI: the config lives in `$CARGO_HOME/config.toml` + `env.sh` (per
container), and artifacts live under `<main-repo>/.build-cache/` (gitignored,
on the persistent mount).

## What it does

1. **Faster linker (mold, fallback LLVM lld)** — selected via PATH (a shim dir
   whose `ld` is mold), so gcc picks it up with **no rustflags**. Because it's
   not a rustflag, it does **not** enter cargo's fingerprint and never triggers
   rebuilds. Cuts the link step that dominates every edit and lowers peak link
   RAM (the container is capped at 8 GiB).
2. **Shared compiler cache (sccache)** — `SCCACHE_DIR=<repo>/.build-cache/sccache`,
   shared across all worktrees and container restarts. Unchanged third-party
   crates compile once and are reused everywhere. Each worktree keeps its **own
   `target/`**, so concurrent builds never block on a shared cargo lock.
   Workspace crates keep incremental compilation (sccache passes them through).
3. **Official DuckDB download** — the `duckdb` dependency downloads the exact
   pinned DuckDB release for clean builds. This avoids a local C++ build and
   keeps the host ABI aligned with the bundled extension.

## Usage

```bash
# Once per container start (downloads tools and configures the build cache,
# writes $CARGO_HOME/config.toml + env.sh):
scripts/dev/dev-cache-setup.sh

# Activate the linker in the current shell (added to ~/.bashrc for new shells):
source <main-repo>/.build-cache/env.sh

# Then build/test normally:
cargo build
cargo test
```

That's it — no per-build flags. `cargo`, `cargo test`, and rust-analyzer all
download the pinned DuckDB library when needed.

## Other helpers

- `scripts/dev/dev-clean.sh` — `cargo clean` scoped to first-party crates only
  (deps stay built). Less essential now that the heavy DuckDB lib lives in
  `.build-cache/`, but handy.
- Inspect cache hits: `<repo>/.build-cache/bin/sccache --show-stats`.

## Cross-repo Delta dev loop

- `scripts/dev/delta-dev-setup.sh` — one-time (idempotent): clone our forks of
  `duckdb-delta` + `delta-kernel-rs` into gitignored `.duckdb-delta/` /
  `.delta-kernel-rs/`, pin the `duckdb` submodule to the ABI-matching tag, patch
  the extension's CMakeLists for a local-kernel override, and bootstrap the
  toolchain (below).
- `scripts/dev/delta-toolchain-setup.sh` — bootstrap a relocatable conda-forge
  gcc-13 under gitignored `.delta-toolchain/` via micromamba (no root; the
  devcontainer's gcc 11 can't compile the extension). Invoked by the setup
  script; safe to run standalone.
- `scripts/dev/delta-build.sh [debug|release]` — build the extension against the
  **local** kernel; prints the `export THUNDERDUCK_DELTA_EXT_PATH=...` line that
  the server's dev-load hook consumes.

Full design, pinning matrix, and gotchas:
[`docs/context/delta-cross-repo-dev-loop.md`](../../docs/context/delta-cross-repo-dev-loop.md).

## Disabling

- Delete the `# >>> thunderduck dev-cache (managed) >>>` block in
  `$CARGO_HOME/config.toml` and remove the `source …/env.sh` line from
  `~/.bashrc`.
- Nuke everything: `rm -rf <main-repo>/.build-cache`.

## Why local-only / not CI

CI uses its own `$CARGO_HOME`, has no `.build-cache/`, and downloads the
version-matched official DuckDB library. The repo's tracked `.cargo/config.toml`
sets the required download mode.
