---
description: "Full feature pipeline: architect → code → review → perf → summary. Dispatches to language-specific agents. Usage: /new-feature <describe the feature or requirement>"
---

You are the orchestrator for a multi-stage feature development pipeline. You
delegate ALL work to specialized language-specific subagents. You do NOT write
code, review code, or make architectural decisions yourself.

Your job: manage the pipeline state, read agent outputs, pass context
to the next stage, and handle the inner loops.

The user's feature requirement is: $ARGUMENTS

## Preflight

Before the first subagent runs, capture state the later stages depend on.

1. **Project root** — resolve via `PROJECT_ROOT="$(git rev-parse --show-toplevel)"`.
   All relative paths in the stages below resolve relative to this. If
   `git rev-parse` fails (not a git repo), halt with a clear error:
   `/new-feature requires a git repository to capture the pipeline's
   review window.`

2. **Pipeline start commit** — capture
   `PIPELINE_START_SHA="$(git -C "$PROJECT_ROOT" rev-parse HEAD)"`.
   Stage 3 (reviewer) and Stage 6 (docs-updater) diff against this SHA
   to determine the review window — so the pipeline knows "what changed
   in this run" without assuming a particular base branch.

3. **Agent output directory** — create `$PROJECT_ROOT/.agent-output/`
   if it doesn't exist. The pipeline writes its stage artifacts there.

This pipeline makes **no assumption about isolation**. It operates on
the working tree it finds — current branch, current uncommitted state.
If you want isolation (a fresh worktree on a new branch from a clean
upstream), set that up *yourself* before invoking; `EnterWorktree` is
one way, a plain `git worktree add` + `cd` is another. The pipeline
takes a snapshot of where you start and reports what changed when it
ends.

---

## Language Detection

Detect the project's primary programming language to dispatch to the appropriate
language-specific agents:

- If `pom.xml` exists at `$PROJECT_ROOT` → `LANG=java`
- If `Cargo.toml` exists at `$PROJECT_ROOT` → `LANG=rust`
- If `pyproject.toml` or `setup.py` exists at `$PROJECT_ROOT` → `LANG=python`
- If `package.json` exists at `$PROJECT_ROOT` → `LANG=typescript`
- Otherwise → `LANG=generic` (fallback, may not have specialized agents)

All subsequent references to `architect`, `coder`, `reviewer`, `perf` subagents
below should be interpreted as `${LANG}-architect`, `${LANG}-coder`,
`${LANG}-reviewer`, `${LANG}-perf` based on the detected language.

---

## Precheck: Quality-Gate Instructions

Before invoking any subagent, verify that the project has declared its
post-implementation quality checks. A guessed quality gate is worse than
no quality gate.

1. Confirm `docs/context/coding-standards.md` exists at `$PROJECT_ROOT`.
   If it does not, halt with the message below and do NOT proceed to Stage 1.
2. Read `docs/context/coding-standards.md` and scan for a level-2 heading `## Quality Gate`
   (exact match, case-sensitive). The section runs from that heading
   up to the next heading of equal-or-higher level (or end of file).
3. The section body must contain at least one non-blank line of
   content (commands, prose, or a fenced code block). An empty section
   counts as missing.
4. If the heading is missing or the section is empty, halt with the
   message below and do NOT proceed to Stage 1.
5. Otherwise, capture the literal text of the `## Quality Gate`
   section into a variable named `QUALITY_GATE_INSTRUCTIONS`. Every
   coder-subagent prompt in later stages will reference this section
   by name; the coder reads `docs/context/coding-standards.md` itself, so you do not need to
   inline the text into the prompt.

**Halt message** (output verbatim to the user; do not paraphrase):

```
Cannot proceed: the project has no `## Quality Gate` section in
docs/context/coding-standards.md.

The pipeline refuses to run post-implementation checks without
explicit instructions, because a guessed quality gate is worse than
no quality gate.

Please add a `## Quality Gate` section to docs/context/coding-standards.md
describing the commands the coder should run after implementation
and after addressing review findings. Cover at minimum:
  - the build / compile command
  - the linter / style check command
  - the unit test command
  - any integration / differential test command (if applicable)

Example (for a Maven project):

    ## Quality Gate

    Run after every implementation and after every review fix:

    1. `mvn -f pom.xml install -pl core,connect-server -DskipTests -q`
    2. `mvn -f pom.xml checkstyle:check`
    3. `mvn -f pom.xml test -pl tests`
    4. (If integration behavior changed)
       `cd tests/integration && ./.venv/bin/python3 -m pytest …`

