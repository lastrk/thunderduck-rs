# v2 (τ) Architecture & Implementation Review — 2026-07-03

**Scope.** The v2 transliterator τ: `crates/core/src/transpiler_v2/` (~10.9k LOC), `crates/core/src/parser_v2/` (~1.2k), `crates/connect-server/src/converter/v2_relation_converter.rs` (~2.2k), dispatch in `service.rs`.
**Baseline.** Branch `feat/v2-transpiler`, HEAD `b263549` + uncommitted working tree; corpus ~205/324.
**Rubric.** `docs/adrs/README.md` (ADR-000…022 + §CV invariants INV1–INV10 / load-bearing LB1–LB9), `tasks/lessons.md`, `CLAUDE.md`.
**Method.** Four parallel read-only deep-dives (boundary, emission, analyzer/types, ingress) + line-level spot-verification of every High/Med finding.

> **Caveat — moving target.** The repo was being edited *during* this review: the spine doc gained ADR-022 and dropped INV7 at 14:02, and `emission.rs`/`expression.rs`/`mod.rs`/`struct_names.rs` are uncommitted. Line numbers are a snapshot of the working tree at review time.

---

## Verdict

The core architecture is **holding**. No real code contaminates τ with behavior-carrying legacy types (INV10/INV3 clean in practice); there is no legacy fallback (ADR-022); dispatch is at the protobuf boundary (ADR-004/021); emission is a coherent hand-written `match` (Approach A, ADR-009-sanctioned) with no string-surgery on generated SQL and no cost-based optimization (ADR-001/019); and the Arrow-value ingress dispatch that caused the historical silent-NULL corruption is now loud and grep-guarded.

The defects cluster in three places: **(1)** a silent `Unresolved`→`VARCHAR` corruption chain that spans ingress → analyzer → emission; **(2)** type-table drift/omissions vs the legacy oracle; and **(3)** the architecture's *own guardrails* (INV5/INV6/INV10 enforcement) having gaps, plus pervasive stale slice-scoped doc drift.

---

## 1. Architecture violations

### 1.1 [HIGH] Silent `Unresolved` → `VARCHAR` corruption chain (ingress → analyzer → emission)
Three independently-verified links form one data-corruption vector:

- **Ingress:** `crates/connect-server/src/converter/type_converter.rs:199` — `_ => DataType::Unresolved` (legacy `parse_type_str`). Any unrecognized DDL/JSON type string silently becomes `Unresolved`; it also cannot parse `struct<…>`/`map<…>` at all (returns `StructType::empty()` at `v2_relation_converter.rs:1230`).
- **Analyzer:** `crates/core/src/transpiler_v2/type_inference.rs:701` and `:688` — `_ => Unresolved` for any unknown function.
- **Emission:** `crates/core/src/transpiler_v2/emission.rs:2601` — `DataType::Unresolved => "VARCHAR"`.

**Rule.** INV5 ("schema everywhere; no `Unresolved` reaches emission"), ADR-022 (boundary errors must be honest, not silent), `lessons.md` "silent-NULL catch-alls are data-corruption anti-patterns."
**Why it matters.** An expression/type τ doesn't actually understand is emitted as a fabricated `VARCHAR` (or an empty struct schema) rather than surfacing an `Unsupported*` boundary error — the emission-side twin of the `relation_converter.rs` bug the project already fixed once. This is exactly the "typed expression above a delegated construct" hazard ADR-005 calls the highest-risk path.
**Fix.** Make the emission `Unresolved` arm a typed `EmissionError`; give τ a *fallible* type-string parser (`Result<DataType, …>`, loud on the unknown arm); assert `has_resolved_schema` on the production path (see 3.2).

