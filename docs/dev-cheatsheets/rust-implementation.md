# Rust Implementation Cheatsheet

Portable patterns for writing production-quality Rust that compiles
first-try. Assume Opus 4.7 / Sonnet 4.5+ reader — no language basics.

## Code search (before you edit)

Search ladder — pick by what you already know:

1. **Only the intent/behavior, no symbol yet** → `semble.search` (pass
   `repo` = project root, e.g. `/workspace`), then hand the hit to codegraph.
2. **A symbol or relationship** → `codegraph_explore` (source + callers +
   blast radius in one call).
3. **A literal string** → shell `grep -rn` last (no Grep tool on native
   builds — Claude Code v2.1.117+).

## Ownership & borrowing

- Accept the most general borrow that works: `&str` not `&String`,
  `&[T]` not `&Vec<T>`, `impl AsRef<Path>` not `&PathBuf`.
- Return owned types from constructors and transformations.
- `Cow<'_, str>` when a function sometimes borrows, sometimes allocates.
- `.clone()` is a last resort; leave a comment explaining why borrowing
  doesn't work. Prefer `to_owned()` for `&str → String` — signals intent.

## Error handling

**Library crates** — `thiserror`. Every variant carries context.
```rust
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("invalid header at byte {offset}: expected {expected}, got {actual}")]
    InvalidHeader { offset: usize, expected: u8, actual: u8 },
    #[error("unexpected EOF after {bytes_read} bytes")]
    UnexpectedEof { bytes_read: usize },
}
```

**Application crates** — `anyhow::Result` with `.context()` at every
fallible call site that crosses a logical boundary.
```rust
let config = std::fs::read_to_string(&path)
    .with_context(|| format!("failed to read config from {}", path.display()))?;
```

- Never `.unwrap()` in library code. Never.
- `.expect("reason")` acceptable in application code ONLY for
  invariants proven by preceding logic, with a message explaining why
  it cannot fail.
- Propagate with `?`. Add `.context()` when the auto-conversion loses
  the operation-that-failed information.

## Function design

- Do one thing. If you need "and" to describe it, split.
- Max 4 parameters. Beyond that → options/config struct.
- Return early to reduce nesting.
  ```rust
  if !condition { return Err(...); }
  // happy path at top level
  ```
- Iterator chains when clearer.
  ```rust
  let total: u64 = items.iter().filter(|i| i.is_active()).map(|i| i.cost()).sum();
  ```
  Don't force it — `for` with complex control flow (early break,
  mutable accumulation) is clearer than a convoluted iterator chain.

## Struct & enum design

- `Debug` derived on almost everything.
- `Clone`, `PartialEq`, `Eq`, `Hash`, `Default` only when semantically
  meaningful.
- Fields private by default; accessor methods when external code needs
  them.
- Builder pattern for structs with >3–4 fields or optional config.
- Enum variants descriptive: `ConnectionState::Handshaking`, not `State2`.

## Async (tokio)

- `async fn` for interfaces. Avoid `impl Future` returns unless naming
  the future type for storage.
- Never block the async runtime: `spawn_blocking` for file I/O, CPU
  work, or any call > ~1ms.
- `tokio::select!` for multiple futures — handle every branch, including
  cancellation.
- `tokio::sync::mpsc` for channels (bounded, explicit capacity).
- `JoinSet` over spawn-and-forget for structured concurrency.

## Testing

- Every new function gets a test; every bug fix gets a regression test.
- `#[cfg(test)] mod tests { ... }` in the same file for unit tests.
- Arrange-Act-Assert; descriptive names.
  ```rust
  #[test]
  fn parse_valid_header_returns_metadata() {
      let input = b"\x89PNG\r\n\x1a\n";                    // Arrange
      let result = parse_header(input);                     // Act
      assert!(result.is_ok());                              // Assert
      assert_eq!(result.unwrap().format, ImageFormat::Png);
  }
  ```
- `parse_empty_input_returns_unexpected_eof_error`, not `test_parse`.
- `#[should_panic(expected = "...")]` for panic paths.
- `proptest` / `quickcheck` for parsers, serializers, algorithmic code.
- Trait + test impl for external dependencies; `mockall` for complex
  interfaces.

## Preferred crates (defaults; project may override)

| Concern | Crate |
|---|---|
| Errors — library | `thiserror` |
| Errors — application | `anyhow` |
| Serialization | `serde` + `serde_json` |
| HTTP server | `axum` + `tower` |
| HTTP client | `reqwest` |
| Async runtime | `tokio` |
| CLI | `clap` (derive) |
| Logging | `tracing` (prefer over `log`) |
| Database | `sqlx` (compile-time checked) |
| Date/time | `chrono` or `time` |
| Config | `config` crate or `dotenvy` |

Never add a dependency for something achievable in <20 lines of std.

## Security

- Never hardcode secrets. Env vars via `dotenvy` or a secrets manager.
- Never log PII, tokens, passwords, keys. `secrecy::Secret<T>` for
  sensitive types (redacts on `Debug`/`Display`).
- Parameterized queries only — never `format!` user input into SQL.
- Sanitize file paths from user input; canonicalize + boundary check
  before joining into `PathBuf`.

## Pre-return checklist

- [ ] Compiles with no warnings.
- [ ] `cargo clippy -- -D warnings` passes.
- [ ] `cargo fmt --check` passes.
- [ ] No `.unwrap()` in library code.
- [ ] All new functions have tests.
- [ ] All public items have `///` doc comments.
- [ ] No TODO, FIXME, or commented-out code.
- [ ] No hardcoded credentials or secrets.
- [ ] Error messages carry diagnosis-quality context.
- [ ] No changes outside the requested scope.
