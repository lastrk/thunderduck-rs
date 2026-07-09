# /goal — implement the select-block review findings (witness-driven)

> Invoke with Fable 5 as the main-loop model. The main loop is BOTH
> orchestrator and architect: it explores, designs, and writes plans itself;
> it delegates only coding (Sonnet 5) and reviewing (independent subagent).

## Goal

Drive `tasks/select-block-review-findings.md` to closure on branch
`feat/select-block-emission` (worktree
`/workspace/.claude/worktrees/select-block-emission`): every CONFIRMED
correctness finding fixed or explicitly deferred with written rationale in
the report, every born-red witness case flipped green, zero regressions on
the rest of the corpus, quality gates green after every cycle.

**Success is measured by the progress script, nothing else.** Aggregate
pass counts and vibes do not count; per-case evidence does.

## Cycle 0 — build the measurement first

Before touching any emission/analyzer code, create
`tests/scripts/witness-progress.sh` (plus, if needed, a small Python helper
under `tests/integration/utils/`). Contract:

1. **Manifest** — check in `tests/integration/select_block_witness_manifest.json`:
   the witness case ids and the finding each pins:
   `join-018`→F1, `join-020`→F2, `cx-015`/`cx-016`→F3, `join-019`/`jn-023`→F4,
   `join-021`→F5, `join-022`→F7. Cycles that add new witnesses (F9, F10, …)
   MUST extend the manifest in the same commit.
2. **Baseline** — check in the current per-case PASS set once (cycle 0),
   generated from a fresh `run-differential-tests.sh core` + `sql_v2` run
   (store sorted `file::case_id` lines, e.g.
   `tests/integration/select_block_corpus_baseline.txt`). Regenerating the
   baseline later is allowed ONLY in a commit whose message says why.
3. **Invocation** — the script runs both corpora (reuse
   `run-differential-tests.sh`; export
   `SPARK_HOME=/workspace/.spark/spark-4.1.1
   THUNDERDUCK_VENV_DIR=/workspace/.venv` first), parses the pytest `-v`
   per-case lines, and reports:
   - `REGRESSIONS: <n>` — baseline-PASS cases now non-PASS, each listed.
   - `WITNESS FLIPS: <k>/<total>` — manifest cases now PASS, each listed
     with its finding id.
   - Exit 0 iff `REGRESSIONS == 0`; exit 1 otherwise. (Witness flips are
     progress, not a gate — un-flipped witnesses never fail the script.)
4. Validate the script by running it unchanged at cycle 0: it must report
   0 regressions and 0/8 flips. Commit script + manifest + baseline as the
   cycle-0 commit.

## Iteration protocol (repeat until stop condition)

Each cycle takes ONE finding cluster, in this order (re-derive from the
report if it has moved on):

1. **F1+F2+F3+F4 — the USING/default_projections cluster.** One
   architectural fix, not four patches: the hoisted slot list must survive
   every path that consumes or rebuilds a block (`set_projections`
   shadowing, `into_pure_from` re-wrap, `extend_from`). Prefer making the
   invariant structural (e.g. slots derived from the analyzer's
   resolved_schema at the consumption point, or wrap-paths that re-derive
   defaults) over per-call-site patching — per-site patches are how
   findings 1-4 became four separate holes.
2. **F5 — USING vis-exemption strand** (merge-visibility must not treat a
   buried-under-synthetic alias as exempt).
3. **F7 — duplicate `__td_jr` collision** (collision fallback needs a
   distinct alias, or refuse the inline).
4. **F9+F10 — strand-class completion** (aggregate/lateral wrap strip;
   merge-path leak). WITNESS FIRST: add the red corpus cases (mirror
   filt-016/017), extend the manifest, THEN fix.
5. **F8+F11+F12 — Spark-parity error semantics.** Each needs a decision
   recorded in the report before code: what Spark actually does (verify
   against Spark 4.1.1 source/behavior, ADR-016), then `expected_error`
   witnesses, then the fix (F8's root cause is the analyzer tier-(f)
   fallback, not the strip; F12 needs a clean boundary error for stranded
   `q.*`; F11 needs qualified-ref ambiguity checking).
6. **F13 — debug_assert on untrusted input** (downgrade to a boundary error
   or neutralize the demand; unit-test pinned).
7. **F14+F15 + deferred-cleanup list — hygiene cycle(s)**, only after all
   correctness findings are closed.

