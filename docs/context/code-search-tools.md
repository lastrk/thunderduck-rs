# Code Search Tools

Use built-in shell tools first. `rg` is the lexical baseline for names,
strings, imports, macro calls, and broad local exploration.

For Rust symbol navigation, use the in-repository `scip-nav` skill:

```bash
S=.agents/skills/scip-nav/scip_query.py
python3 $S refs <symbol> [--count]
python3 $S def <symbol>
python3 $S sym <query>
python3 $S expand <crate> [pattern]
```

`scip-nav` reads immutable, worktree-safe rust-analyzer SCIP snapshots. It is
the canonical source for exact Rust definitions, trait/type-resolved
references, caller counts, and cross-crate symbol discovery. Refresh after
edits when necessary; queries fail closed if no matching snapshot exists.

Use `rg` when the question is textual or when discovering an unfamiliar area.
Then use `scip-nav` to establish exact Rust relationships before making a
dead-code, rename, or architecture decision. Macro expansion is available
through `scip-nav expand` and is intentionally heavyweight.

No other code-exploration service is part of the project workflow.
