# Code Search Tools

> Four tools answer code-navigation questions on this repo. Pick by the rules
> below — they are empirically validated on thunderduck-rs (21-op bake-off,
> 2026-07-10), not guesses. Prefer them over ad-hoc file reading.

The four:

- **codegraph** — `mcp__codegraph__codegraph_explore { query, projectPath }`. Tree-sitter
  symbol/edge graph in SQLite. Returns verbatim source + call paths + a
  dependent-**site** blast radius in one (verbose, ~2–6.5k tok) call.
- **semble** — `mcp__semble__search { query, repo }` / `mcp__semble__find_related { repo,
  file_path, line }`. Hybrid semantic+lexical chunk search. Cheap (~0.5–0.9k tok);
  best when you have *intent* but no symbol name. Pass `repo` = project root
  (`/workspace`) or it errors.
- **scip-nav** — `Bash: python3 .claude/skills/scip-nav/scip_query.py <refs|def|sym> <NAME>
  [--count]`. Type/trait-resolved refs, exact defs, symbol search from a static
  rust-analyzer SCIP snapshot. **Tiny** (5–160 tok), exact. The current Git worktree
  selects an immutable content-keyed snapshot in the shared cache; missing exact
  snapshots fail closed (`refresh`, or `--stale-ok`/`--auto-refresh`). Also has a
  heavyweight **`expand <crate> [pat]`** mode (nightly `rustc -Zunpretty=expanded`)
  that shows **macro-generated code** — compile-bound, crate-scoped, needs a green
  tree; for exploration/review, not tight loops. See the `scip-nav` skill.
- **rg** — `Bash: rg`. The lexical baseline. Cheapest (40–60 tok) and *correct*
  for text, string literals, imports, and macro call-sites. Don't overlook it.
  Note: `rg` here is a **Claude Code shell shim** (ripgrep 14.1.1), not a `$PATH`
  binary — it works in agent/Bash-tool calls but a raw subprocess that doesn't load
  the Claude shell won't have it (there is no standalone `rg`; the Grep *tool* is
  also absent on native builds). Scripts should not assume a bare `rg` binary.

## The 5 rules (cover ~everything)

1. **Know the symbol name → `scip-nav` (`def`/`refs`/`sym`).** Exact, trait/type-resolved,
   5–160 tok. Default for go-to-def, callers, rename, dead-code, cross-crate endpoints.
2. **Concept / no name yet → `semble` first, then `scip-nav` to pin exact lines.**
   The cheap discovery→resolution combo.
3. **Text / string literal / imports / macro call-sites → `rg`.** Exact and cheapest;
   beats the fancy tools when the target is lexical.
4. **Dependency *graph* (blast-radius, call-path, "what tests cover this", subsystem
   survey) → `codegraph`.** The only tool returning dependent *sites* + call flow +
   test-caller flags. Worth its ~3–4k tok *only here*.
5. **Never trust codegraph for a caller *count*** (undercounts trait-dispatch: 26 vs
   true 44 for `require_proto`) **nor `rg`** (overcounts defs/docs: 48 vs 44). Use
   `scip-nav refs --count`.

## Per-op prescription (winner · winner tokens · best alternative)

