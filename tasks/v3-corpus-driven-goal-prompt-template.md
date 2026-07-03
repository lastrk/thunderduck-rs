# v3 Corpus-Driven `/goal` Prompt Template

**Purpose.** Reusable `/goal` prompt for driving Thunderduck's v2 restart to
100% corpus green. **v3 supersedes `v2-corpus-driven-goal-prompt-template.md`**
by making the subagent orchestration explicit and prescriptive: named
subagent types, verbatim prompt templates, explicit else-branches, and named
handoff files. The parent (Claude) is instructed *how* to orchestrate, not
just *what* to accomplish.

**Companion documents:**
- `tasks/v2-corpus-driven-iteration-methodology.md` — the per-pass discipline (unchanged from v2 usage).
- `tasks/v2-corpus-driven-pass-log.md` — the rolling log.

## Design rationale (why v3 over v2)

The v2 template read as an aspirational directive; per-pass, the parent
had to invent subagent prompts on the fly, often forgetting to pass
essential context (diagnostic paths, pipeline-start SHA, warning budgets).
The v2 session (Passes 57–76, +89 corpus lift) worked because I re-derived
the missing context every pass — but it also produced inconsistent subagent
briefs and one API-error mid-session (Pass 70) when a prompt was too broad.

v3 fixes this by:

1. **Prescribing every `Agent(...)` call site** with a labeled template.
2. **Naming handoff files by pass** so downstream subagents always know
   where prior output lives.
3. **Explicit else-branches** at every fork (verdict, corpus regression,
   warning budget breach, HALT-AND-FLAG condition).
4. **A "prompt boilerplate" block** appended to every subagent prompt so
   the durable constraints (no-commit, warning budget, ADR citations,
   verification commands) travel with every dispatch.

The v3 prompt itself is still ≤4000 chars (fits the `/goal` size budget)
because the templates live in this file and are referenced by section
number.

---

## The `/goal` prompt (paste this into `/goal`)

```
/goal Drive Thunderduck v2 corpus to 100% e2e green via prescriptive-orchestration corpus-driven passes (v3)

**Template + methodology:**
- Orchestration template: `tasks/v3-corpus-driven-goal-prompt-template.md` — the per-pass Agent(...) call sites, verbatim prompts, and else-branches. MUST be re-read at pass start.
- Methodology: `tasks/v2-corpus-driven-iteration-methodology.md` — 5-fix-iteration cap, HALT-AND-FLAG triggers, zero-DEFER, compiler-warning discipline, pass-log format.

**Primary goal.** 324/324 core_v2 corpus cases green (`tests/scripts/v2-progress.sh`); byte-identical to Spark per ADR-015.

**Secondary goal (non-negotiable).** Zero DEFER; findings closed in the pass they surface. Zero new compiler warnings on files this pass modifies. No commits without user approval.

**Design authority:** `docs/thunderduck-rearchitect-ADRs.md` (ADR-000..ADR-022). Every fix cites applicable ADR.
**Open decisions:** `tasks/v2-restart-open-decisions.md`. New gaps → Decision 14+ (HALT-AND-FLAG-3).
**Inheritance checklist:** `tasks/v2-restart-inheritance-checklist.md`.
**Pass log:** `tasks/v2-corpus-driven-pass-log.md` (append one entry per pass, per methodology §Pass log).

**Baseline snapshot** (capture once at goal start; reuse across passes):
- `PIPELINE_START_SHA` = `git rev-parse HEAD`.
- Initial corpus count = `./tests/scripts/v2-progress.sh`.
- Initial warning count = `cargo build --release -p thunderduck-connect-server 2>&1 | grep -E "^warning:" | wc -l`.

**Per-pass loop.** Follow the exact Agent(...) call sequence in template §Pass Loop. Do NOT improvise subagent types, prompt content, or ordering — the template is the contract. When it says "invoke rust-diagnostician with prompt from §T1", pass the §T1 template verbatim with only the marked {placeholders} substituted.

**Escape hatches.** Template §Escape Hatches enumerates the else-branches for common failure modes (target-case-red after 5 iterations, unexpected corpus regression, warning-budget breach, unresolved architectural gap). Follow those, not improvisation.

**Terminate when:** 324/324; no regressions; INV1..INV10 active + green; Quality Gate green; every applicable checklist item present; zero DEFER items outstanding.

**On termination:** update `v2_progress.md`; add a dev journal entry (`docs/dev_journal/YYYY-MM-DD-corpus-100-percent.md`); extend `docs/dev-journal-toc.md`. **Do NOT commit without user approval.**
```