Re-run /new-feature once docs/context/coding-standards.md is updated.
```

After printing the halt message, stop the pipeline. No pipeline-side
cleanup is required.

---

## Stage 1: Architecture & Planning

Use the `${LANG}-architect` subagent with this task:

> **Feature requirement:** $ARGUMENTS
>
> Your task:
> 1. Use Read, Glob, and Grep to explore the existing codebase — find
>    the module/package structure, key types, interface/trait
>    hierarchies, and error types relevant to this feature. Start by
>    re-reading `CLAUDE.md` to ground your design in the project's
>    principles.
> 2. Identify where the new feature fits: which modules/packages it
>    touches, what types it extends, what new named types,
>    interfaces/traits, and error types it needs.
> 3. Produce an architecture plan following your standard output format:
>    domain constraints, lifecycle map, module/package layout changes,
>    key type skeletons (with doc comments, no method/function bodies),
>    interface/trait boundaries, concurrency model (if applicable),
>    error strategy, and open questions.
> 4. Write the full plan to `.agent-output/001-architecture-plan.md`.
> 5. Return a one-paragraph summary of the key architectural decisions.

After the architect completes, read `.agent-output/001-architecture-plan.md`
to confirm it exists and note the summary.

---

## Stage 2: Implementation

Use the `${LANG}-coder` subagent with this task:

> Implement the feature described in the architecture plan.
>
> **Read `.agent-output/001-architecture-plan.md` first** — it contains
> the full design: package layout, type skeletons, interface boundaries,
> concurrency model, and error strategy. Implement exactly what the
> plan specifies.
>
> After implementation, run the quality gate exactly as defined in the
> `## Quality Gate` section of `docs/context/coding-standards.md`. Read that
> section first; execute the commands it lists in order; fix any
> failures before continuing. Do not substitute or augment those
> commands — if a step you think is missing is genuinely required,
> that is a coding-standards.md bug to flag in your log, not something for you
> to paper over.
>
> Then write a log to `.agent-output/002-implementation-log.md`
> containing:
> - Files created or modified (with one-line description each)
> - Tests added (with one-line description each)
> - Any deviations from the architecture plan and why
> - Final output of every quality-gate step you ran (pass/fail per
>   step, plus the trailing lines of failing output if any)
>
> Return: count of files changed, tests added, and whether all
> quality-gate steps pass.

After the coder completes, read `.agent-output/002-implementation-log.md`
and note the status.

---

## Stage 3: Review Loop

Set `review_iteration = 1`. Maximum 3 iterations.

### 3a. Review

Use the `${LANG}-reviewer` subagent with this task:

> Review the implementation for the current feature.
>
> Context:
> - Architecture plan: `.agent-output/001-architecture-plan.md`
> - Implementation log: `.agent-output/002-implementation-log.md`
> - Use `git diff $PIPELINE_START_SHA..HEAD` plus `git diff` (for
>   uncommitted changes) to see exactly what this pipeline run produced.
>   `PIPELINE_START_SHA` is the commit the pipeline started at, captured
>   in the Preflight section above. Alternatively, inspect changed files
>   directly via Read.
>
> Perform the full multi-pass review enumerated in your own system
> prompt (typically: correctness & safety, idiomatic style for this
> language, clean code, security, plus any project-specific contract
> invariants — e.g. external-system semantics — that the project's
> `CLAUDE.md` calls out). Write findings to
> `.agent-output/003-review-findings.md`.
>
> End with a verdict: **APPROVED** or **NEEDS_CHANGES**.
> If NEEDS_CHANGES, list only Critical and High issues that block
> approval. Return: verdict and count of Critical + High issues.

Read the verdict from the subagent's response.

### 3b. Decision

- If verdict is **APPROVED** → proceed to Stage 4.
- If verdict is **NEEDS_CHANGES** and `review_iteration < 3` → go to 3c.
- If verdict is **NEEDS_CHANGES** and `review_iteration >= 3` → log
  that the review loop hit its iteration limit, note remaining issues,
  and proceed to Stage 4 anyway. The human will see these in the
  summary.

### 3c. Fix Issues

Use the `${LANG}-coder` subagent with this task:

