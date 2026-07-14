#!/usr/bin/env bash
# ext5 test-only runner (NO rebuild, NO signing). Uses the already-built
# build/release/test/unittest. Allow that binary in the code-security service
# once; .test files are read at runtime so no rebuild is needed for golden tweaks.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 2

TESTS="
test/sql/spark_try_divide.test
test/sql/spark_try_sum.test
test/sql/spark_try_avg.test
test/sql/native_corr_parity.test
test/sql/native_covar_samp_parity.test
test/sql/native_kurtosis_parity.test
test/sql/native_regr_slope_parity.test
test/sql/native_regr_r2_parity.test
test/sql/native_count_if_parity.test
test/sql/native_try_cast_parity.test
"
pass=0; fail=0
for f in $TESTS; do
  ./build/release/test/unittest "$f" >/tmp/ext5_t.out 2>&1; rc=$?
  if [ "$rc" -eq 0 ]; then echo "PASS  $f"; pass=$((pass+1))
  else echo "FAIL(rc=$rc)  $f"; tail -20 /tmp/ext5_t.out; fail=$((fail+1)); fi
done
echo ">>> ext5 targeted tests: $pass passed / $fail failed"
