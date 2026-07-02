# Function Registry (legacy path)

> **Status: existing implementation — runs behind `--transpiler legacy` (the default).** The authoritative v2 architecture supersedes this file where they conflict; the two paths coexist, so do not delete the legacy path to make room for v2. This file's v2 successor is listed in the legacy→v2 map in [`../README.md`](../README.md); the v2 spine is [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

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