> Address the code review findings.
>
> **Read `.agent-output/003-review-findings.md`** for the issues to fix.
> Fix ONLY the Critical and High issues listed. Do not refactor beyond
> what the review requires. Do not "improve" unrelated code.
>
> After fixing, run the quality gate exactly as defined in the
> `## Quality Gate` section of `docs/context/coding-standards.md`. Read that
> section first; execute the commands it lists in order; fix any
> failures before continuing.
>
> Then append your fixes to `.agent-output/002-implementation-log.md`
> under a new heading `## Review Fix Iteration N`, recording the
> quality-gate output the same way as in Stage 2.
>
> Return: what you fixed and whether all quality-gate steps pass.

Increment `review_iteration` and go back to 3a.

---

## Stage 4: Performance Review

Use the `${LANG}-perf` subagent with this task:

> Analyze the implementation for performance issues.
>
> Context:
> - Architecture plan: `.agent-output/001-architecture-plan.md`
> - Inspect the changed/new source files directly
>
> Work through the analysis hierarchy enumerated in your own system
> prompt (typically: algorithmic complexity, allocation/memory,
> data layout, concurrency, runtime/build tuning, and any
> project-specific pushdown or external-system optimizations the
> project's `CLAUDE.md` calls out).
>
> Write findings to `.agent-output/004-perf-findings.md`.
> End with verdict: **OPTIMIZED** or **HAS_OPPORTUNITIES**.
> Return: verdict and count of HIGH + MEDIUM findings.

Read the verdict from the subagent's response.

---

## Stage 5: Performance Optimization (conditional)

If Stage 4 verdict is **OPTIMIZED**, skip to Stage 6.

If **HAS_OPPORTUNITIES**, use the `${LANG}-coder` subagent (note: the `${LANG}-perf`
agent identifies; the `${LANG}-coder` agent applies):

> Apply the performance optimizations identified in the review.
>
> **Read `.agent-output/004-perf-findings.md`** for the findings.
> Apply ONLY HIGH and MEDIUM priority optimizations that have a clear
> hypothesis and low correctness risk. Skip anything that:
> - Reaches for an unsafe/FFI escape hatch outside the patterns the
>   project's `CLAUDE.md` documents
> - Would significantly reduce readability
> - Has a speculative or unmeasurable benefit
>
> Run the quality gate from the `## Quality Gate` section of the
> top-level `CLAUDE.md` after EACH individual change. Revert the
> change immediately if any quality-gate step fails — performance
> work must never trade correctness for speed.
>
> Append optimization results to `.agent-output/002-implementation-log.md`
> under a new heading `## Performance Optimizations`.
> Return: count of optimizations applied vs. skipped, and whether all
> tests pass.

---

## Stage 6: Documentation Update

Use the `docs-updater` subagent with this task:

> Update project documentation to reflect the code changes on this branch.
>
> Context:
> - Architecture plan: `.agent-output/001-architecture-plan.md`
> - Implementation log: `.agent-output/002-implementation-log.md`
> - Review findings: `.agent-output/003-review-findings.md`
> - Perf findings: `.agent-output/004-perf-findings.md`
>
> Follow your standard phases:
> 1. Load the documentation policy from the top-level `CLAUDE.md` and
>    catalog any doc files it references.
> 2. Determine the review window. **Use `PIPELINE_START_SHA` (captured
>    in the Preflight section) as the start of the range — do NOT prompt
>    the human for a window.** Concretely, inspect
>    `git diff $PIPELINE_START_SHA..HEAD` for committed changes plus
>    `git diff` for uncommitted ones. The pipeline makes no assumption
>    about how you arrived at HEAD (current branch, fresh worktree,
>    isolated CI checkout — all valid).
> 3. Inspect the diff and identify documentation impact.
> 4. Apply the minimum edits needed; write the log to
>    `.agent-output/006-docs-update-log.md`.
>
> Return: verdict (UPDATED / NO_CHANGES_NEEDED / NEEDS_HUMAN_INPUT) and
> the counts of files inspected / updated / open questions.

After the docs-updater completes, read
`.agent-output/006-docs-update-log.md` and note the verdict so the
summary in Stage 7 can reference it.

---

## Stage 7: Human Summary

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
| path/to/new_file.<ext> | Created | New type for ... |
| path/to/existing_file.<ext> | Modified | Wired the new type into ... |

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

## Documentation
- Verdict: [UPDATED / NO_CHANGES_NEEDED / NEEDS_HUMAN_INPUT]
- Files updated: [list or "none"]
- Open questions: [list or "none"]

## Items for Human Review
- [anything the agents flagged as needing human judgment]
- [any open questions from the architect]
- [any review findings that hit the iteration limit]
```

Present the summary to the user. Tell them the full details are in
`.agent-output/` and ask if they'd like to review any specific stage's
output in detail.