Within a cycle:

- **A. Architect (main loop, Fable 5).** Read the finding's evidence in the
  report, read the current code, design the fix, and write a DETAILED
  implementation plan into `.agent-output/NNN-plan-<finding>.md`: exact
  files/functions, the mechanism of the fix, invariants that must hold
  (ADR-022 single path; Spark parity over DuckDB ergonomics; analyzer owns
  resolution, emission owns scoping), tests to add/re-baseline, and the
  acceptance criteria (which manifest witnesses must flip, which unit tests
  pin the mechanism). The plan must be executable without further design
  decisions.
- **B. Code (Sonnet 5 subagent).** Spawn `rust-coder` with `model: sonnet`.
  Give it the plan file, the repo rules pointer (CLAUDE.md +
  docs/dev-cheatsheets/rust-implementation.md), and the constraint set
  below. It implements, adds/updates unit tests, runs
  `cargo check -p thunderduck-core`, scoped `rustfmt --check`, and
  `cargo test -p thunderduck-core --lib`, and writes its change log to
  `.agent-output/NNN-implementation-<finding>.md`. It does NOT run the
  corpora (orchestrator's job) and does NOT commit.
- **C. Review (independent subagent).** Spawn `rust-reviewer` (read-only —
  must not be the coder; default model). Give it the plan, the diff
  (`git diff`), and the finding's failure scenarios. It verifies: the
  mechanism actually closes the failure scenario (not just the witness),
  no new latent holes of the same class, style/invariants (no `unwrap`,
  doc comments, ADR conformance). Findings with severity to
  `.agent-output/NNN-review-<finding>.md`.
- **D. Fix loop.** Critical/High review findings go back to a fresh
  `rust-coder` (Sonnet 5) with the review text. Max 3 iterations; if still
  red, STOP the cycle, revert to the last green commit, and record the
  blocker in the report before re-planning.
- **E. Quality gate (concludes the cycle — all must pass, in order):**
  1. `cargo fmt --check` (scoped to changed files),
  2. no NEW clippy warnings on touched files,
  3. `cargo test -p thunderduck-core` (all lib tests; also
     `-p thunderduck-connect-server --bins` when the wire path is touched),
  4. `tests/scripts/witness-progress.sh` — **exit 0 (zero regressions)**,
     with the cycle's target witnesses now listed as flipped.
- **F. Bookkeeping + commit.** Mark the finding FIXED in
  `tasks/select-block-review-findings.md` (with the mechanism, one line).
  Commit code + tests + report note together (message: what/why/gates, end
  with the Claude co-author line). NEVER push. NEVER merge.
- **G. Ledger.** After the FINAL cycle only: run
  `tests/scripts/differential-progress.sh` and commit the ledger row.

## Hard constraints (every cycle, every subagent)

- Work ONLY in `/workspace/.claude/worktrees/select-block-emission`.
- `export SPARK_HOME=/workspace/.spark/spark-4.1.1
  THUNDERDUCK_VENV_DIR=/workspace/.venv` before any differential run; never
  re-run setup-differential-testing.sh.
- Never `pkill` servers — `./tests/scripts/kill-test-servers.sh` only.
- Zero regressions is a HARD gate: one baseline-PASS case going red fails
  the cycle regardless of how many witnesses flipped.
- Witness-first discipline (ADR-001/ADR-015): no fix for an un-witnessed
  failure scenario; add the red case, extend the manifest, then fix.
- ADR-022: no fallback paths, no dual rendering; τ's single pipeline only.
- Spark parity beats DuckDB ergonomics (ADR-015/016); consult Spark 4.1.1
  source via WebFetch when behavior is in question and record the citation
  in the plan.
- Emission builds SQL from the typed AST only; no string post-processing.
- The EMIT_TAP/INV suite must stay green (`cargo test -p thunderduck-core
  -- invariants`).

## Stop condition

Stop (success) when: manifest witnesses all green (8/8 plus any added
during F9/F10/F8/F11/F12 cycles), `witness-progress.sh` exits 0, findings
1-13 each marked FIXED or DEFERRED-with-rationale in the report, hygiene
cycle done or explicitly deferred, final ledger row committed. Stop
(blocked) when a cycle fails its fix loop twice in a row on the same
finding — leave the tree green (revert), write the blocker analysis into
the report, and end with a summary of what remains.
