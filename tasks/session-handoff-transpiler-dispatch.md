# Session handoff — transpiler dispatch flag (legacy vs v2)

**Pick up in:** a Claude session started **inside the devcontainer** (the build/test gate cannot run on the macOS host — see "Why verification is pending").
**State:** code complete, **uncommitted**, **not yet verified**. The working tree already contains all changes below (same checkout the container mounts).
**Branch when handed off:** `bugfix/port-thunderduck-late-may-2` (changes are uncommitted on top of it).
**Plan file (host):** `/Users/laszlo.torok/.claude/plans/linked-greeting-parrot.md` (not in repo; this doc supersedes it for continuation).

---

## Goal

Add a startup‑set switch that routes each Spark Connect request to either the existing transpiler ("legacy") or the new common‑AST/analyzer/emission pipeline ("v2") from `docs/thunderduck-rearchitect-ADRs.md`. Must **default to legacy**, be **non‑destructive**, and **hard‑error** when v2 is selected (pipeline not built yet). Mirrors the existing `RuntimeCompatMode` (`--strict`/`--relaxed` + `THUNDERDUCK_COMPAT_MODE`) pattern.

Confirmed decisions: v2‑selected‑but‑unimplemented → **gRPC `Unimplemented` per request**; surface = `--transpiler <legacy|v2>` + `THUNDERDUCK_TRANSPILER`; enum `TranspilerPath{Legacy,V2}` default `Legacy`; new pipeline lives as a **module in `core`**.

---

## What changed (uncommitted in working tree)

| File | Change |
|------|--------|
| `crates/core/src/transpiler_v2/mod.rs` **(new)** | `TranspilerPath{Legacy,V2}` + `parse()`/`from_env()` (default Legacy); stub `generate(plan, mode)` returns `ThunderduckError::Unsupported`; 3 unit tests. |
| `crates/core/src/lib.rs` | `pub mod transpiler_v2;` |
| `crates/connect-server/src/main.rs` | `Args.transpiler: Option<String>`; resolve CLI→`from_env`→Legacy, hard‑error on bad value; pass to `ThunderduckService::new(mgr, mode, transpiler)`; log chosen path. |
| `crates/connect-server/src/service.rs` | `transpiler: TranspilerPath` field + ctor param; new free fn `generate_sql(transpiler, plan, session) -> Result<String, Status>` (Legacy→`SqlGenerator`; V2→`transpiler_v2::generate` mapped to `ConnectError::Unsupported`→`Status::unimplemented`). All 6 relation→SQL sites routed through it: `execute_plan`, `analyze_plan` fallback, `CreateDataframeView`, `SqlCommand`, `WriteOperation`, `execute_approx_quantile`. `transpiler` threaded into the two free fns `handle_command` and `execute_approx_quantile`. |
| `CLAUDE.md` | Documented `--transpiler` / `THUNDERDUCK_TRANSPILER` in the Server cheatsheet. |

Design notes: proto→`LogicalPlan` conversion stays **shared** (ADR‑003: v2 IR *is* the existing AST). Converter‑internal `SqlGenerator` uses and `SchemaInferrer` schema inference intentionally **stay legacy** for now (v2 analyzer per ADR‑005 will own them later). `TranspilerPath` lives on the service (not the session) because it needs no per‑session resolution, unlike `CompatMode`.

---

## Why verification is pending

On the macOS host, `target/` holds **Linux ELF** build‑script binaries from the devcontainer, so every `cargo` invocation SIGKILLs. rust‑analyzer validated the edits (clean except a **pre‑existing** `field 'mode' is never read` on `ThunderduckService`, identical on `HEAD` — not introduced here). **Inside the devcontainer the gate runs normally.**

## Verification gate to run (in devcontainer) — CLAUDE.md "Verification Before Done"

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo test -p thunderduck-core transpiler_v2          # the new unit tests specifically
./tests/scripts/run-differential-tests.sh tpch        # legacy default must be unchanged
```
Manual smoke:
```bash
cargo build --release
./target/release/thunderduck-connect-server --help            # shows --transpiler
./target/release/thunderduck-connect-server --transpiler v2   # any query → gRPC Unimplemented
THUNDERDUCK_TRANSPILER=v2 ./target/release/thunderduck-connect-server   # same via env
./target/release/thunderduck-connect-server --transpiler bogus # exits with clear error
# unset / --transpiler legacy → unchanged behavior
```
This change does not touch SQL generation semantics, so the full `all`/strict differential runs are not required by the gate; TPC‑H (legacy default) green is sufficient to prove the seam is behavior‑preserving.

---

## Open decisions for the next session

1. **Pre‑existing dead `mode` field** on `ThunderduckService` (vestigial; `clippy -D warnings` will flag it). It predates this change. Either remove it here (drop the field + ctor param + the `mode` arg passed to the service in `main.rs` + the now‑unused `RuntimeCompatMode` import in `service.rs`; `main.rs` still computes `mode` for `SessionManager`) to keep clippy green, or leave it as out‑of‑scope. Recommend removing since clippy‑clean is a hard gate.
2. **Commit** once the gate is green: suggested new branch `feat/transpiler-dispatch-flag`, then PR against `nubank/thunderduck-rs` (the cleanup-docs PR #13 is the related prior work).

## If any gate step is red

Treat per CLAUDE.md "Autonomous Bug Fixing": the new code is small and localized to the 5 files above. Most likely issues: an `AsRef`/borrow mismatch at a `session.as_ref()` call, or a missing import (`DuckDbSession`, `LogicalPlan`, `transpiler_v2`) in `service.rs`. The `transpiler_v2::generate` stub is intentionally `Err(Unsupported)` — a failing v2 smoke test that expects `Unimplemented` is success.