---

## §Pass Loop — exact Agent(...) call sequence

Each pass follows steps 1–7 in order. Do not skip, reorder, or add steps
without an escape-hatch trigger firing (§Escape Hatches).

### Step 1 — Pick

Refresh failure clustering:

```bash
cd /workspace/tests/integration && source venv/bin/activate && \
  python3 -m pytest differential/test_dataframe_corpus_differential.py --tb=short 2>&1 \
  | grep -oE "τ: [^\"]+|DuckDB error: [^\"]+|Binder Error: [^\"]+|Catalog Error: [^\"]+|Parser Error: [^\"]+" \
  | sort | uniq -c | sort -rn | head -20 > .agent-output/pass-{PASS_N}-clusters.txt
```

Pick ONE case (or ≤3 same-shape variants) — highest cascade wins, tie
broken by fixture simplicity.

**Persist:** append to `.agent-output/pass-{PASS_N}-pick.md`:
- `PASS_N` (integer).
- `TARGET_CASE_IDS` (comma-separated).
- `BASELINE_CORPUS` (from most recent `v2-progress.sh`).
- `BASELINE_WARNINGS` (release build).
- `PIPELINE_START_SHA` (unchanged across passes; the goal-baseline SHA).

`TaskCreate` a pass tracker.

### Step 2 — Diagnose (Agent call site D)

Dispatch:

```
Agent(
  subagent_type: "rust-diagnostician",
  description: "Diagnose {TARGET_CASE_IDS} pass {PASS_N}",
  prompt: <<T1 with substitutions>>
)
```

**Template T1 — rust-diagnostician prompt (pass verbatim, only substitute
`{...}` placeholders):**

```
Diagnose target corpus case(s) `{TARGET_CASE_IDS}` in the Thunderduck v2 transpiler.

**Test cases** (source at `tests/integration/differential/dataframe_corpus.py`):
{PASTE_CASE_LAMBDAS}

**Observed failure symptoms:**
{PASTE_PYTEST_ERROR_LINES}

**Task.** Multi-hypothesis, systematic root-cause analysis. Instrument if needed (tracing::debug! is IN SCOPE for this pass; rebuild release binary if you add tracing).

**Hypotheses to rank & falsify:**
- H1..HN (enumerate based on the failure symptom family).

**Layer-boundary snapshots to capture:**
- Proto → CommonAst (v2_relation_converter / parser_v2).
- CommonAst → TypedAst (analyzer).
- TypedAst → SQL (emission::dispatch_op).
- SQL → DuckDB result (or DuckDB error).

**Reproduction:**
cd /workspace/tests/integration && source venv/bin/activate && \
  python3 -m pytest "differential/test_dataframe_corpus_differential.py::test_case[{FIRST_CASE_ID}]" -v --tb=long

**Deliverable — WRITE TO** `/workspace/.agent-output/diagnostic-pass-{PASS_N}.md` (under 500 words):
1. Ranked hypotheses with evidence for/against each.
2. Confirmed root cause with file:line references.
3. ADR-022 category (Spark-emulated / Thunderduck-boundary / runtime-correctness).
4. Suggested owning layer (converter / analyzer / type-inference / emission / runtime).
5. Layer-boundary snapshots you captured.

Do NOT fix the bug. Diagnose only. Revert any purely-diagnostic instrumentation.

{BOILERPLATE — see §Boilerplate}
```

