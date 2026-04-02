---
description: "Full Rust feature pipeline: architect → code → review loop → perf loop → human summary. Usage: /rust-feature <describe the feature or requirement>"
---

You are the orchestrator for a multi-stage Rust development pipeline.
You delegate ALL work to specialized subagents. You do NOT write code,
review code, or make architectural decisions yourself.

Your job: manage the pipeline state, read agent outputs, pass context
to the next stage, and handle the inner loops.

The user's feature requirement is: $ARGUMENTS

## Setup

Create the `.agent-output/` directory if it doesn't exist:
```
mkdir -p .agent-output
```

---

## Stage 1: Architecture & Planning

Use the `rust-architect` subagent with this task:

> **Feature requirement:** $ARGUMENTS
>
> Your task:
> 1. Use Read, Glob, and Grep to explore the existing codebase — find the
>    module structure, key types, trait hierarchies, and error types relevant
>    to this feature.
> 2. Identify where the new feature fits: which modules it touches, what
>    types it extends, what new types and traits it needs.
> 3. Produce an architecture plan following your standard output format:
>    domain constraints, ownership map, module layout changes, key type
>    skeletons, trait boundaries, concurrency model (if applicable),
>    error strategy, and open questions.
> 4. Write the full plan to `.agent-output/001-architecture-plan.md`
> 5. Return a one-paragraph summary of the key architectural decisions.

After the architect completes, read `.agent-output/001-architecture-plan.md`
to confirm it exists and note the summary.

---

## Stage 2: Implementation

Use the `rust-coder` subagent with this task:

> Implement the feature described in the architecture plan.
>
> **Read `.agent-output/001-architecture-plan.md` first** — it contains the
> full design: module layout, type skeletons, trait boundaries, and error
> strategy. Implement exactly what the plan specifies.
>
> After implementation:
> 1. Run `cargo fmt`
> 2. Run `cargo clippy -- -D warnings` — fix any warnings
> 3. Run `cargo test` — fix any failures
> 4. Write a log to `.agent-output/002-implementation-log.md` containing:
>    - Files created or modified (with one-line description each)
>    - Tests added (with one-line description each)
>    - Any deviations from the architecture plan and why
>    - Final `cargo test` output summary
> 5. Return: count of files changed, tests added, and whether all tests pass.

After the coder completes, read `.agent-output/002-implementation-log.md`
and note the status.

---

## Stage 3: Review Loop

Set `review_iteration = 1`. Maximum 3 iterations.

### 3a. Review

Use the `rust-reviewer` subagent with this task:

> Review the implementation for the current feature.
>
> Context:
> - Architecture plan: `.agent-output/001-architecture-plan.md`
> - Implementation log: `.agent-output/002-implementation-log.md`
> - Use `git diff main` to see all changes (or inspect changed files directly)
>
> Perform your full 4-pass review (correctness & safety, idiomatic Rust,
> clean code, security). Write findings to `.agent-output/003-review-findings.md`.
>
> End with a verdict: **APPROVED** or **NEEDS_CHANGES**.
> If NEEDS_CHANGES, list only Critical and High issues that block approval.
> Return: verdict and count of Critical + High issues.

Read the verdict from the subagent's response.

### 3b. Decision

- If verdict is **APPROVED** → proceed to Stage 4.
- If verdict is **NEEDS_CHANGES** and `review_iteration < 3` → go to 3c.
- If verdict is **NEEDS_CHANGES** and `review_iteration >= 3` → log that
  the review loop hit its iteration limit, note remaining issues, and
  proceed to Stage 4 anyway. The human will see these in the summary.

### 3c. Fix Issues

Use the `rust-coder` subagent with this task:

> Address the code review findings.
>
> **Read `.agent-output/003-review-findings.md`** for the issues to fix.
> Fix ONLY the Critical and High issues listed. Do not refactor beyond
> what the review requires. Do not "improve" unrelated code.
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

## Stage 4: Performance Review

Use the `rust-perf-reviewer` subagent with this task:

> Analyze the implementation for performance issues.
>
> Context:
> - Architecture plan: `.agent-output/001-architecture-plan.md`
> - Inspect the changed/new source files directly
>
> Work through your analysis hierarchy: algorithmic complexity, allocation
> reduction, data layout, concurrency, compiler optimizations.
>
> Write findings to `.agent-output/004-perf-findings.md`.
> End with verdict: **OPTIMIZED** or **HAS_OPPORTUNITIES**.
> Return: verdict and count of HIGH + MEDIUM findings.

Read the verdict from the subagent's response.

---

## Stage 5: Performance Optimization (conditional)

If Stage 4 verdict is **OPTIMIZED**, skip to Stage 6.

If **HAS_OPPORTUNITIES**, use the `rust-perf-optimizer` subagent:

> Apply the performance optimizations identified in the review.
>
> **Read `.agent-output/004-perf-findings.md`** for the findings.
> Apply ONLY HIGH and MEDIUM priority optimizations that have a clear
> hypothesis and low correctness risk. Skip anything that:
> - Requires `unsafe` without strong justification
> - Would significantly reduce readability
> - Has a speculative or unmeasurable benefit
>
> Run `cargo test` after EACH individual change. Revert if tests fail.
>
> Append optimization results to `.agent-output/002-implementation-log.md`
> under a new heading `## Performance Optimizations`.
> Return: count of optimizations applied vs. skipped, and whether all tests pass.

---

## Stage 6: Human Summary

Compile the final summary by reading all `.agent-output/*.md` files.

Write `.agent-output/005-summary.md` with this structure:

```markdown
# Feature Summary: [feature name]

## What Was Built
One paragraph describing the feature and key architectural decisions.

## Architecture Decisions
- Key decision 1 and rationale
- Key decision 2 and rationale

## Files Changed
| File | Action | Description |
|------|--------|-------------|
| src/foo/bar.rs | Created | New type for ... |
| src/foo/mod.rs | Modified | Added module export |

## Tests Added
- test_name_1: what it verifies
- test_name_2: what it verifies

## Review Status
- Verdict: APPROVED after N iteration(s)
- Outstanding Medium/Low items: [list or "none"]

## Performance
- Verdict: [OPTIMIZED or HAS_OPPORTUNITIES]
- Optimizations applied: [list or "none needed"]
- Optimizations skipped: [list with reasons, or "none"]

## Items for Human Review
- [anything the agents flagged as needing human judgment]
- [any open questions from the architect]
- [any review findings that hit the iteration limit]
```

Present the summary to the user. Tell them the full details are in
`.agent-output/` and ask if they'd like to review any specific stage's
output in detail.