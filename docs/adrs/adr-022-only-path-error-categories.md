# ADR-022 — τ is the only path; three error categories

**Status:** Proposed. *Amended 2026-08-07 (§ Amendment 1) to admit a third
category: named strict rejections of malformed SQL that Spark's permissive
grammar accepts. The amendment narrows category 1 and adds a register; the
two original categories are otherwise unchanged.*
**Depends on:** ADR-000 (no-JVM premise), ADR-002 (delegation boundary), ADR-021 (substrate ownership)
**Depended on by:** every implementation slice; ADR-024 and ADR-026 (reference-resolution Spark-emulated errors); ADR-025 (lossless interval types)

**Context.** τ is the transpiler. When τ does not implement a Spark input, τ says so — it produces a typed error to the caller, not a partial or synthetic SQL string, and not a route to some other execution path. The correctness contract is Spark parity, a named limitation, or a **named and registered** divergence; what is forbidden is the *unnamed* third choice — silently different. This ADR pins that contract.

**Decision.** τ is the only production path. All Spark Connect requests flow to τ; τ's output is the response. If τ cannot handle the input, τ returns a typed error that surfaces directly to the client. Errors fall into exactly three categories:

1. **Spark-emulated errors.** Inputs Spark itself would reject — unknown columns, ambiguous references, type mismatches, aggregate-context violations, and malformed queries **that Spark also rejects**. τ emulates Spark's error semantics: same error class, same message shape where practical, same failure mode. The Spark Connect facade is preserved — a Spark client sees the same errors it would see against reference Spark. Concretely: `AnalyzerError::AmbiguousColumn` / `UnknownColumn` / `UnknownTable` / `TypeMismatch` and future Spark-equivalent variants.

2. **Thunderduck-boundary errors.** Inputs Spark accepts but τ has not implemented, or that τ cannot correctly transliterate to DuckDB. Distinct error class: `AnalyzerError::PuntedOperator`, `EmissionError::UnsupportedOp` / `UnsupportedExpression` / `UnsupportedFunction`. The message is honest — "this operator is not implemented in Thunderduck," not "an internal error." This is the only place Thunderduck-specificity leaks through the Spark Connect facade, and the leak is deliberate and named.

3. **Strict rejections (Amendment 1).** Inputs Spark *accepts* that τ deliberately rejects as malformed under the SQL dialect Spark documents itself as following. Not a τ gap — τ will not implement these — and not Spark-emulated, because Spark raises nothing. Every instance is enumerated in the **Strict-rejection register** below; an unregistered strict rejection is a bug, not a policy application. The message names the malformation, never "not implemented."

No category triggers a runtime fallback. All surface directly to the client. A slice's Pass 1 architect designing a new error variant classifies it into one of the three categories at design time; a reviewer verifies the classification. Category 3 additionally requires a register entry in the same change.

**Alternatives considered (and rejected).**
- *Fall back to an alternate implementation when τ does not implement an input.* Adds runtime dispatch machinery, corpus-attribution instrumentation, and cross-implementation coordination. The alternate implementation drifts because nobody actively maintains it while τ climbs. Silent-divergence failures become indistinguishable from "green via τ" in the aggregate progress signal.
- *Two paths behind a build-time feature flag.* Same coordination cost as runtime dispatch; adds "which flag was set when I built this artifact?" as a surface bug.

**Consequences.**
- (+) Progress signal is unambiguous: every corpus-green case is τ's own success; every red case is a τ bug, an unimplemented feature, or — since Amendment 1 — a registered strict rejection carried by a `divergent`-flagged case. Amendment 1 would have *broken* this property had the flag not been added with it: an intentional divergence and a regression are indistinguishable to a bare pass/fail oracle.
- (+) No fallback machinery — no eligibility predicate, no attribution instrumentation, no runtime env-var routing.
- (+) The Spark Connect facade is honest about where the Spark emulation ends: Thunderduck-boundary errors say so.
- (−) The DataFrame corpus (`tests/scripts/v2-progress.sh`, 324 cases) is the fitness function while τ grows; TPC-H is temporarily red until τ covers its query surface.
- (−) Any non-τ source that remains as reference material does not compile as a service backend and is not exercised by tests. INV10's grep barrier (§CV.5) forbids τ from importing behavior-carrying types across the boundary.

**Refinement hooks.**
- **Timing of non-τ source cleanup** is a scheduling decision, not an ADR one — delete once no test, no CI job, and no build step references it. Incremental per-slice deletion is permitted.
- **Boundary-error precision.** Over time, the set of `Unsupported*` variants shrinks as τ grows. A future ADR (post-corpus-green) may enumerate the residual *permanent* unsupported set — inputs Spark accepts that τ will never support (e.g. distributed-only operators). Until then, boundary errors are pragmatic — "not yet" is a valid reason.
- **Reference emulation is produced by ADR-024 and ADR-026.** ADR-024's 0/1/2+ attribute binding produces qualified-reference `UnknownColumn` / `AmbiguousColumn` errors. ADR-026 separately produces Spark Connect's plan-ID-specific error classes. Neither path may degrade to permissive name-only binding where Spark rejects the reference.