**Verdict handling:**
- Read `.agent-output/diagnostic-pass-{PASS_N}.md`.
- If diagnostic identifies a root cause with a named owning layer → proceed to Step 3.
- If diagnostic says "cannot reproduce" or "multiple root causes" → HALT-AND-FLAG-1 (re-pick a tighter cluster).

### Step 3 — Architect (Agent call site A)

Dispatch:

```
Agent(
  subagent_type: "rust-architect",
  description: "Architect fix for pass {PASS_N}",
  prompt: <<T2 with substitutions>>
)
```

**Template T2 — rust-architect prompt (verbatim):**

```
Architect the smallest correct fix for `{TARGET_CASE_IDS}` per the diagnostic at `/workspace/.agent-output/diagnostic-pass-{PASS_N}.md`.

**Constraints:**
- Cite applicable ADRs from `docs/thunderduck-rearchitect-ADRs.md` by identifier.
- Cite applicable sections of `tasks/v2-restart-inheritance-checklist.md`.
- Consult `tasks/v2-restart-open-decisions.md`; if required behavior contradicts a resolved decision → note it as HALT-AND-FLAG-3 and stop.
- Enumerate every file:line change.
- Every arm added must have either a corpus witness (the target case OR a cascade case) OR a unit test that exercises it. Reject partial-arm shortcuts.
- Owning layer must be named: converter / analyzer / type-inference / emission / runtime.

**Deliverable — WRITE TO** `/workspace/.agent-output/architecture-pass-{PASS_N}.md`:
- Domain constraints (Spark's exact behavior for this input).
- Lifecycle: proto → CommonAst → TypedAst → SQL.
- Module/function layout: which functions change, with file:line.
- Type-inference or nullability changes (if any).
- Error strategy: Spark-emulated vs Thunderduck-boundary (per ADR-022).
- Open questions.

Return a one-paragraph summary of key decisions.

{BOILERPLATE}
```

**Verdict handling:**
- Architect returns → proceed to Step 4.
- Architect reports HALT-AND-FLAG-3 (unresolved architectural gap) → stop the pass, append Decision N+ to `tasks/v2-restart-open-decisions.md`, HALT for user resolution.

### Step 4 — Implement (Agent call site I)

Dispatch:

```
Agent(
  subagent_type: "rust-coder",
  description: "Implement pass {PASS_N}",
  prompt: <<T3 with substitutions>>
)
```

**Template T3 — rust-coder prompt (verbatim):**

