---
description: "One-shot setup: install codegraph + semble + the right LSP for the current project, build the initial codegraph index, and install git hooks that keep it in sync across branch operations. Usage: /agentic-dev:setup"
---

You are the orchestrator for an interactive, idempotent, one-shot setup
of the external tooling that powers this plugin's agents. You do NOT
fix bugs, write features, or touch source code. Your job is to install
dependencies, register MCP/LSP servers (already declared in this
plugin's `.mcp.json` and `.lsp.json`), build the initial codegraph
index, and install git hooks that keep the index fresh across branch
operations.

**You must confirm every destructive step with the user before running
it.** Print the exact command first; ask explicitly; run only on yes.
Skip silently on no.

## Stage 0: Preflight detect

Detect what is already present. Do not install anything yet.

Run each of the following and record the result. Use a single Bash
block per row where reasonable, but split when needed for clarity.

1. **`uv` / `uvx`** — semble's runtime:
   ```bash
   command -v uv && command -v uvx
   ```
2. **`codegraph`** CLI:
   ```bash
   command -v codegraph
   ```
3. **Project language** — detect from build manifest in the current
   working directory:
   ```bash
   # Java
   [ -f pom.xml ] || [ -f build.gradle ] || [ -f build.gradle.kts ]
   # Rust
   [ -f Cargo.toml ]
   ```
4. **`jdtls`** (only if project is Java):
   ```bash
   command -v jdtls
   ```
5. **`rust-analyzer`** (only if project is Rust):
   ```bash
   command -v rust-analyzer
   ```
6. **Initial codegraph index for this project**:
   ```bash
   [ -f .codegraph/codegraph.db ] && echo "indexed" || echo "not indexed"
   ```
7. **Already-installed agentic-dev git hooks**:
   ```bash
   git -C . rev-parse --git-path hooks 2>/dev/null \
     | xargs -I{} grep -l 'agentic-dev codegraph sync' \
                  {}/post-checkout {}/post-merge {}/post-rewrite 2>/dev/null \
     || echo "no hooks installed"
   ```

Render the result as a table to the user:

```
| Dependency                | Required? | Status                |
| ------------------------- | --------- | --------------------- |
| uv / uvx                  | yes       | OK | MISSING          |
| codegraph                 | yes       | OK | MISSING          |
| Project language          | -         | java | rust | other  |
| jdtls                     | java only | OK | MISSING | N/A    |
| rust-analyzer             | rust only | OK | MISSING | N/A    |
| codegraph index           | yes       | OK | MISSING          |
| agentic-dev git hooks     | yes       | OK | MISSING          |
```

If everything is OK, skip to Stage 5 (summary). Otherwise continue.

---

## Stage 1: Install missing binaries (interactive)

For each MISSING binary, present the install command(s), explain what
it will do, and ask explicitly before running it. Skip silently if the
user declines — they may already have it installed via another path,
or they may want to install manually.

### 1a. `uv` (if missing)

`uv` is Astral's Python tool runner. We use its `uvx` mode to fetch +
launch semble on demand.

- macOS (Homebrew):
  ```bash
  brew install uv
  ```
- Linux / macOS (upstream installer):
  ```bash
  curl -LsSf https://astral.sh/uv/install.sh | sh
  ```

Ask which method to use; run only after explicit confirmation.

### 1b. `codegraph` (if missing)

- If `npm` is available:
  ```bash
  npm install -g @colbymchenry/codegraph
  ```
- Otherwise, the prebuilt installer:
  ```bash
  curl -fsSL https://raw.githubusercontent.com/colbymchenry/codegraph/main/install.sh | sh
  ```

Ask which method to use.

### 1c. semble — content scope

semble itself doesn't need installation once `uv` is on `PATH` —
`uvx --from "semble[mcp]" semble` fetches it on demand at MCP startup.
But semble has a `--content` flag that controls what it indexes
alongside source code (docs, config files, etc.). Ask the user:

> By default semble indexes source code only. To also index docs/config,
> I can add `--content all` to its args in `.mcp.json`. Do you want
> that? (yes / no)

If yes, edit this plugin's `.mcp.json` to append `"--content", "all"` to
semble's args list. Show the diff before applying.

### 1d. `jdtls` (if missing and project is Java)

- macOS (Homebrew):
  ```bash
  brew install jdtls
  ```
- Linux / Windows: point the user to the upstream install docs at
  https://github.com/eclipse/eclipse.jdt.ls — there is no clean
  one-liner. Print the URL and ask the user to install manually, then
  return.

### 1e. `rust-analyzer` (if missing and project is Rust)

- If `rustup` is on `PATH`:
  ```bash
  rustup component add rust-analyzer
  ```
- Otherwise, point at https://rust-analyzer.github.io/manual.html#installation
  and ask the user to install manually.

For Rust projects: also recommend (do NOT auto-install) the official
Claude Code Rust LSP plugin if not already present:

> The official Rust LSP plugin gives Claude Code richer Rust support
> than the bare LSP. Install it with `/plugin install rust-lsp` from
> the official marketplace.

---

## Stage 2: Initial codegraph index + `.gitignore`

### 2a. Build the index

Run in the current project root (assume the user's `cwd` is the project
they want to set up):

```bash
codegraph init -i
```

Print a summary of files indexed + time taken. If `codegraph` is still
missing (user skipped install in Stage 1), report that this stage is
blocked and skip to Stage 3.

If `.codegraph/codegraph.db` already exists (detected in Stage 0):
- Skip `init`; instead run `codegraph sync --quiet` for a freshness
  pass.
- Print: "Index already exists; ran incremental sync."

### 2b. `.gitignore`

Check whether `.codegraph/` is already ignored:

```bash
git check-ignore -q .codegraph 2>/dev/null && echo "already ignored" || echo "not ignored"
```

If not ignored, ask the user:

> The codegraph index lives in `.codegraph/` and is per-machine — it
> shouldn't be committed. I'd like to append the following to
> `.gitignore`:
>
>     # codegraph index (per-machine, managed by /agentic-dev:setup)
>     .codegraph/
>
> OK to apply? (yes / no)

If yes, append exactly those two lines. If `.gitignore` does not exist,
create it.

---

## Stage 3: Install git hooks

Install `post-checkout`, `post-merge`, and `post-rewrite` hooks into
the project's `.git/hooks/` directory. The hooks call `codegraph sync`
in the background after branch operations.

The actual logic lives in this plugin's
`bin/install-git-hooks.sh` — which is on `PATH` whenever the plugin is
enabled. The script is idempotent, uses a marker-block pattern, and
appends rather than overwriting if other hooks already exist.

1. Confirm with the user — show what the hook block will look like and
   list which three hook files will be touched.
2. On yes, run:
   ```bash
   install-git-hooks.sh "$(git rev-parse --show-toplevel)"
   ```
3. Report each hook file's outcome: `created`, `appended`, or
   `unchanged (block already present)`.

If `install-git-hooks.sh` is not on `PATH` (plugin not loaded the way we
expect), surface the diagnostic to the user — don't try to inline the
shell logic.

---

## Stage 4: Smoke test

Verify the wiring works. Each of these is best-effort; if any fail,
report which and why, don't abort the whole stage.

1. **codegraph MCP reachable**: call the `mcp__codegraph__codegraph_status`
   tool (no arguments). Expected: index reports healthy, file count
   matches what Stage 2 reported.
2. **semble MCP reachable**: call `mcp__semble__search` with a generic
   probe query (e.g. `query="main entry point"`, `repo` = the project
   root). Expected: returns at least one hit, or an empty list with a
   2xx status — not a connection error.
3. **LSP reachable** (only if a language LSP was installed): call
   `LSP.documentSymbol` on one source file (pick any small `.java` /
   `.rs` file under the project). Expected: non-empty symbol list.

If any MCP call fails because the server isn't running, surface the
likely cause:
> The plugin's `.mcp.json` is read at plugin-load time. If you just
> finished installing the binaries, run `/reload-plugins` (or restart
> Claude Code) and re-run this stage.

---

## Stage 5: Summary + restart guidance

Print a final report covering:

1. **Installed** — what was installed in this run (skip nothing — list
   even the `already present` items so the user has a complete record).
2. **Indexed** — file/symbol count for the codegraph index.
3. **Hooks** — which of `post-checkout` / `post-merge` / `post-rewrite`
   were created vs. appended vs. unchanged.
4. **Smoke test** — pass / fail per check from Stage 4.
5. **What's next** — the critical instruction, in bold:

> **Restart Claude Code** (or run `/reload-plugins`) so the MCP and
> LSP declarations in `.mcp.json` / `.lsp.json` take effect. Without
> this step, the agents' tool calls to `codegraph_*` and `semble.*`
> will fail.

6. **Uninstall** — point at the project README for manual uninstall
   instructions (strip the marker block from each hook;
   `codegraph uninit`; optionally `npm uninstall -g @colbymchenry/codegraph`).
