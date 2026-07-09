---
description: "Bug fix pipeline: diagnose → code → review → verify. Dispatches to language-specific agents. Usage: /fix-bug <describe the bug, symptom, or failing test>"
---

You are the orchestrator for a multi-stage bug fix pipeline. You delegate ALL
work to specialized language-specific subagents. You do NOT write code,
diagnose bugs, or review code yourself.

Your job: manage the pipeline state, read agent outputs, pass context
to the next stage, and handle the review loop.

The user's bug report is: $ARGUMENTS

## Preflight

Before the first subagent runs, capture state the later stages depend on.

1. **Project root** — resolve via `PROJECT_ROOT="$(git rev-parse --show-toplevel)"`.
   All relative paths in the stages below resolve relative to this. If
   `git rev-parse` fails (not a git repo), halt with a clear error:
   `/fix-bug requires a git repository to capture the pipeline's
   review window.`

2. **Pipeline start commit** — capture
   `PIPELINE_START_SHA="$(git -C "$PROJECT_ROOT" rev-parse HEAD)"`.
   Stage 6 (docs-updater) diffs against this SHA to determine the
   review window — so the pipeline knows "what changed in this run"
   without assuming a particular base branch.

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

All subsequent references to `diagnostician`, `coder`, `reviewer` subagents
below should be interpreted as `${LANG}-diagnostician`, `${LANG}-coder`,
`${LANG}-reviewer` based on the detected language.

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

Re-run /fix-bug once docs/context/coding-standards.md is updated.
```

After printing the halt message, stop the pipeline. No pipeline-side
cleanup is required.

---

## Stage 1: Diagnosis

Use the `${LANG}-diagnostician` subagent with this task:

> **Bug report:** $ARGUMENTS
>
> Follow the scientific method (Phases 1–5 from your system prompt):
>
> 1. **Observe**: Reproduce the bug. Capture the exact symptom — compile
>    error, lint violation, failing unit test, failing integration test,
>    wrong output. Map the data flow from source to sink through the
>    layers your project's top-level `CLAUDE.md` (and your own system
>    prompt) describes.
> 2. **Hypothesize**: Generate 3–5 competing hypotheses for the root
>    cause. Each must be specific, testable, and falsifiable. Draw from
>    the project- and language-specific failure categories your system
>    prompt enumerates.
> 3. **Experiment**: Test hypotheses in priority order. Use whatever
>    diagnostic instrumentation is idiomatic for this project — temporary
>    log statements, ad-hoc scripts, targeted REPL/CLI queries, or
>    inspection of the project's own log artifacts. Record exact outputs.
> 4. **Diagnose**: Write the root cause statement with the broken step,
>    mechanism, and evidence.
> 5. **Prescribe**: Propose the minimal correct fix — the smallest diff
>    that resolves the root cause. Predict side effects (does it touch a
>    symmetric/dual code path that also needs updating?).
>
> Write the full diagnostic report to
> `.agent-output/001-diagnostic-report.md`. **Clean up all diagnostic
> artifacts** (DIAGNOSTIC log lines, temporary assertions) before
> completing.
>
> Return: one-paragraph root cause summary and the prescribed fix.

After the diagnostician completes, read
`.agent-output/001-diagnostic-report.md` to confirm the diagnosis and
prescribed fix.

---

## Stage 2: Implementation

Use the `${LANG}-coder` subagent with this task:

> Fix the bug described in the diagnostic report.
>
> **Read `.agent-output/001-diagnostic-report.md` first** — it contains
> the root cause analysis and prescribed fix. Implement EXACTLY the
> prescribed fix. Do not add unrelated improvements or refactors.
>
> After implementation, run the quality gate exactly as defined in the
> `## Quality Gate` section of `docs/context/coding-standards.md`. Read that
> section first; execute the commands it lists in order; fix any
> failures before continuing. Do not substitute or augment those
> commands — if a step you think is missing is genuinely required,
> that is a coding-standards.md bug to flag in your log, not something for you
> to paper over. (The orchestrator already verified the section exists;
> if it has somehow gone missing, stop and tell the user rather than
> guessing.)
>
> Additionally, run any reproduction step the diagnostic report
> identifies (the test, query, or invocation that triggered the bug)
> to confirm the original symptom is gone.
>
> Then write a log to `.agent-output/002-implementation-log.md`
> containing:
> - Files modified (with one-line description each)
> - Tests added (regression test that captures the original failure)
> - Any deviations from the prescription and why
> - Final output of every quality-gate step you ran (pass/fail per
>   step, plus the trailing lines of failing output if any)
> - Result of the bug-reproduction step
>
> Return: count of files changed, tests added, and whether all
> quality-gate steps and the reproduction pass.

