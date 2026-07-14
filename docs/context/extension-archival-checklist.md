# Extension Archival Checklist

> Phase 2 of the DuckDB extension absorption (`extension/` in-tree, `feat/vendor-extension`) makes `nubank/thunderduck-duckdb-extension` (and its mirror `lastrk/thunderduck-duckdb-extension`) redundant as a *build* dependency: thunderduck-rs no longer reads from them at any point in a local dev build, a CI build, or a fresh clone. This checklist is what to verify **before** actually archiving those repositories on GitHub, plus the exact (manual, human-run) archival commands. Archival itself is a repo-hosting action outside a coding agent's write scope — this doc is deliberately a checklist for a person to run, not a script this repo executes.

## Why archive at all

Once `extension/` is the only source of truth, keeping the origin repos live invites drift: someone patches a bug there instead of in `extension/`, and the fix never reaches thunderduck-rs. Archiving (GitHub's read-only "Archive this repository" state) closes that loop while preserving history for anyone who lands here from an old link.

## Pre-archival gates

All six must be green before archiving. Each corresponds to a stage of the phase-2 absorption; re-run the cited command if you're not looking at a fresh gate result.

1. **Import fidelity.** `extension/` is content-identical to the origin repo at the commit it was imported from, modulo the deliberate deltas — the authoritative list is `extension/README.md`'s Provenance section ("Deliberate deltas vs the import HEAD") — and the `.claude/`/`.github/`/`.nu/`/submodule-content exclusions.
   ```bash
   diff -rq --exclude='.git*' --exclude='duckdb' --exclude='extension-ci-tools' --exclude='.claude' --exclude='.nu' --exclude='.github' /path/to/origin/checkout extension/
   ```
   Expect empty output outside the recorded deltas.

2. **Submodules build both ways.** `extension/duckdb` and `extension/extension-ci-tools` are root-level submodules (`.gitmodules`), not fetch-scripted and not checked in as content. Prove the Rust build is unaffected by their presence *or* absence:
   ```bash
   git submodule deinit -f extension/duckdb extension/extension-ci-tools
   cargo build -p thunderduck-core   # must succeed — submodules are extension-build-only, not a Rust build input
   git submodule update --init extension/duckdb extension/extension-ci-tools
   ```

3. **Local build + smoke.** `scripts/dev/build-extension.sh --smoke` builds `extension/` for the host platform at the single DuckDB version pinned in `extension/BUILD_PINS.toml`, and:
   - passes the three-way version lock (submodule tag / `BUILD_PINS.toml` / `duckdb` crate version),
   - produces a `.duckdb_extension` binary whose footer names the pinned DuckDB version and the host platform,
   - passes `make test` (the extension's own SQLLogicTest suite), and
   - passes the swap-in proof — `THUNDERDUCK_EXT_PATH=<locally built binary> cargo test -p thunderduck-core --lib extension_loader -- --nocapture` — including the `spark_avg_decimal_probe` case, proving thunderduck-rs loads and correctly type-resolves a **freshly, locally built** binary, not just the vendored one.

4. **Adoption flow.** `scripts/dev/adopt-extension-release.sh --from-local <dir>` regenerates `extensions/vendored/MANIFEST.toml`'s `[source]` block from an in-tree build (no origin-repo release download involved) and round-trips cleanly — a dry run followed by restoring the pre-existing manifest must leave `extensions/vendored/` byte-identical to before the dry run.

5. **CI release workflow.** `.github/workflows/extension-release.yml` (manual `workflow_dispatch` only) builds all 4 shipped platforms from `extension/` at the pinned DuckDB version and opens a PR checking the binaries into `extensions/vendored/` — it does not read from the origin repo at any step. (The workflow's `override_ci_tools_repository: lastrk/extension-ci-tools@bebb406d...` is unrelated to the origin *source* repo — see the fork-persistence caveat below.)

6. **Docs sweep.** No remaining *production* reference to the origin repo outside historical/provenance files:
   ```bash
   grep -rn "nubank/thunderduck-duckdb-extension\|lastrk/thunderduck-duckdb-extension" docs/ README.md extension/ .github/ scripts/ 2>/dev/null
   ```
   Expect hits only in: `extension/README.md` (Provenance section), `extension/BUILD_PINS.toml` (`[provenance]` block), `docs/thunderduck-rearchitect-ADRs.md` (dated historical notes that explicitly say "kept as-is"), this checklist, `docs/dev_journal/` entries (chronological history — never edited retroactively), `scripts/dev/adopt-extension-release.sh` (`REPO_SLUG` + doc comment for the legacy download mode — retained deliberately, since archived GitHub repos still serve release downloads — and the in-tree `[source]` provenance string), and `.github/workflows/extension-release.yml` (provenance text in the generated PR body).

## Manual archival commands (human-run, after all 6 gates are green)

These are **not** run by any agent working in a thunderduck-rs worktree — they act on the origin repositories, which are out of this repo's write scope entirely. Run them yourself, from wherever you have the appropriate GitHub permissions:

```bash
# Origin repo
gh repo archive nubank/thunderduck-duckdb-extension

# Mirror (if you also control it / it isn't already read-only)
gh repo archive lastrk/thunderduck-duckdb-extension
```

Optionally, before archiving, add a final commit or repo-description update on the origin pointing readers at the new home:
```bash
gh repo edit nubank/thunderduck-duckdb-extension --description "ARCHIVED — absorbed into thunderduck-rs at extension/. See https://github.com/<thunderduck-rs>/tree/main/extension"
```

## Fork-persistence caveat — do NOT archive `lastrk/extension-ci-tools`

`.github/workflows/extension-release.yml`'s `build` job pins `override_ci_tools_repository: lastrk/extension-ci-tools` at commit `bebb406d50413c4f4a55a44d9316a69b9d1a0018` — **this is a completely different repository from `lastrk/thunderduck-duckdb-extension`** (the mirror above) and must **not** be archived or deleted as part of this cleanup. It exists solely to carry one patch on top of a `duckdb/extension-ci-tools` v1.5.x tag that disables EPEL 8 in the manylinux Docker images (upstream EPEL 8 went end-of-life; tracked as `duckdb/extension-ci-tools#374`/`#375`). The release workflow depends on that exact fork+commit existing and being fetchable indefinitely, until:

1. `duckdb/extension-ci-tools#375` (or an equivalent fix) lands upstream and is backported to a `v1.5.x` tag, at which point
2. `.github/workflows/extension-release.yml`'s `override_ci_tools_repository` / `ci_tools_version` should be dropped back to the canonical `duckdb/extension-ci-tools` tag (matching `extension/extension-ci-tools`'s own submodule pin, `extension/BUILD_PINS.toml`).

Until that day, archiving or deleting `lastrk/extension-ci-tools` silently breaks the release workflow's manylinux build legs the next time someone runs it. This repo is unrelated to the source-absorption story above and is out of scope for this checklist's archival step.
