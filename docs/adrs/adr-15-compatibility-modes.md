# ADR-15: Compatibility Modes

> **SUPERSEDED BY `docs/thunderduck-rearchitect-ADRs.md` §ADR-020 (2026-07-01).**
> The `Strict`/`Relaxed`/`Auto` `CompatMode` enum below has been eliminated. The `thdck_spark_funcs` extension is now **mandatory** and loaded unconditionally by every session; there is no relaxed/vanilla-DuckDB fallback path. The CLI flags `--strict` / `--relaxed` and the `THUNDERDUCK_COMPAT_MODE` environment variable are no longer recognized. See rearchitect ADR-020 for the full rationale (relaxed mode's parity ceiling was intrinsically capped at ~85%; the maintenance cost of two dispatch tables outweighed the deployment-flexibility benefit once the extension became reliably distributed).
>
> The decisions below are preserved for historical context only. Any conflict between this ADR and the rearchitect ADRs is resolved in favor of the rearchitect ADRs per the CLAUDE.md rule.

**Decision: Mirror the Java strict/relaxed/auto model**

```rust
pub enum CompatMode { Strict, Relaxed, Auto }
```

- **Relaxed** (default): vanilla DuckDB functions, ~85% Spark parity, no extension required.
- **Strict**: `thdck_spark_funcs` extension loaded, exact Spark numeric semantics, ~100% parity.
- **Auto**: strict if extension available, relaxed otherwise.

CLI flags: `--strict`, `--relaxed`. Environment variable: `THUNDERDUCK_COMPAT_MODE=strict|relaxed|auto`.

---

← [Back to Architecture Overview](../architecture.md)