| Op | ✅ Use | Tok | Best alternative (tok · why worse) |
|---|---|---|---|
| Concept→location | `semble` → `scip def` | 800 | semble alone 650 (misses a leg); codegraph 4000 (0/3) |
| Exact symbol by name | `rg 'pub (struct\|enum\|fn) X'` | 60 | scip def 3800 (exact, floods members) |
| String / error-literal | `rg` | 40 | semble 460 (literal escapes embedding); codegraph 2500 (wrong) |
| Subsystem survey | `codegraph explore` | 3000 | semble 875 (chunks, no graph — good pre-step) |
| Go-to-definition | `scip def` | 90 | codegraph 4600 (50×); semble 390 (misses `Option<T>` impl) |
| All trait impls | `scip def` | 130 | codegraph 6000 (45×); rg 120 (misses aliased/generic) |
| Type API surface | `scip sym` | 1340 | codegraph 4600 (dup noise). *count = struct+fields+methods, not method tally* |
| Type-at-point / return type | `rg 'fn X'` → `scip def` | 450 | codegraph 2000 (4–6×); scip alone **can't** (no hover) |
| All callers / references | `scip refs [--count]` | 85 | rg 1813 (→52, over); codegraph 2100 (→26, under) |
| Blast-radius | `codegraph explore` | 3000 | scip/rg give *occurrences* (780/815), wrong unit |
| Call-path between symbols | `codegraph explore (flow)` | 3800 | semble 650 (names bridge, no ordered chain) |
| Cross-crate flow endpoints | `scip def + sym` | 160 | codegraph 3600 (accurate, 23×, buries endpoints) |
| Enum-variant handler | `scip sym` → `scip refs` | 150 | codegraph 6000 (40×); semble 470 (near-miss) |
| Macro call-site enumeration | `rg -c 'macro!'` | 40 | scip **wrong** (macros aren't SCIP refs) |
| Macro-generated body (definition) | `semble` → `Read range` | 710 | scip sym→Read 285 (if macro name known); codegraph trims body |
| Macro-EXPANDED code (what it generates) | `scip-nav expand <crate> <pat>` | compile-bound | **only tool that can** — codegraph/semble/rg/SCIP see source, not expansion; live LSP `expandMacro` is the only surgical alt |
| Async/channel hop | `semble` → `scip refs` | 570 | codegraph 2300 (most complete, 4×); scip alone = recv side only |
| Dead / narrowly-used? | `scip refs --count` | 5 | rg 48 (over); codegraph 26 (under by 18) |
| Rename safety (all sites) | `scip refs + def` | 700 | rg 1800 (52, no trait awareness); codegraph 6500 (26) |
| Test coverage of a symbol | `codegraph explore` | 3200 | scip refs 130 (Rust-level 0). **Neither sees the Python corpus** — pair w/ corpus grep |
| Invariant / banned imports | `rg` (anchored `use`) | 50 | scip 10 (not import-scoped, 0 acc); codegraph 4500 (90×) |
| Find duplication / siblings | `scip sym <prefix>` | 2800 | semble `find_related` **wrong** (cross-file, misses in-file siblings) |

## Token economics

- **scip-nav 5–160 tok** — the workhorse; `--count` ≈ free. Wins/co-wins ~9 ops.
- **rg 40–60 tok** — outright wins 4 ops (lexical targets); often the cheapest *correct* answer.
- **semble 460–875 tok** — the discovery front-end; rarely the final answer, often the right first step.
- **codegraph 2000–6500 tok** — wins exactly 4 ops (blast-radius, call-path, survey, test-coverage);
  elsewhere 20–90× the winner's cost for equal-or-worse accuracy.

## Caveats worth remembering

- **codegraph undercounts trait dispatch** through `Option<T>`/generics and **rg overcounts**
  (defs + doc-comments). For any *count* that gates a decision (dead code, rename, narrow-use),
  use `scip-nav refs --count`.
- **SCIP has no hover types**; macro call-sites aren't first-class SCIP refs (use `rg`).
  For a macro's *definition* read the source; for the *generated code* use `scip-nav
  expand` (nightly, compile-bound, green tree). Hover/type-at-point still needs live LSP.
- **Test coverage in this repo lives in the Python differential corpus**
  (`tests/integration/differential/*`), invisible to all four Rust tools — a Rust "no covering
  tests" flag is not the whole story; grep the corpus too.
- **scip-nav is a snapshot**: run `refresh` (~15 s, ~3 GB transient, then freed) after edits, or
  a query will fail closed. `status` reports the discovered worktree, shared cache,
  fingerprint, and exact snapshot. `--stale-ok` is scoped to the same worktree.
