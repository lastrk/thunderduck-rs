# Function Registry (SUPERSEDED)

> **SUPERSEDED — DO NOT USE AS GUIDANCE — HISTORICAL REFERENCE ONLY.**
> This ADR describes the retired legacy v1 transpiler. The corresponding Rust modules were deleted on 2026-07-05. Kept in-tree as a historical reference to the pre-τ architecture. ADR index: [`../README.md`](../README.md) · τ spine: [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

**Decision: `LazyLock<FunctionRegistry>` with direct mappings and custom translators**

```rust
static FUNCTION_REGISTRY: LazyLock<FunctionRegistry> = LazyLock::new(FunctionRegistry::new);

pub struct FunctionRegistry {
    direct: HashMap<&'static str, &'static str>,
    custom: HashMap<&'static str, fn(&[&str]) -> String>,
}
```

500+ Spark → DuckDB function mappings ported from the Java reference. Spark-divergent functions (e.g. `hash`, `xxhash64`, decimal `sum`/`avg`, `skewness`) route through the `thdck_spark_funcs` extension, which is mandatory and bundled into every build (see [rearchitect ADR-020](../../thunderduck-rearchitect-ADRs.md)).

---

← [Back to ADR Index](../README.md)
