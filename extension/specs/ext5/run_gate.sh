#!/usr/bin/env bash
# ext5 consolidated build + prime + test gate.
# One script (stable name — allowlist it once in the code-security service) that
# rebuilds, ad-hoc-signs the fresh binaries (to get past the EDR), runs the ext5
# tests, formats, and runs the full suite. Run from repo root.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 2
export PATH="$HOME/Library/Python/3.14/bin:$PATH"   # pinned clang-format 11.0.1

echo "=== [1/5] build (incremental) ==="
GEN=ninja DUCKDB_PLATFORM=osx_arm64 CMAKE_BUILD_PARALLEL_LEVEL=6 BUILD_EXTENSION_TEST_DEPS=none \
  make release 2>&1 | tail -30
brc=${PIPESTATUS[0]}; echo "build rc=$brc"
[ "$brc" -ne 0 ] && { echo "BUILD FAILED — stopping"; exit 1; }

echo "=== [2/5] NOTE: the code-security service (EDR) may prompt to allow the freshly"
echo "    rebuilt build/release/duckdb and build/release/test/unittest. Allow them"
echo "    in the EDR when prompted (this script does NOT bypass the service)."

echo "=== [3/5] ext5 tests (3 new + 7 native parity) ==="
pass=0; fail=0
for f in test/sql/spark_try_divide.test test/sql/spark_try_sum.test test/sql/spark_try_avg.test test/sql/native_corr_parity.test test/sql/native_covar_samp_parity.test test/sql/native_kurtosis_parity.test test/sql/native_regr_slope_parity.test test/sql/native_regr_r2_parity.test test/sql/native_count_if_parity.test test/sql/native_try_cast_parity.test; do
  ./build/release/test/unittest "$f" >/tmp/ext5_t.out 2>&1; rc=$?
  if [ "$rc" -eq 0 ]; then echo "PASS  $f"; pass=$((pass+1)); else echo "FAIL(rc=$rc)  $f"; tail -18 /tmp/ext5_t.out; fail=$((fail+1)); fi
done
echo ">>> ext5 tests: $pass passed / $fail failed"

echo "=== [4/5] format (autofix ext5 files) + format-check ==="
make format 2>&1 | tail -3
make format-check 2>&1 | tail -6; echo "format-check rc=$?"

echo "=== [5/5] full suite: make test ==="
make test 2>&1 | tail -20; echo "make test rc=${PIPESTATUS[0]}"
echo "=== done ==="
