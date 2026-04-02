# Dev Journal — 2026-04-02 — Agent Pipeline, Code Review, Performance Optimizations

## Summary

Introduced a multi-agent development pipeline (architect/coder/reviewer/perf) with custom agent
definitions and a `/rust-feature` skill. Ran two full code review passes and one performance review
across `crates/`, fixing all Critical/High findings and most Medium findings. Applied 8 targeted
performance optimizations to hot paths.

---

## Agent Pipeline Setup

Added `.claude/agents/` with four specialized agent definitions:
- **rust-architect** — read-only, explores codebase and produces architecture plans
- **rust-coder** — read-write, implements plans and runs tests
- **rust-reviewer** — read-only, reviews for correctness, style, safety, maintainability
- **rust-perf** — read-only, identifies bottlenecks and proposes targeted optimizations

Added `.claude/commands/new-feature.md` — orchestrates the full pipeline:
architect → coder → reviewer → fix loop → perf reviewer → perf optimizer → summary.

Updated `CLAUDE.md` with Rust standards, build quality gates, preferred crates, code style rules,
and agent pipeline documentation.

---

## Code Review — Round 1 (12 findings fixed)

### Critical (2)
- **C1**: Integer overflow in interval decomposition — `unsigned_abs() as i64` wraps for `i64::MIN`.
  Fixed: keep arithmetic in `u64` throughout (`generator/mod.rs`).
- **C2**: Unchecked index `a.aggregates[*idx]` panics on malformed plans.
  Fixed: use `.get(*idx)` (`generator/mod.rs`).

### High (4)
- **H1**: `.unwrap()` in expression converter on `args.into_iter().next()`.
  Fixed: replaced with `args.remove(0)` (`expression_converter.rs`).
- **H2**: SQL injection via `$TZ` env var in `SET TimeZone`.
  Fixed: single-quote escaping (`session.rs`).
- **H3**: `RecordBatch::try_new().unwrap_or(b)` silently discards errors.
  Fixed: added `eprintln!` logging (`service.rs`).
- **H4**: `.ok()` silently ignores DDL failure.
  Fixed: propagated error with `?` (`service.rs`).

### Medium (6)
- **M1**: Duplicated `PipeIfUnresolved` trait → consolidated to `data_type.rs`.
- **M2**: Struct literal field names not quoted → use `quote_ident()`.
- **M3**: `inject_distinct` precondition undocumented → added doc comment.
- **M4**: `rewrite_named_struct` escaped quotes unhandled → handle doubled escapes.
- **M5**: Direct indexing in `nullable()` for "when" → iterator chain.
- **M6**: `num_rows() as i64` could wrap → `i64::try_from().unwrap_or(i64::MAX)`.

---

## Performance Optimizations (8 applied)

| OPT | File | Change |
|-----|------|--------|
| OPT-1 | `struct_type.rs` | `field_by_name`/`field_index`: `to_lowercase()` → `eq_ignore_ascii_case()` (zero-alloc) |
| OPT-2 | `generator/mod.rs` | `quote_ident`: fast path skips `replace()` when no `"` (single alloc) |
| OPT-3 | `functions/mod.rs` | `translate`/`translate_typed`: stack-based ASCII lowercase buffer (zero-alloc) |
| OPT-4 | `type_mapper.rs` | `to_duckdb` returns `Cow<'static, str>` (zero-alloc for scalar types) |
| OPT-5 | `generator/mod.rs` | `TD_DEBUG_SQL` env check cached in `LazyLock<bool>` (one syscall per process) |
| OPT-7 | `arrow_ipc.rs` | IPC buffer pre-allocated with size estimate |
| OPT-9 | `Cargo.toml` | `[profile.release]` with `codegen-units = 1`, `opt-level = 3`, `strip = true` |
| OPT-10 | `session.rs` | `run_query` keyword check: `to_uppercase()` → prefix `eq_ignore_ascii_case()` |

LTO disabled (`lto = false`) due to OOM on CI-class machines. Can re-enable with `lto = "thin"`
when build environment has sufficient memory.

---

## Code Review — Round 2 (5 findings fixed)

### High (2)
- **H1**: `rewrite_named_struct` emits unquoted field names in struct literals.
  Fixed: double-quote field names with escape handling (`generator/mod.rs`).
- **H2**: Timezone validation was escape-only, not a whitelist.
  Fixed: whitelist validation (alphanumeric + `/_-+: `), fallback to UTC (`session.rs`).

### Medium (3)
- **M1**: IPC buffer estimation overflow → `saturating_mul`/`saturating_add` (`arrow_ipc.rs`).
- **M2**: Unchecked `as i32` casts in `execute_approx_quantile` → `i32::try_from` (`service.rs`).
- **M3**: Unchecked `as u8` for decimal precision/scale → `u8::try_from` with defaults (`sql_converter.rs`).

---

## Other Changes

- Added `crates/core/build.rs` to version control (extension download script for `--features bundled-extension`)
- Added `venv/` to `.gitignore` (Python virtualenv for integration tests)
- Updated `docs/reference-gap-analysis.md`

---

## Test Status

**Unit tests**: 80 passing, 0 failing