### 1.2 [MED] τ ingress depends on the *legacy* converter for type parsing
`v2_relation_converter.rs:43` — `use crate::converter::type_converter::{parse_type_str, proto_to_data_type};` (used at `:468`, `:1059`, `:1225`). `type_converter.rs` is legacy ("not part of τ … deletable at any point," CLAUDE.md).
**Rule.** ADR-021 substrate independence; INV10 *spirit* (INV10's literal list names `thunderduck_core::…` modules, so a `connect-server`-local legacy helper slips the explicit check).
**Why it matters.** τ inherits 1.1's silent catch-all through this import, and the v2 path breaks the day Slice K deletes the legacy converter. This is precisely the coupling the barrier exists to prevent.
**Fix.** Move a τ-owned type-string/proto-type parser into `transpiler_v2`/`parser_v2`; drop the `crate::converter::type_converter` dependency.

### 1.3 [MED] `EMIT_TAP` — process-global mutable state on the production dispatch path, for a test
`emission.rs:54` `static EMIT_TAP: AtomicU64`, `:62` `static EMIT_TAP_MUTEX: Mutex<()>`; `dispatch_op` does `EMIT_TAP.fetch_add(1, …)` on every `Ok` (`:160`). The only readers are `invariants.rs:33` (`inv2_dispatch_is_only_sql_writer`) and emission tests.
**Rule.** INV2 (node-local decisions; no hidden non-local state) — ironically the invariant this counter is meant to *prove*.
**Why it matters.** Global mutable state is threaded through production emission purely to satisfy one counting test; the mutex exists only to serialize that test. The property ("dispatch is the sole SQL writer") is already structurally true because `dispatch_op` is the single entry.
**Fix.** Gate `EMIT_TAP` behind `#[cfg(test)]`, or prove single-writer structurally (module privacy / a call-site grep test) and delete it.

### 1.4 [MED] Symmetric-omission discipline violated: `any`/`some` aggregates
`any`/`some` (aliases of `bool_or`) appear only in the `function_return_type` delegation list (`type_inference.rs:575-576`) but are **missing** from: the boolean return arm (`:346` lists only `bool_and|every|bool_or`), both nullability predicates, and `AGGREGATE_NAMES` (`:827-829`). Delegation lands them on `aggregate_return_type`'s `_ => arg_type.clone()` (`:355`) instead of `Boolean`. (Also a duplicate `"every"` across the overlapping lists at `:577` and `:585`.)
**Rule.** ADR-005 symmetric-omission discipline; `lessons.md` "count_if" ("the omission travels together across sibling code paths").
**Why it matters.** This is the exact trap already fixed for `count_if`, re-occurring. Worse, because `any`/`some` are missing from `AGGREGATE_NAMES`, the mechanical omission test that iterates that constant (`type_inference.rs:977`) **cannot see them** — see 3.4.
**Fix.** Add `any`/`some` to the `bool_or` arm, both nullability predicates, and `AGGREGATE_NAMES`; delete the duplicate `every`.

---

## 2. Accidental complexity

### 2.1 [LOW] Dead / no-op code that reads as coverage
- `emission.rs:2386` `#[allow(dead_code)] spark_aggregate_return_cast(...)` — never wired; only reference is an identity test. Documents an emission decision no dispatch consults (helper-scale version of the dead-`EMISSION_TABLE` anti-pattern). Integer-`SUM`→`BIGINT` parity (CLAUDE.md Gotcha #7) is not applied in the aggregate path via this helper — confirm where, if anywhere, it happens.
- Identity `match` arms `name => name` (`emission.rs:1299`, `1383-1388`, `1473-1474`) are byte-identical to the `_ => &name_lower` fallthrough; the `spark_*` ones falsely imply an extension-membership guard that isn't there.
**Fix.** Delete the dead helper and identity arms, or make them load-bearing (route `spark_*` through `extension_targets()`).

### 2.2 [LOW] Redundant double plan-walk in overlay seeding
`service.rs:107` `build_base_types` calls `plan_has_empty_scan(...)` then `BaseTypes::build_from_plan(...)`, which *itself* re-checks `!plan_has_empty_scan` at `base_types.rs:54`. The outer guard is fully redundant (2–3 recursive walks/request). Separately, the catalog closure is `|_| None` — so the ADR-012 overlay is currently **never seeded with real Spark types** (relies entirely on AST-carried schemas); the comment claiming "Slice B wires the catalog bridge" is stale.
**Fix.** Call `build_from_plan(ast, |_| None)` unconditionally; track the unwired catalog bridge explicitly.

### 2.3 [LOW] `render_function_call` is a ~550-line function with load-bearing guard order
`emission.rs:1262-1818` mixes ~30 early-return special shapes with a name→name `match`; correctness depends on guards preceding fallthroughs (e.g. `sort_array` 2-arg at `:1490` before `:1515`) with no structural protection.
**Fix.** Split into `render_special_function` (early-returns) + a name table; add a test that no name appears in both.

### 2.4 [LOW] Redundant / brittle invariant scaffolding
INV3 is checked twice with divergent base sets (`invariants.rs:73` = `{generator,functions}` vs `emission.rs:3254` = `{generator,functions,logical,parser,runtime}` with a stronger `contains` form). Both region-split on the literal string `"#[cfg(test)]\nmod tests {"` — a `rustfmt` change to that line silently makes the scan cover the whole file.
**Fix.** Keep one authoritative INV3 self-test; gate the region split on a sentinel the test owns, not on source layout.

---

## 3. Guardrail / enforcement gaps

### 3.1 [MED] INV6 is unenforced while τ emits ~9 extension functions
`invariants.rs:151` `#[ignore] inv6_extension_targets_exist() { todo!() }`; `emission.rs:2629` `extension_targets()` returns an empty set (test at `:3337` asserts it's empty). Yet emission actively emits `spark_hash`, `spark_xxhash64`, `spark_try_divide/sum/avg`, `spark_skewness`, `sha256`, … (`emission.rs:1380-1388`, `1871`, `1882`).
**Rule.** INV6 (every `Extension(...)` target exists and is loaded); ADR-010.
**Why it matters.** Nothing verifies an emitted extension name exists in ext6 — a typo or missing symbol becomes a raw DuckDB runtime error, not a caught boundary. The one INV6 data source (`extension_targets()`) is disconnected from the actually-emitted names. (Deferral to Slice D is sanctioned, but the barrier is currently *vacuous* while extensions are already emitted.)
**Fix.** Populate `extension_targets()` from the emitted set and un-`#[ignore]` INV6 in the same change.

### 3.2 [MED] INV5 is a fixture-only assertion, not a runtime guard
`has_resolved_schema` (`analyzer.rs:432`) is called only by `inv5_schema_everywhere` over the fixture registry (`invariants.rs:131`). Nothing checks it on the production `generate()` path.
**Why it matters.** "Schema everywhere" holds only for the fixture set; a production plan whose shape no fixture covers can reach emission with `Unresolved` and, via 1.1, produce wrong SQL silently.
**Fix.** Call `has_resolved_schema` in `generate()` after `analyze()`; convert failure to a boundary error.

### 3.3 [MED] INV10 checker matches only `use`-prefixed line starts
`invariants.rs:351` matches lines that `starts_with("use crate::…")`. It misses: inline fully-qualified paths (`crate::functions::foo()` with no `use`), grouped/brace imports (`use crate::{types::TypeInferenceEngine, …}`), brace members on continuation lines, and `as`-renames. The sibling `emission.rs:3263` already uses the stronger `contains("crate::{base}::")` form. Also, the ADR §CV.5 grep-of-record (`docs/…:720`) omits `parser`/`runtime` that the checker does cover — doc narrower than code.
**Why it matters.** The load-bearing substrate barrier has documented blind spots. *No current code exploits them* (grep confirms), so this is latent — but the check is weaker than it reads.
**Fix.** Match the path-segment substring after comment-stripping + brace-joining; align the ADR grep upward.

### 3.4 [MED] The symmetric-omission tests are undercovered by their own denominator
The `§8` omission tests iterate `AGGREGATE_NAMES` (`type_inference.rs:977`, `expression.rs:1020`). A name omitted from `AGGREGATE_NAMES` itself (e.g. `any`/`some` per 1.4; `nth_value` is in a nullability predicate but not the constant) is invisible to the very test meant to catch omissions. `array_agg` is in a nullability predicate (`:388`) but has no return-type arm (falls to `_ => arg_type.clone()`).
**Fix.** Derive the tables from `AGGREGATE_NAMES` (or make it their union) so the test's denominator cannot silently under-cover.

---

## 4. Documentation drift (cross-cutting accidental complexity)

### 4.1 [MED] Pervasive stale slice-scoped doc-comments — some leak to clients
Every reviewed area carries frozen early-slice prose contradicted by the code:
- `invariants.rs:7` "At Slice A.1 only INV10 is active" — INV2/3/4/5/10 are all active gates.
- `mod.rs:8-13` + `emission.rs` header — claim only `SingleRow..Limit` wired; `dispatch_op` (`emission.rs:124-146`) wires Aggregate/Join/SetOp/WithColumns/Deduplicate/NaFill…; only `TableFunction`/`Unnest` still error.
- `service.rs:44-47` — "τ `generate()` errors unconditionally" (contradicted by its own tests).
- `base_types.rs` build comment — "Slice B wires the catalog bridge" (still unwired).
- **~30 client-visible** `reason: "… not supported at Slice A.2"` strings in `v2_lowering.rs` / `v2_relation_converter.rs` — internal slice labels leak onto the wire in `Status::unimplemented` messages.
**Fix.** Refresh module headers to current state; strip slice numbers from client-facing `reason` strings (keep in code comments only).

### 4.2 [LOW] `docs/adrs/README.md` (the ADR index) is now stale vs the 14:02 spine edit
The index authored earlier today says "ADR-000 → ADR-021," lists INV7 as an active invariant, and uses pre-edit INV10 wording; the spine now has ADR-022 and INV7 = "none." (Owned by me — offer to refresh.)

---

## 5. Bonus: Spark-parity bugs found en route (secondary to the review's focus)

Not "complexity" or "arch violations," but latent correctness gaps the differential oracle will eventually flag:

- **[HIGH] `ceil`/`floor` return type** — v2 `type_inference.rs:623` hardcodes `Long`; the legacy oracle (`types/type_inference.rs:540`) returns `Long` only for Decimal/Double/Float and preserves the arg type otherwise. `ceil(int_col)` gets the wrong column type.
- **[MED] `Cast` nullability** — `expression.rs:531-543` omits `String → Boolean` (and likely `Binary`) from the fail-to-null set; `CAST(str AS BOOLEAN)` is stamped non-nullable but Spark can return NULL.
- **[LOW] `date_trunc` → always `Timestamp`; `map(...)` → always `Map<String,String>`** (`type_inference.rs:645`, `660-666`) — plausible-but-wrong for the non-common case; both have full `&FunctionCall` available to do better (as `struct`/`named_struct` already do).
- **[LOW] `date_format`** (`emission.rs:1366`) — fixed 8-token `replace()` chain mistranslates uncommon `SimpleDateFormat` patterns (AST-built, so *not* an ADR-019 violation; a parity gap).

---

## Explicitly NOT flagged (sanctioned by ADR — do not "fix")

- Two `TypeInferenceEngine`s (τ's own vs legacy) and the hard-copied coercion lattice / function shapes — ADR-021 §4 + `lessons.md` "legacy-verbatim shape hard-copying is the honest cost."
- Hand-written `match` dispatch instead of a codegen'd table — ADR-009 amendment + `lessons.md` "Approach A."
- Empty `rewrites.rs` (B layer) — ADR-007.
- Structured `LocalRelation`/`Values` + first-class `plan_id` (no synthesized VALUES-SQL, no string-encoded qualifiers) — ADR-021.
- `parser_v2/dialect.rs` verbatim `SparkDialect` duplication; `format_decimal128` copy — sanctioned barrier duplication.
- Fused single bottom-up analyzer pass + set-op down-sweep; correlated-subquery deferral — ADR-006.
- Internal resolver/star-expander in the analyzer — required by INV5 (not over-resolution).
- `#[ignore]` INV1/INV8/INV9 stubs — deferred-slice ownership per §CV.5.1.
- Arrow-value ingress `_ => Err(UnsupportedProtoShape)` (grep-guarded) — the fixed Gotcha #9, working as designed.

---

## Recommended order of attack

1. **Close the `Unresolved` chain** (1.1) — highest blast radius: emission `Unresolved` → boundary error; fallible type-string parser owned by τ (also closes 1.2); `has_resolved_schema` on `generate()` (3.2).
2. **Fix the oracle drifts** (1.4 `any`/`some`, 5 `ceil`/`floor`) and make `AGGREGATE_NAMES` the union denominator (3.4) so the omission test regains coverage.
3. **De-scope `EMIT_TAP`** behind `#[cfg(test)]` (1.3).
4. **Sweep stale slice labels** (4.1), prioritizing the client-visible `reason` strings.
5. **Tighten INV10 matching** (3.3) and **wire INV6** when `extension_targets()` is populated (3.1).
6. Housekeeping: delete dead code (2.1), remove the redundant plan-walk (2.2), consolidate INV3 checks (2.4).
