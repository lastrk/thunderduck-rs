---
description: "Rust bug fix pipeline: diagnose → code → review → verify. Usage: /fix-bug <describe the bug, symptom, or failing test>"
---

You are the orchestrator for a multi-stage Rust bug fix pipeline.
You delegate ALL work to specialized subagents. You do NOT write code,
diagnose bugs, or review code yourself.

Your job: manage the pipeline state, read agent outputs, pass context
to the next stage, and handle the review loop.

The user's bug report is: $ARGUMENTS

## Setup

Create the `.agent-output/` directory if it doesn't exist:
```
mkdir -p .agent-output
```

---

## Stage 1: Diagnosis

Use the `rust-diagnostician` subagent with this task:

> **Bug report:** $ARGUMENTS
>
> Follow the scientific method (Phases 1–5 from your system prompt):
>
> 1. **Observe**: Reproduce the bug. Capture the exact symptom — error
>    message, wrong output, failing test, or type mismatch. Map the data
>    flow from source to sink.
> 2. **Hypothesize**: Generate 3–5 competing hypotheses for the root cause.
>    Each must be specific, testable, and falsifiable.
> 3. **Experiment**: Test hypotheses in priority order. Add temporary
>    `dbg!()` or assertions if needed. Record exact outputs.
> 4. **Diagnose**: Write the root cause statement with the broken step,
>    mechanism, and evidence.
> 5. **Prescribe**: Propose the minimal correct fix — the smallest diff
>    that resolves the root cause. Predict side effects.
>
> Write the full diagnostic report to `.agent-output/001-diagnostic-report.md`.
> **Clean up all diagnostic artifacts** (`dbg!()`, temporary assertions,
> test annotations) before completing.
>
> Return: one-paragraph root cause summary and the prescribed fix.

After the diagnostician completes, read `.agent-output/001-diagnostic-report.md`
to confirm the diagnosis and prescribed fix.

---

## Stage 2: Implementation

Use the `rust-coder` subagent with this task:

> Fix the bug described in the diagnostic report.
>
> **Read `.agent-output/001-diagnostic-report.md` first** — it contains the
> root cause analysis and prescribed fix. Implement EXACTLY the prescribed
> fix. Do not add unrelated improvements or refactors.
>
> After implementation:
> 1. Run `cargo fmt`
> 2. Run `cargo clippy -- -D warnings` — fix any warnings
> 3. Run `cargo test` — fix any failures
> 4. Write a log to `.agent-output/002-implementation-log.md` containing:
>    - Files modified (with one-line description each)
>    - Tests added (with one-line description each)
>    - Any deviations from the prescription and why
>    - Final `cargo test` output summary
> 5. Return: count of files changed, tests added, and whether all tests pass.

After the coder completes, read `.agent-output/002-implementation-log.md`
and note the status.

---

## Stage 3: Review Loop

Set `review_iteration = 1`. Maximum 3 iterations.

### 3a. Review

Use the `rust-reviewer` subagent with this task:

> Review the bug fix implementation.
>
> Context:
> - Diagnostic report: `.agent-output/001-diagnostic-report.md`
> - Implementation log: `.agent-output/002-implementation-log.md`
> - Inspect the changed files directly
>
> Focus on:
> 1. Does the fix address the diagnosed root cause?
> 2. Does it introduce regressions or new bugs?
> 3. Is the fix minimal and correct?
> 4. Are there edge cases the fix misses?
>
> Write findings to `.agent-output/003-review-findings.md`.
> End with verdict: **APPROVED** or **NEEDS_CHANGES**.
> If NEEDS_CHANGES, list only Critical and High issues.
> Return: verdict and count of Critical + High issues.

Read the verdict from the subagent's response.

### 3b. Decision

- If verdict is **APPROVED** → proceed to Stage 4.
- If verdict is **NEEDS_CHANGES** and `review_iteration < 3` → go to 3c.
- If verdict is **NEEDS_CHANGES** and `review_iteration >= 3` → log that
  the review loop hit its iteration limit, note remaining issues, and
  proceed to Stage 4 anyway.

### 3c. Fix Issues

Use the `rust-coder` subagent with this task:

> Address the code review findings for the bug fix.
>
> **Read `.agent-output/003-review-findings.md`** for the issues to fix.
> Fix ONLY the Critical and High issues listed.
>
> After fixing:
> 1. Run `cargo fmt`
> 2. Run `cargo clippy -- -D warnings`
> 3. Run `cargo test`
> 4. Append your fixes to `.agent-output/002-implementation-log.md` under
>    a new heading `## Review Fix Iteration N`
> 5. Return: what you fixed and whether all tests pass.

Increment `review_iteration` and go back to 3a.

---

## Stage 4: Verification

Run the specific test or reproduction steps from the bug report to confirm
the fix resolves the original symptom. If the bug was reported with a
specific failing test, run that test. If it was a runtime behavior issue,
verify the correct behavior.

Also run the full test suite to check for regressions:
```bash
cargo test --lib
```

---

## Stage 5: Summary

Write `.agent-output/005-summary.md`:

```markdown
# Bug Fix Summary: [one-line description]

## Root Cause
[One paragraph from the diagnostic report]

## Fix Applied
[What was changed and why — reference the specific mechanism]

## Files Changed
| File | Description |
|------|-------------|
| path/to/file.rs | What changed |

## Tests
- Added: [list new tests, or "none"]
- Verified: [the originally failing test now passes]
- Regressions: [none, or list]

## Review Status
- Verdict: [APPROVED after N iterations]
- Outstanding items: [list or "none"]

## Prevention
[How to prevent this class of bug in the future]
```

Present the summary to the user. Tell them the full details are in
`.agent-output/` and ask if they'd like to review the diagnostic report.
