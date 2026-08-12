# gRPC Framework

> **Status: current — runtime/serving substrate.** Applies to τ (`crates/core/src/transpiler_v2/`). Active ADR index: [`../README.md`](../README.md).

**Decision: `tonic` + `prost`**

`tonic` is the canonical async-native gRPC library for Rust. `prost` handles protobuf codegen. The Spark Connect `.proto` files are copied verbatim from the Java reference implementation (`connect-server/src/main/proto/`) and compiled at build time via `tonic_build` in `build.rs`.

No alternative was seriously considered — tonic is the ecosystem standard.

---

← [Back to ADR Index](../README.md)
