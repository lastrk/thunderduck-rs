# ADR-017 — Delta-table writes are scoped to append-into-existing via DuckDB's Delta extension; DELETE / MERGE / overwrite / create are rejected pending DuckDB support

**Status:** Proposed
**Depends on:** ADR-005, ADR-011, ADR-013, ADR-016
**Depended on by:** ADR-015

*(This is the first of the per-format external-table write specializations promised by OQ-2. It hangs off the command arm (ADR-011) and the external-table delegation (ADR-013); it is a leaf consequence, not part of the spine.)*

**Context.** DuckDB's Delta extension (write support graduated from experimental in v1.5.2) currently provides read plus *limited write*: a **blind `INSERT`/append** into an *attached* Delta table, atomic per transaction (multiple `INSERT`s inside one `BEGIN`/`COMMIT` become a single Delta version), with an optional idempotent-append API (`delta_set_transaction_version`) giving exactly-once retries via compare-and-swap on a per-application version. It does **not** yet support `UPDATE`, `MERGE`, or `DELETE`; table-creation DDL is officially future work (a `CREATE TABLE`-on-attached-schema path appears in some third-party examples but is unconfirmed and version-fragile). Writes require attaching the table (`ATTACH … TYPE delta`); the path-based `delta_scan` is read-only. Per INV8 thunderduck delegates the writer and does not implement Delta-protocol write logic itself, so thunderduck's Delta write surface is exactly DuckDB's.

**Decision.** For this iteration thunderduck supports exactly one Delta write path: **`INSERT` / append into a pre-existing, attached Delta table**. A single Spark write *action* maps to a single DuckDB transaction so that it produces one Delta version (matching Spark's one-commit-per-write). The source rows come from a τ-translated relation, so the emitted command is `INSERT INTO ⟨attached Delta table⟩ ⟨τ(source relation)⟩` — composing the existing relation path (τ) with the command arm (`emit_command`, ADR-011). Every other Delta write operation is **rejected with a diagnostic that names the limitation as DuckDB-version-gated, not a thunderduck limitation**, each with an explicit revisit-trigger:
- `DELETE FROM`, `MERGE INTO`, `UPDATE` → rejected; revisit when the Delta extension ships delete/update/merge.
- `mode("overwrite")` → rejected; faithful overwrite needs delete/truncate, which DuckDB-Delta lacks; revisit with delete support.
- CTAS / create-on-write, and the `errorIfExists` / `ignore` create-semantics → out of scope; DuckDB-Delta table-creation DDL is future work, so **a writable Delta table must pre-exist**; revisit when DDL is confirmed in a pinned build.

A writable Delta relation must have attached provenance (ADR-013); a path-scan Delta relation is read-only (INV9).

**Consequences.**
- (+) Delivers the highest-value actually-supported Delta write (append) now, composed cleanly from the existing τ relation path and `emit_command` rather than new machinery.
- (+) One-action → one-transaction → one-version matches Spark's commit semantics; the idempotent-append API is available if retry-safety is wanted.
- (+) Rejections are precise and DuckDB-gated, so the supported set lifts automatically as DuckDB's Delta writer matures, and the diagnostics tell users *why* a write is refused and *when* to expect it.
- (−) Append-only is a real functional gap versus Spark: no overwrite, delete, merge, or CTAS into Delta — many Spark Delta write workloads will not run until DuckDB catches up. This is an honest scope limit, surfaced as a typed rejection, not silent.
- (−) Insert-time type handling must reproduce **Spark's** store-assignment casting and column-resolution (by-name vs by-position), a write-direction instance of ADR-005 — not DuckDB's native `INSERT` casting.
- (−) Carries write-direction fidelity risk (LB8): DuckDB's Delta write surface is young, including a noted delta-kernel-rs OneLake regression.

**Refinement hooks.** Define the Spark-write-mode → action mapping (append supported; all others → typed rejection with revisit-trigger). Define `INSERT` column resolution to match Spark: `insertInto` is by-position, `saveAsTable` / `DataFrameWriterV2` by-name, with Spark's store-assignment casts — reproduce Spark's, validated via the oracle. Decide whether to use `delta_set_transaction_version` for retry-safety (note: Spark structured-streaming `txnAppId`/`txnVersion` exactly-once is out of scope here; batch append is one plain commit). Specify the state-diff oracle for append: logical read-after-write row comparison plus Delta version/log lineage, through both engines. Pin the Delta extension version (ADR-016) and track the delete/update/merge/DDL roadmap to lift the rejections one at a time.

---

