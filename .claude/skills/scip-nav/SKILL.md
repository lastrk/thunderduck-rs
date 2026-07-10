---
name: scip-nav
description: >-
  Type-accurate Rust code navigation from a static rust-analyzer SCIP snapshot —
  no resident language server. Use to answer "who calls / references this symbol",
  "where is this defined", and "find symbols named X" with TRUE, trait- and
  type-resolved results. Reach for it specifically to FILL THE GAPS that codegraph
  (syntactic tree-sitter) and semble (embeddings) get wrong on this codebase:
  trait-method call sites dispatched through Option<T>/generics (codegraph
  undercounts these badly), exact cross-crate definitions, and def-vs-reference
  separation. NOT for concept/NL search (use semble) or macro-body/blast-radius
  survey (use codegraph). SCIP carries symbols/defs/refs/docs but NOT hover types
  or macro expansion — those need a live LSP session.
allowed-tools: Bash(python3:*), Bash(rust-analyzer:*)
---

# scip-nav — SCIP-snapshot code navigation

A memory-cheap gap-filler for Rust code intelligence. It queries a static
`.scip/index.scip` produced by `rust-analyzer scip`. Generation costs a ~3 GB /
~17 s transient spike (fits the 8 GiB devcontainer cap — see the
`rust-analyzer-lsp-viability` memory); after that the index is a 10 MB file
queried at ~zero resident memory, unlike a warm MCP language server.

## When to use (and not)

- ✅ **"Who calls / references `X`?"** — trait/type-resolved, so it counts
  `.method()` calls dispatched through `Option<T>`, generics, and across crates
  that codegraph misses. Validated: `require_proto` → 44 refs (codegraph reported
  1, then 26; ripgrep 45 with a doc-comment false positive).
- ✅ **"Where is `X` defined?"** — exact, and separates the trait decl from each impl.
- ✅ **"What symbols are named like `X`?"** — workspace symbol search.
- ❌ Concept / natural-language search → use **semble**.
- ❌ Macro-body inspection, blast-radius/call-path survey, verbatim source → use **codegraph**.
- ❌ Type-at-point (hover) or macro expansion → SCIP can't; needs a live LSP session.

## Usage

```bash
S=.claude/skills/scip-nav/scip_query.py
python3 $S status                 # index freshness; warns if a .rs file is newer
python3 $S refs   <symbol>        # all references (call sites), grouped by file
python3 $S def    <symbol>        # definition(s), trait decl + each impl
python3 $S sym    <query>         # fuzzy symbol search (substring, case-insensitive)
python3 $S refresh                # regenerate the index (bounded ulimit + timeout)
# append --count to refs/def/sym for a terse integer (scripting/validation)
```

`<symbol>` is a bare identifier (e.g. `require_proto`, `CommonAst`, `convert`);
matching is identifier-boundary against the SCIP symbol string.

## Refresh policy

The snapshot is **point-in-time**. Run `refresh` after meaningful edits (or wire it
to a post-edit/pre-review hook). `status` flags staleness by comparing the index
mtime against the newest `.rs` file. `refresh` is bounded (`ulimit -v`, `nice`,
`timeout`) so a runaway rust-analyzer dies instead of the container.

## How it works

`scip_query.py` is a dependency-free SCIP protobuf reader (no `protoc`, no `scip`
CLI). It walks `Index.documents[].occurrences[]`, matching `Occurrence.symbol` and
testing the `Definition` bit of `symbol_roles` to split definitions from references.
Field numbers follow github.com/sourcegraph/scip `scip.proto`.
