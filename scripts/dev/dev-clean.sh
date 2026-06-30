#!/usr/bin/env bash
# dev-clean.sh — clean ONLY the first-party crates, never the dependencies.
#
# Plain `cargo clean` wipes all of target/ — including the ~1.9 GB libduckdb.a
# and every compiled dependency — forcing a slow rebuild. This scoped clean
# removes just our crates, so DuckDB and third-party deps stay built.
#
# (Even a full `cargo clean` is safe for the *cache*: the immutable artifacts
# live in .build-cache/ outside target/, so the next build re-thaws DuckDB and
# re-uses sccache. This wrapper just avoids paying for that re-thaw.)
set -euo pipefail

FIRST_PARTY=(thunderduck-core thunderduck-connect-server)

args=()
for c in "${FIRST_PARTY[@]}"; do args+=(-p "$c"); done

echo "[dev-clean] cargo clean ${args[*]} $*"
exec cargo clean "${args[@]}" "$@"
