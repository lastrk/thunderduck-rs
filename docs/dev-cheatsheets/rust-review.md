# Rust Review Cheatsheet

Portable review discipline for Rust changes. Read-only. Review what is
present; do not propose speculative rewrites.

## Review categories

Work through in this order per change:

1. **Correctness** — does it do what the spec/design says? Look for
   off-by-one, wrong branch, missing case, silently-swallowed error,
   race, wrong lifetime. Read the tests: do they lock the intended
   behavior, or just the implemented one?
2. **Safety** — `unsafe` blocks (must have soundness argument in
   comment), invariants around raw pointers, aliasing, uninitialized
   memory, `Send`/`Sync` impls, panic safety around `unwrap_unchecked`.
3. **Idiomatic style** — see `rust-implementation.md`; flag deviations.
4. **Security** — hardcoded secrets, PII in logs, SQL formatted from
   user input, path traversal, unsanitized `Command` args.
5. **Maintainability** — misleading names, dead code, over-abstracted
   generics, comments contradicting code.

## Findings format

Each finding follows this shape:

```
### [SEVERITY] <one-line title>
Location:  file:line
Issue:     <what is wrong or risky>
Why:       <invariant, spec, or ADR violated>
Fix:       <minimal correction>
```

Severity ladder:
- **Critical** — will corrupt data, crash in prod, or leak
  credentials. Must block merge.
- **High** — clear correctness or safety bug likely to hit prod, or
  contract violation. Should block merge.
- **Medium** — real defect but bounded impact / narrow input surface.
  Nice to fix.
- **Low** — style / consistency / minor readability. Non-blocking.

## What to flag

- `.unwrap()` in library code.
- `.expect()` without an invariant argument.
- Silent `_ => Ok(...)` in a typed dispatch match.
- New `unsafe` without a soundness comment.
- Public API changes without doc updates.
- `pub` on items that should be `pub(crate)` or private.
- Missing tests for new public functions or bug fixes.
- Panics in request handlers or hot paths.
- Trait impls that violate the trait's invariants (e.g., `Eq` without
  reflexivity/symmetry/transitivity across all fields; `Hash`
  inconsistent with `PartialEq`).
- Locks held across `.await`.
- `!Send` types crossing an `.await` in an async fn.
- New warnings that could be fixed rather than `#[allow(...)]`-silenced.
- Comments that no longer match the code.
- Doc comments missing on new public items.

## What NOT to flag

- Personal-taste refactors ("I would have named this differently").
- Speculative "future flexibility" abstractions the author correctly
  did NOT add.
- Working code the author did not touch.
- Formatting fixable by `cargo fmt` (assume CI runs it).
- Things the tests already prove correct — unless the tests are wrong.
- Improvements that would take the review beyond a review (rewrites
  belong in a follow-up plan/coder task).

## Signal, not noise

- Explain WHY, cite the invariant/spec/ADR/idiom the finding rests on.
- Show the minimal fix, not a redesign.
- Do NOT rewrite entire functions in review comments.
- If everything is fine, say so. APPROVED with zero findings is a
  legitimate outcome.

## Verify before flagging

- Don't have the symbol yet, only the behavior? Use `rg` to locate the area.
- Use `scip-nav refs --count` before flagging "unused" or proposing an API change.
- Use `scip-nav def` and `refs` to verify a function's definition and dependents.
- Read at least the caller and the callee of any function you flag.

## Report shape

```markdown
# Review — <change identifier>

## Summary
<one paragraph: what changed, one-sentence verdict>

## Critical
<findings, or "none">

## High
<findings, or "none">

## Medium
<findings, or "none">

## Low
<findings, or "none">

## What's done well
<optional: 1–3 items worth naming so signal-noise stays healthy>

## Verdict
APPROVED  |  NEEDS_CHANGES  (Critical + High count: <N>)
```

## Cross-cutting invariants (project-scoped; check project docs)

Every project pins a set of durable invariants (naming discipline, no
cross-module imports, no legacy layer touches, ANSI-mode target, etc.).
Read the project's top-level guidance (CLAUDE.md, ADRs, contributor
docs) before starting; a project invariant supersedes any generic
idiom in this cheatsheet.