**Carve-out register (for permanent Thunderduck-boundary errors).** *Currently empty.* When a boundary error is deemed permanent (Thunderduck will not implement this feature), it is recorded here with a written justification. Non-permanent "not yet" boundary errors are the ADR-022 category-2 default (surfaced to the caller with an honest `Unsupported*` reason) — they are not tracked here.

---

### Amendment 1 (2026-08-07) — τ rejects malformed SQL that Spark's permissive grammar accepts

**Decision.** Thunderduck rejects SQL that is malformed under the dialect Spark
documents itself as following, **even where Spark's own parser accepts it**.
Such a rejection is category 3 above: a *strict rejection*, reported as the
client's error (`INVALID_ARGUMENT`) with a message naming the malformation —
never as a Thunderduck-boundary "not implemented".

**Why this is not the forbidden "silently different".** The prohibition in this
ADR's Context is on *unnamed* divergence — behaviour that differs with nothing
recording that it does. A strict rejection is loud (the query fails), named (a
register entry), and regression-tested (a `divergent` corpus case). The failure
mode ADR-022 exists to prevent is a wrong answer that looks like a right one;
this is the opposite.

**Rationale.** Spark's grammar accepts constructs the standard does not, most of
which are typos rather than intent. `SELECT * FROM emp WHERE` is accepted by
Spark because `WHERE` — a reserved word under the standard — is parsed as a
table alias, silently yielding a query with no predicate. Returning an unfiltered
result for what is almost certainly a truncated query is a worse outcome for the
user than failing. Thunderduck prefers to fail.

**What defines "malformed" — the arbiter.** *Not* "whatever `sqlparser` rejects."
A 36-case survey against live Spark 4.1.1 (2026-08-07, recorded in
`tasks/tau-error-class-audit-2026-08.md`) established that `sqlparser`'s grammar
is not a proxy for the standard in **either** direction: it rejects valid Spark
(HiveQL `TRANSFORM`, `CREATE TABLE ... USING parquet`) and accepts malformed SQL
Spark rejects (`SELECT id + FROM emp`). Making the parser the authority would
therefore both slander valid queries as malformed and let real malformations
through. The authority is the **register below**: an input is a strict rejection
only if it is listed, with Spark's observed behaviour and a justification. An
unregistered strict rejection is a bug in τ, not an application of this policy.

**Scope.** Category 3 applies *only* where Spark **accepts** the input. Where
both engines reject and only the class or stage differs — `SELECT FROM emp`
(Spark: `UNRESOLVED_COLUMN.WITHOUT_SUGGESTION` at analysis), `SELECT id, FROM emp`
(Spark: `TRAILING_COMMA_IN_SELECT`) — that remains category 1, and τ owes Spark's
class. Amendment 1 is not a licence to stop matching Spark's error classes.

**Corpus mechanism.** A `divergent` flag on a corpus case, carrying the τ error
class the case must produce. The case is **green when τ diverges exactly as
registered** and red if τ's behaviour drifts in either direction — accepting the
input again, or rejecting it with a different class. This keeps the fitness
function meaningful and makes each divergence itself regression-tested. *(Status:
specified here, not yet implemented in the harness.)*

**Strict-rejection register.**

| # | Input | Spark 4.1.1 | τ | Justification |
|---|---|---|---|---|
| 1 | `SELECT * FROM emp WHERE` | **Accepts** — parses `WHERE` as a table alias, returns all rows unfiltered | Reject: malformed | `WHERE` is reserved under the standard and cannot be an alias. Spark's reading turns a truncated query into a silent full-table scan; a wrong answer that looks right is precisely what this ADR exists to prevent. |

**Known non-conformance (τ is currently too lenient).** The same survey found
`SELECT id + FROM emp` — a dangling operator — **accepted by τ** and rejected by
Spark (`UNRESOLVED_COLUMN.WITHOUT_SUGGESTION`, having parsed `FROM` as an
identifier). This amendment obliges τ to reject it, so it is an open defect
against this policy rather than a register entry. Tracked in
`tasks/tau-error-class-audit-2026-08.md`.

**Consequences of Amendment 1.**
- (+) Truncated or typo'd SQL fails loudly instead of returning a plausible wrong
  answer.
- (+) The divergence surface is bounded by an enumerated register rather than by
  a third-party grammar that drifts with every `sqlparser` bump.
- (−) A user migrating a query that Spark tolerated will see it fail. That is the
  intended trade, but it is a real migration cost and each register entry must
  earn it.
- (−) τ must now *tighten* in at least one known place (above), so the amendment
  creates work rather than only ratifying current behaviour.
- (−) Requires the `divergent` corpus flag to exist before any entry can be
  regression-tested; until then register entries are documented but unguarded.

---

