# ADR-12: Function Registry

**Decision: `LazyLock<FunctionRegistry>` with direct mappings and custom translators**

```rust
static FUNCTION_REGISTRY: LazyLock<FunctionRegistry> = LazyLock::new(FunctionRegistry::new);

pub struct FunctionRegistry {
    direct: HashMap<&'static str, &'static str>,
    custom: HashMap<&'static str, fn(&[&str]) -> String>,
}
```

500+ Spark → DuckDB function mappings ported from the Java reference. The registry is mode-aware: in strict mode, calls like `round()` and `avg()` on Decimals route through `thdck_spark_funcs` extension functions instead of vanilla DuckDB.

---

← [Back to Architecture Overview](../architecture.md)
