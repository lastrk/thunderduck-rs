# Rust Performance Cheatsheet

Portable discipline for identifying bottlenecks and prescribing
targeted, measurable optimizations. Does not change code style, add
features, or refactor for readability.

## Analysis hierarchy (work top-down)

1. **Algorithmic complexity** — the biggest wins live here.
   Nested loops that should be a `HashMap` lookup, O(V²) filtering
   that becomes O(V) with a pre-computed set, quadratic string
   concatenation, per-element linear scans.
2. **Allocation / memory** — heap traffic on hot paths, `Vec` growth
   without `with_capacity`, `format!` in loops, `.clone()` on `Arc`
   where `&Arc` would do, `String` where `&str` would do,
   `.collect::<Vec<_>>()` when the caller wants an iterator.
3. **Data layout** — struct padding, `Box<T>` where `T` is small,
   `Rc`/`Arc` where a borrow would work, cache-unfriendly access
   patterns, split fields that should be co-located.
4. **Concurrency** — lock granularity, contention hot spots, blocking
   calls on the async runtime (`std::fs`, `std::thread::sleep`,
   heavy CPU work inside `async fn` without `spawn_blocking`),
   `Arc<Mutex<Vec<T>>>` where a channel or sharded map fits better,
   locks held across `.await`.
5. **Syscall / I/O** — unbuffered `File` reads, per-line `write!`
   without `BufWriter`, `serde_json::from_reader` on unbuffered I/O,
   TLS handshake per request instead of pooled client, N+1 DB queries.
6. **Build / runtime tuning** — release profile settings (`opt-level`,
   `lto`, `codegen-units`), `#[inline]` on tiny hot functions, `PGO`
   for extreme cases. Rare wins; last resort.

## Prioritization

Don't know the function name, only the behavior of a hot path? `semble.search`
(pass `repo` = project root, e.g. `/workspace`) finds the candidate by intent,
then hand the hit to codegraph. Weight each candidate by
**call frequency × per-call cost × delta**.
`codegraph_callers` gives the frequency; profiling (`cargo flamegraph`,
`perf`, `pprof`, `cargo bench`) gives the per-call cost. Skip changes
where any of the three is small.

Cold paths (once per plan, once per RPC, startup, etc.) are usually
not worth optimizing — mention them as INFO but do not prescribe work.

## Proposal format

Every proposal must state:

```
### [SEVERITY] <one-line title>
Bottleneck:    <where the time / memory is going now>
Hypothesis:    <why the proposed change is faster / leaner>
Change:        <specific code delta, ≤10 lines when possible>
Verification:  <exact command to prove the win —
                cargo bench, criterion, hyperfine, /usr/bin/time, ...>
Risk:          <correctness, readability, dependency, complexity>
```

Severity ladder:
- **HIGH** — dominates a hot path; expected win ≥ 2× or eliminates a
  visible latency spike.
- **MEDIUM** — real win on a warm path; expected ≥ 20%.
- **LOW** — measurable but small win; only worth landing if trivial.
- **INFO** — noted for the record; no action recommended.

## What NOT to touch

- Style changes (rename, reorder, restructure without measured win).
- Feature additions.
- Refactors for readability.
- `unsafe` escape hatches unless the language really requires them
  and the project already documents `unsafe` as an accepted pattern.
- Micro-optimizations without a benchmark to prove them (branch hints,
  hand-unrolled loops, manual SIMD before `std::simd`).
- Speculative optimizations. If you can't measure the win, do not
  prescribe the change.

## Benchmark discipline

- `criterion` for micro-benchmarks with statistical rigor. Report:
  baseline mean ± σ, proposed mean ± σ, delta, and CV; commit both
  the benchmark and the result.
- `hyperfine` for CLI-level end-to-end timing (`hyperfine --warmup 3
  './bin_before' './bin_after'`).
- Compare on the same hardware, same load, warmed caches.
- Rerun after landing; regressions later are easier to catch when you
  have a locked baseline.

## Rust-specific traps

- `Vec::extend(iter)` allocates once if `iter` has a size hint;
  `push` in a loop reallocates. Prefer `extend` + `with_capacity` on
  known-size input.
- `String::push_str` is O(1) amortized; `+ &str` clones and reallocates.
- `HashMap::insert` on a String key: consider `entry` API to skip a
  redundant hash + one clone.
- `format!("{}", x)` builds a new String; use `write!(f, "{}", x)` when
  `f: &mut Formatter` or `impl fmt::Write` is available.
- `.collect::<Vec<_>>().iter()` is a needless allocation; keep the
  iterator alive instead.
- `Arc<Mutex<T>>` for read-heavy state → `Arc<RwLock<T>>` or
  `arc_swap::ArcSwap<T>`.
- Async: `tokio::sync::Mutex` only when the guard must cross `.await`;
  otherwise `std::sync::Mutex` is faster.
- `Box<dyn Trait>` in hot dispatch → generics or an enum with
  compile-time dispatch.

## Report shape

```markdown
# Perf Review — <change identifier>

## Summary
<paragraph: hot paths inspected, prioritization signal>

## HIGH
<proposals, or "none">

## MEDIUM
<proposals, or "none">

## LOW
<proposals, or "none">

## INFO
<notes / observations without action>

## Verdict
OPTIMIZED  |  HAS_OPPORTUNITIES  (HIGH + MEDIUM count: <N>)
```
