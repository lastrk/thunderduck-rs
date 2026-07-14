# Cross-repo Delta dev loop

> **Scope:** developing Delta Lake read/write *beyond* what upstream ships, by
> editing `delta-kernel-rs` + `duckdb-delta` + thunderduck together and rebuilding
> fast. Devcontainer-only; nothing here touches CI or the production build.

## Why

The `duckdb-delta` extension gives DuckDB `delta_scan` (read) and blind-insert
(write) via the `delta-kernel-rs` FFI. Both are behind what we need: the extension
pins kernel **v0.21.0** (upstream is already past it) and offers no
MERGE/UPDATE/DELETE/overwrite. Advancing means changing the kernel (protocol/FFI),
rebuilding the extension against *that custom kernel*, and loading it into
thunderduck — repeatedly. This loop makes that cycle: **edit → rebuild → restart →
test**, without recompiling thunderduck.

## Topology

Two gitignored checkouts of *our forks* plus a gitignored userspace toolchain
live at the repo root (see `.gitignore`; materialized by
`scripts/dev/delta-dev-setup.sh`):

```
thunderduck-rs (this repo)
├── .delta-kernel-rs/   fork lastrk/delta-kernel-rs   (branch thunderduck-delta-dev off tag v0.21.0)
├── .duckdb-delta/      fork lastrk/duckdb-delta       (branch thunderduck-delta-dev off v1.5-variegata)
│   └── duckdb/         submodule pinned to tag v1.5.4  ← ABI anchor
└── .delta-toolchain/   conda-forge gcc-13 (micromamba) — see "Toolchain" below
```

## The pinning matrix (why these exact refs)

| Repo | Ref | Reason |
|------|-----|--------|
| thunderduck `duckdb` crate | `1.10504.0` | == DuckDB **v1.5.4** (`extensions/vendored/MANIFEST.toml` `[source].duckdb_version`) |
| `.duckdb-delta` branch | `v1.5-variegata` | DuckDB-1.5.x-aligned line ("variegata" = 1.5.x codename) |
| `.duckdb-delta/duckdb` submodule | tag `v1.5.4` | **must equal** thunderduck's linked libduckdb, or the extension won't `LOAD` |
| `.delta-kernel-rs` branch | off tag `v0.21.0` | the FFI version the extension's C++ currently compiles against — green baseline |

**ABI rule:** a DuckDB C++ extension only loads into a DuckDB of the *same
version + platform*. thunderduck links libduckdb v1.5.4, so the extension is built
against v1.5.4 (submodule tag). `session.rs` sets `allow_unsigned_extensions=true`,
so the locally-built (unsigned) extension loads.

## How the custom kernel replaces the stable dependency

`.duckdb-delta/CMakeLists.txt` normally does `ExternalProject_Add(delta_kernel
GIT_TAG v0.21.0 ...)` then `cargo build --package delta_kernel_ffi`. Our setup
script patches it (marker: `DELTA_KERNEL_LOCAL_DIR`) so that when
`-DDELTA_KERNEL_LOCAL_DIR=<abs>` is passed it builds that local checkout in place
(`SOURCE_DIR` + no-op `DOWNLOAD_COMMAND` + `BUILD_ALWAYS`) instead of cloning the
tag. Unset ⇒ upstream behaviour (clone v0.21.0). The patch is committed to our
fork branch, so it is durable and pushable.

`delta-build.sh` passes that flag via extension-ci-tools' `EXT_FLAGS` hook (not
`TOOLCHAIN_FLAGS`, which would clobber the makefile's vcpkg appends).

## Toolchain (why a bundled gcc)

The devcontainer ships **gcc 11**, whose libstdc++ rejects a self-referential
`unordered_map<string, StatNode>` in duckdb-delta's write-path source; the
extension's CI uses gcc 13/14, where newer libstdc++ accepts it. `apt` is
unavailable here (no root). So `delta-toolchain-setup.sh` installs a relocatable
**conda-forge gcc 13** under `.delta-toolchain/` via micromamba (no root), and
`delta-build.sh` builds with it. The extension links **`-static-libstdc++
-static-libgcc`**, so it embeds the newer libstdc++ and carries *no* dynamic
libstdc++/libgcc_s dependency — it loads cleanly into thunderduck's gcc-11
process (verified: `objdump -T` shows no external `GLIBCXX_*` needs). DuckDB's
own C++ ABI is keyed to the v1.5.4 version, not the compiler, so mixing gcc 11
(host) and gcc 13 (extension) across the load boundary is safe.

## How thunderduck loads it

`crates/core/src/runtime/extension_loader.rs` `load()` loads the mandatory
`thdck_spark_funcs`, then — if env **`THUNDERDUCK_DELTA_EXT_PATH`** is set — `LOAD`s
that path too. Unset ⇒ no-op (production unaffected). Set-but-unloadable ⇒ hard
error. Because the extension is read from disk at session start (not embedded via
`include_bytes!`), iterating it needs only a **server restart**, not a `cargo`
rebuild — unless you changed the loader itself.

## The dev cycle

```bash
# One-time (idempotent): clone forks, pin submodule, patch CMakeLists, and
# bootstrap the userspace gcc-13 toolchain (~a few hundred MB, first run only).
scripts/dev/delta-dev-setup.sh

# Build the extension against the local kernel (first run compiles DuckDB v1.5.4
# from source — slow; later runs are incremental). Resource-capped for the 8 GiB
# container: linker jobs=2, compile/cargo jobs=4 (override via
# DELTA_BUILD_LINK_JOBS / DELTA_BUILD_COMPILE_JOBS).
scripts/dev/delta-build.sh                       # prints the export line

export THUNDERDUCK_DELTA_EXT_PATH='.../.duckdb-delta/build/release/extension/delta/delta.duckdb_extension'

# Iterate: edit .delta-kernel-rs (and/or .duckdb-delta), then rebuild and
# restart. Use the worktree-scoped, ownership-verified killer — NEVER
# `pkill -f thunderduck-connect-server`, which crosses worktrees (see
# CLAUDE.md "Per-worktree test isolation").
scripts/dev/delta-build.sh && ./tests/scripts/kill-test-servers.sh
./target/release/thunderduck-connect-server &    # sessions now LOAD delta_scan
```

## Gotchas

- **Checkouts live under this worktree.** `ExitWorktree --remove` deletes
  `.delta-kernel-rs/`, `.duckdb-delta/`, and their (large) build trees. Use
  `--keep` to preserve them, or move the work to a durable branch first.
- **Kernel bump breaks the FFI.** Moving `.delta-kernel-rs` past v0.21.0 (toward
  v0.25.0+) changes the FFI surface `duckdb-delta` compiles against; reconciling
  that FFI is the actual feature work this loop exists to enable — expect the
  extension's C++ to need edits after a kernel bump.
- **DuckDB version drift.** If thunderduck's `duckdb` crate is bumped, re-pin the
  `.duckdb-delta/duckdb` submodule to the matching tag or the extension won't load.
- **This is not the production path.** Shipping Delta means wiring τ's read/write
  emission (a `FileFormat::Delta` → `delta_scan` arm, and the write seam in
  `service.rs`) and deciding how the extension is distributed — out of scope here.
