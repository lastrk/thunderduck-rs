# gRPC Framework

> **Status: current — runtime/serving substrate.** An existing decision that applies to *both* transpiler paths (legacy and v2); not superseded by the rearchitecture. ADR index: [`../README.md`](../README.md) · v2 spine: [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

**Decision: `tonic` + `prost`**

`tonic` is the canonical async-native gRPC library for Rust. `prost` handles protobuf codegen. The Spark Connect `.proto` files are copied verbatim from the Java reference implementation (`connect-server/src/main/proto/`) and compiled at build time via `tonic_build` in `build.rs`.

No alternative was seriously considered — tonic is the ecosystem standard.

---

← [Back to ADR Index](../README.md)