```
Implement the fix in `/workspace/.agent-output/architecture-pass-{PASS_N}.md`.

**Read that plan first.** Implement exactly what it specifies. If you find yourself deviating, document the deviation in the implementation log and cite the reason.

**Constraints:**
- INV3/INV10 respected — no legacy imports.
- No `unwrap()` in production paths.
- No `#[allow(...)]` to silence warnings.
- No dead-code arms.
- No silent NULL fallbacks (CLAUDE.md §Known Gotchas #9).

**Quality Gate** (per CLAUDE.md §Quality Gate — read that section):
- cargo check both crates.
- rustfmt --check on touched files.
- cargo test -p thunderduck-core --lib --tests.
- cargo test -p thunderduck-connect-server --tests.

**Corpus verification:**
cd /workspace && cargo build --release -p thunderduck-connect-server
cd /workspace/tests/integration && source venv/bin/activate && \
  python3 -m pytest {TARGET_CASE_PYTEST_ARGS} -v
cd /workspace && ./tests/scripts/v2-progress.sh

**Requirements:**
- Target case(s) GREEN.
- Full corpus ≥ {BASELINE_CORPUS} (monotone non-regress).
- Warning count ≤ {BASELINE_WARNINGS} on touched files (no new warnings).

**Deliverable — WRITE TO** `/workspace/.agent-output/implementation-pass-{PASS_N}.md`:
- Files created/modified with one-line descriptions.
- Tests added.
- Deviations from the plan (with reason).
- Full Quality Gate output per step.
- Target-case + corpus + warning-count deltas.

Return: files-changed count, tests-added count, corpus count, gate status.

DO NOT commit.

{BOILERPLATE}
```

**Verdict handling:**
- Target case green + corpus ≥ baseline + warnings ≤ baseline → proceed to Step 5.
- Target case red → increment `FIX_ITERATION`. If < 5, return to this step with the current findings. If = 5, HALT-AND-FLAG-1: re-diagnose (Step 2 again with fresh instrumentation).
- Corpus regressed → HALT immediately, report regression list to user, do NOT proceed.
- Warnings exceeded baseline → require in-pass fix before proceeding.

### Step 5 — Review + Perf (parallel Agent call sites R + P)

Dispatch BOTH in a single message with two `Agent` tool calls:

```
Agent(
  subagent_type: "rust-reviewer",
  description: "Review pass {PASS_N}",
  prompt: <<T4 with substitutions>>
)
Agent(
  subagent_type: "rust-perf",
  description: "Perf review pass {PASS_N}",
  prompt: <<T5 with substitutions>>
)
```

**Template T4 — rust-reviewer prompt (verbatim):**

```
Review the implementation for pass {PASS_N}.

**Context files:**
- Diagnostic: `/workspace/.agent-output/diagnostic-pass-{PASS_N}.md`.
- Architecture plan: `/workspace/.agent-output/architecture-pass-{PASS_N}.md`.
- Implementation log: `/workspace/.agent-output/implementation-pass-{PASS_N}.md`.

**Diff to review:**
git -C /workspace diff {PIPELINE_START_SHA}..HEAD
git -C /workspace diff  # uncommitted

**Focus areas (project-specific):**
- INV3 (no crate::generator / crate::functions imports).
- INV10 (τ files import only crate::types + intra-τ).
- ADR-013 (typed AST as interchange).
- ADR-015 (Spark parity — cite the exact Catalyst rule for any semantic decision).
- ADR-022 (τ-only path; boundary errors are Unsupported*, never corrupt output).
- CLAUDE.md §Known Gotchas #9 (no silent NULL fallbacks / catch-all Ok in typed dispatch).
- No unwrap() in prod, no #[allow(...)] silencing warnings.
- Every new arm has a corpus witness or unit test.

**Deliverable — WRITE TO** `/workspace/.agent-output/review-pass-{PASS_N}.md`:
- Findings by severity (Critical / High / Medium / Low) with file:line + ADR/invariant cited.
- Verdict: APPROVED or NEEDS_CHANGES.
- Critical + High count.

{BOILERPLATE}
```

**Template T5 — rust-perf prompt (verbatim):**

```
Perf review the implementation for pass {PASS_N}.

**Files touched** (list from implementation log):
{FILES_CHANGED}

**Context:** intra-plan analysis + emission is a cold path relative to per-row data plane. Focus on:
- Allocation profile (Vec growth, format! usage, String concatenation in loops).
- Redundant walks (double schema traversal, per-arg re-rendering).
- Recursion depth on nested structures (Θ(2^D) risk).
- Any obvious O(n²) that becomes O(n) with a HashMap.

**Deliverable — WRITE TO** `/workspace/.agent-output/perf-pass-{PASS_N}.md`:
- HIGH / MEDIUM / LOW / INFO findings with bottleneck, hypothesis, proposed change, verification, risk.
- Verdict: OPTIMIZED or HAS_OPPORTUNITIES.
- HIGH + MEDIUM count.

{BOILERPLATE}
```

### Step 6 — Close findings (Agent call site F)

**If reviewer verdict = APPROVED (0 Critical + 0 High) AND perf verdict = OPTIMIZED (0 HIGH + 0 MEDIUM):**
- Proceed to Step 7. No fix subagent needed.

**Else (any Critical/High/Medium finding exists):**

```
Agent(
  subagent_type: "rust-coder",
  description: "Close findings pass {PASS_N} (iteration {FIX_ITER})",
  prompt: <<T6 with substitutions>>
)
```

**Template T6 — rust-coder findings-fix prompt (verbatim):**

```
Close review + perf findings for pass {PASS_N} in-pass (ZERO-DEFER discipline).

**Findings files:**
- `/workspace/.agent-output/review-pass-{PASS_N}.md`
- `/workspace/.agent-output/perf-pass-{PASS_N}.md`

**Categorize each finding:**
- Fix in-pass if it's in code this pass touched OR is a Critical/High blocker regardless of location.
- Route to follow-up pass if it truly touches unrelated code (pre-existing systemic pattern). Append the follow-up entry to `tasks/v2-corpus-driven-pass-log.md` under this pass's log entry as "Findings queued as follow-up pass".
- REJECT any classification suggesting "future slice" or "housekeeping" — this is corpus-driven, not slice-driven.

**Verification (same as Step 4):**
- Quality Gate green.
- Target case still green.
- Corpus still ≥ {BASELINE_CORPUS}.
- Warnings ≤ {BASELINE_WARNINGS}.

**Deliverable — APPEND TO** `/workspace/.agent-output/implementation-pass-{PASS_N}.md` under `## Review + Perf Fix Iteration {FIX_ITER}`:
- Per-finding: what was done (fixed in-pass / routed to follow-up).
- Quality Gate output.
- Corpus + warning-count deltas.

Return: findings closed, findings routed, gate status.

DO NOT commit.

{BOILERPLATE}
```

**Verdict handling:**
- All Critical + High closed and Medium routed → proceed to Step 7.
- Critical/High remain and `FIX_ITER < 3` → increment `FIX_ITER`, re-dispatch review (Step 5).
- Critical/High remain and `FIX_ITER = 3` → HALT-AND-FLAG-1 (diagnostic was wrong; re-diagnose Step 2).

### Step 7 — Log + close

Append a pass-log entry to `tasks/v2-corpus-driven-pass-log.md` per
methodology §Pass log. Update TaskUpdate to mark pass complete. Proceed to
Step 1 for the next pass (unless termination condition met — see below).

---

## §Boilerplate — appended verbatim to every subagent prompt

Every T1..T6 prompt template ends with the marker `{BOILERPLATE}` — the
parent substitutes this literal block:

```
**Boilerplate constraints (goal-level, apply to every subagent):**
- Do NOT commit or amend git history. User approves commits explicitly.
- Do NOT introduce new compiler warnings on files this pass touches.
- Do NOT use #[allow(...)] to silence warnings when the underlying fix is trivial.
- Do NOT create planning/decision/summary .md files unless the task deliverable explicitly names one.
- Cite ADR identifiers (ADR-000..ADR-022) for every architectural claim.
- If you cannot complete the task in one turn, return a partial-result summary and STOP; do NOT continue past your time budget.
- If a system reminder tells you not to write a specific .md file, respect it — return findings inline instead, and note this to the parent.
```

---

## §Escape Hatches — explicit else-branches

**H1 — Target case red after 5 fix iterations (Step 4 or Step 6).**
Signal: `FIX_ITER = 5` with target case still red.
Action: Increment `PASS_N.retry`, return to Step 2 with a fresh diagnostic
dispatch. Add a note in the pass log: "Retry iteration; original diagnostic
hypothesis was wrong."

**H2 — Unexpected corpus regression (Step 4).**
Signal: any prior-green case flips red after implementation.
Action: HALT immediately. Do NOT run Step 5. Report regression list to
user. Wait for direction.

**H3 — Warning budget breach (Step 4 or Step 6).**
Signal: warning count on touched files > `BASELINE_WARNINGS`.
Action: Re-dispatch the current coder with a targeted prompt asking to
fix ONLY the new warnings. Do not advance until the count is at or below
baseline.

**H4 — Unresolved architectural gap (Step 3).**
Signal: architect returns HALT-AND-FLAG-3 or the diagnostic surfaces a
question not answerable by ADRs 000–022 or `v2-restart-open-decisions.md`.
Action: Append Decision N+ to `tasks/v2-restart-open-decisions.md` with
the question and observed options. HALT the pass. Wait for user to
resolve the decision before dispatching further work.

**H5 — Upstream substrate missing (Step 4).**
Signal: fix lands correctly at the named layer but target stays red
because a prior pass should have supplied enabling substrate (e.g., an
analyzer arm the emission arm expects).
Action: Reopen the prior pass by extending the current pass's scope to
include the missing substrate. Note this in the pass log as "Scope
extension: subsuming Pass K's incomplete substrate."

**H6 — Subagent API error / timeout.**
Signal: `Agent(...)` returns an API error or hangs beyond a reasonable
window.
Action: Retry once with a tighter-scoped prompt (halve the case set,
narrow the file list). If it fails again, HALT and report to user.

**H7 — Tool refused by user permission prompt.**
Signal: user denies a specific tool invocation from a subagent.
Action: Do NOT re-invoke the same tool. Adjust the subagent's prompt to
accomplish the goal via allowed tools, or HALT if no path exists.

---

## §Handoff Files — durable state across subagents

Every pass produces the following files under `.agent-output/`. Downstream
subagents MUST be told to `Read` the relevant files by path (they don't
see the parent's conversation).

| File | Written by (Step) | Read by (Step) |
|------|-------------------|----------------|
| `pass-{N}-clusters.txt` | Parent (1) | Parent (1, subsequent passes) |
| `pass-{N}-pick.md` | Parent (1) | All downstream steps |
| `diagnostic-pass-{N}.md` | rust-diagnostician (2) | Architect (3), Reviewer (5) |
| `architecture-pass-{N}.md` | rust-architect (3) | Coder (4), Reviewer (5) |
| `implementation-pass-{N}.md` | rust-coder (4, 6) | Reviewer (5), Perf (5), later passes |
| `review-pass-{N}.md` | rust-reviewer (5) | Findings-fix coder (6) |
| `perf-pass-{N}.md` | rust-perf (5) | Findings-fix coder (6) |

**Naming rule:** always `-pass-{N}` suffix so multiple passes coexist. The
generic `.agent-output/00X-*.md` scheme (from prior sessions) collides
across passes; v3 abandons it.

**Retention:** keep all pass files indefinitely under `.agent-output/`.
Do not delete or archive during the climb — the pass log references them.

---

## §Termination

- 324/324 corpus green.
- No regressions.
- All INV1..INV10 active + green.
- Quality Gate green including no new warnings on any touched file.
- Every applicable inheritance-checklist item present.
- Zero `DEFER` items outstanding.
- `tasks/v2-corpus-driven-pass-log.md` has an entry for every executed pass.

**On termination:**
1. Run `./tests/scripts/v2-progress.sh` one final time; update `tests/integration/v2_progress.md`.
2. Write dev journal entry: `docs/dev_journal/YYYY-MM-DD-corpus-100-percent.md`.
3. Extend `docs/dev-journal-toc.md`.
4. Do NOT commit. Present summary to user for commit approval.

---

## Iteration notes

- **v3 introduces prescriptive orchestration.** v2 was aspirational; v3
  is directive. The parent no longer chooses subagent types or writes
  fresh prompts — it substitutes placeholders into named templates.
- **First test of v3:** whichever corpus case cluster follows commit
  `8d232df` (baseline 294/324, 30 remaining).
- **The v2 template stays** in the repo for reference; v3 supersedes for
  new sessions.
