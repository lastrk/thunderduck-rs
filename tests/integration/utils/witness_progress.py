#!/usr/bin/env python3
"""Per-case progress comparator for the select-block review-findings goal.

Consumes pytest ``-v`` logs from ``run-differential-tests.sh core`` +
``sql_v2`` runs and reports, against a checked-in baseline PASS set and the
witness manifest:

- ``REGRESSIONS: <n>`` — baseline-PASS cases now non-PASS (or missing from
  the run), each listed. Any regression => exit 1. This is the hard gate.
- ``WITNESS FLIPS: <k>/<total>`` — manifest witness cases now PASS, each
  listed with the finding it pins. Progress signal only — un-flipped
  witnesses never fail the run.

Modes:
  witness_progress.py report  BASELINE MANIFEST LOG [LOG...]
  witness_progress.py capture BASELINE LOG [LOG...]
      (re)writes BASELINE with the sorted ``file::case_id`` PASS set of the
      given logs. Baseline regeneration must be an intentional, explained
      commit (see tasks/goal-implement-review-findings.md).
"""

from __future__ import annotations

import json
import re
import sys

CASE_RX = re.compile(
    r"(\S+?)::test_case\[(.+?)\]\s+(PASSED|FAILED|ERROR|SKIPPED|XFAIL|XPASS)"
)


def parse_logs(paths):
    """(file, case_id) -> status; last occurrence wins (retries/reruns)."""
    out = {}
    for path in paths:
        with open(path, errors="replace") as fh:
            for line in fh:
                m = CASE_RX.search(line)
                if m:
                    out[(m.group(1), m.group(2))] = m.group(3)
    return out


def capture(baseline_path, logs):
    cases = parse_logs(logs)
    passed = sorted(f"{f}::{c}" for (f, c), s in cases.items() if s == "PASSED")
    if not passed:
        print("witness_progress: no PASSED test_case lines found — refusing "
              "to write an empty baseline (wrong/truncated logs?)")
        return 1
    with open(baseline_path, "w") as fh:
        fh.write("\n".join(passed) + "\n")
    print(f"baseline captured: {len(passed)} passing cases -> {baseline_path}")
    return 0


def report(baseline_path, manifest_path, logs):
    with open(baseline_path) as fh:
        baseline = [ln.strip() for ln in fh if ln.strip()]
    with open(manifest_path) as fh:
        manifest = json.load(fh)["witnesses"]

    cases = parse_logs(logs)
    by_id = {}  # case_id -> status (ids are unique across the two corpora)
    current = {}
    for (f, c), s in cases.items():
        current[f"{f}::{c}"] = s
        by_id[c] = s

    regressions = []
    for key in baseline:
        status = current.get(key)
        if status != "PASSED":
            regressions.append((key, status or "MISSING"))

    flips = [(w["case"], w["finding"]) for w in manifest
             if by_id.get(w["case"]) == "PASSED"]

    print(f"cases parsed: {len(current)}   baseline PASS set: {len(baseline)}")
    print(f"REGRESSIONS: {len(regressions)}")
    for key, status in regressions:
        print(f"  {key}  PASSED->{status}")
    print(f"WITNESS FLIPS: {len(flips)}/{len(manifest)}")
    for case, finding in flips:
        print(f"  {case}  ({finding})  red->PASSED")
    for w in manifest:
        if by_id.get(w["case"]) is None:
            print(f"  WARNING: witness {w['case']} not found in the logs")
    return 1 if regressions else 0


def main(argv):
    if len(argv) >= 4 and argv[1] == "report":
        return report(argv[2], argv[3], argv[4:])
    if len(argv) >= 3 and argv[1] == "capture":
        return capture(argv[2], argv[3:])
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