After the coder completes, read `.agent-output/002-implementation-log.md`
and note the status.

---

## Stage 3: Review Loop

Set `review_iteration = 1`. Maximum 3 iterations.

### 3a. Review

Use the `${LANG}-reviewer` subagent with this task:

> Review the bug fix implementation.
>
> Context:
> - Diagnostic report: `.agent-output/001-diagnostic-report.md`
> - Implementation log: `.agent-output/002-implementation-log.md`
> - Inspect the changed files directly via Read
>
> Focus on:
> 1. Does the fix address the diagnosed root cause?
> 2. Does it introduce regressions or new bugs?
> 3. Is the fix minimal and correct?
> 4. Are there edge cases the fix misses? Apply the language-specific
>    edge-case checklist from your own system prompt (e.g. NULL/None
>    handling, exhaustiveness, symmetric/dual code paths, ownership and
>    lifetime invariants, error-propagation boundaries).
> 5. Are any external-system semantics or contract invariants the
>    project's `CLAUDE.md` calls out still preserved?
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

Use the `${LANG}-coder` subagent with this task:

> Address the code review findings for the bug fix.
>
> **Read `.agent-output/003-review-findings.md`** for the issues to fix.
> Fix ONLY the Critical and High issues listed.
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

## Stage 4: Verification

Use the `${LANG}-coder` subagent with this task:

> Verify the bug fix resolves the original symptom and does not introduce
> regressions.
>
> Context:
> - Diagnostic report: `.agent-output/001-diagnostic-report.md`
> - Implementation log: `.agent-output/002-implementation-log.md`
> - Original bug report: $ARGUMENTS
>
> Steps:
> 1. Re-run the specific reproducer named in the bug report (or the
>    failing test the diagnostic report identifies). Use whichever
>    test-invocation command from the `## Quality Gate` section of
>    `docs/context/coding-standards.md` runs a single targeted test; if the gate
>    section does not document a single-test invocation, document that
>    gap in the verification log and run the gate's standard test
>    command instead.
> 2. Run the full quality gate from `docs/context/coding-standards.md` (every command in the
>    `## Quality Gate` section, in order) to check for regressions in
>    unrelated areas.
> 3. Write `.agent-output/004-verification-log.md` containing:
>    - The exact commands you ran
>    - The output summary for each command (pass/fail, plus failing
>      tail if any)
>    - An explicit **PASS** or **FAIL** for the original reproducer
>    - A list of any new regressions (or "none")
> 4. Do NOT modify any source files in this stage — verification only.
>
> Return: PASS or FAIL for the original reproducer, and the count of new
> regressions (if any).

After the subagent completes, read `.agent-output/004-verification-log.md`
and note the verdict so the Stage 6 summary can reference it.

---

## Stage 5: Documentation Update

Use the `docs-updater` subagent with this task:

> Update project documentation to reflect the bug fix on this branch.
>
> Context:
> - Diagnostic report: `.agent-output/001-diagnostic-report.md`
> - Implementation log: `.agent-output/002-implementation-log.md`
> - Review findings: `.agent-output/003-review-findings.md`
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
> 3. Inspect the diff and identify documentation impact. Bug fixes
>    often have low doc impact — that is fine; NO_CHANGES_NEEDED is a
>    valid verdict. Pay particular attention to any "gotchas" or
>    "lessons learned" sections in `CLAUDE.md` or `docs/context/` that
>    this bug suggests should be updated.
> 4. Apply the minimum edits needed; write the log to
>    `.agent-output/006-docs-update-log.md`.
>
> Return: verdict (UPDATED / NO_CHANGES_NEEDED / NEEDS_HUMAN_INPUT) and
> the counts of files inspected / updated / open questions.

After the docs-updater completes, read
`.agent-output/006-docs-update-log.md` and note the verdict so the
summary in Stage 6 can reference it.

---

## Stage 6: Summary

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
| path/to/file.<ext> | What changed |

## Tests
- Added: [list new regression tests, or "none"]
- Verified: [the originally failing test now passes]
- Regressions: [none, or list]

## Review Status
- Verdict: [APPROVED after N iterations]
- Outstanding items: [list or "none"]

## Documentation
- Verdict: [UPDATED / NO_CHANGES_NEEDED / NEEDS_HUMAN_INPUT]
- Files updated: [list or "none"]
- Open questions: [list or "none"]

## Prevention
[How to prevent this class of bug in the future — type-level,
test-level, or process-level safeguard]
```

Present the summary to the user. Tell them the full details are in
`.agent-output/` and ask if they'd like to review the diagnostic report.
