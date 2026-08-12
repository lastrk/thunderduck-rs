# Thunderduck Architecture

Thunderduck is a Rust-native Spark Connect server that translates Spark
DataFrame and SQL plans into DuckDB SQL and returns Arrow results. `τ` is the
only production transpiler.

This page is a discovery entry point, not an architecture authority:

- [`adrs/README.md`](adrs/README.md) indexes the authoritative individual
  architecture decisions and routes each kind of change to the relevant ADRs.
- [`context/architecture.md`](context/architecture.md) is the concise current
  implementation reference: crates, data flow, key types, threading, analysis,
  emission, and Spark-parity invariants.
- [`adrs/runtime/`](adrs/runtime/) contains current serving and execution
  decisions.
- [`adrs/retired/`](adrs/retired/) and
  [`adrs/legacy-transpiler/`](adrs/legacy-transpiler/) are historical context,
  excluded from normal implementation guidance.

## Current data flow

```text
Spark Connect DataFrame protobuf ─┐
                                  ├─> CommonAst ─> analyzer ─> TypedAst
Spark SQL ─> parser and lowering ─┘                          │
                                                            v
                                           DuckDB SQL ─> Arrow results
```

The Connect converter and SQL frontend converge on the same common AST. The
analyzer owns Spark-visible identity, type, nullability, interval spans, and
Connect plan-ID semantics. Emission targets native DuckDB behavior where it is
Spark-compatible and the mandatory `thdck_spark_funcs` extension where it is
not. Unsupported accepted Spark shapes stop at an explicit Thunderduck
boundary; there is no fallback transpiler.

Read [`context/architecture.md`](context/architecture.md) before changing the
converter, analyzer, or emission, then load the ADRs selected by the router in
[`adrs/README.md`](adrs/README.md).
