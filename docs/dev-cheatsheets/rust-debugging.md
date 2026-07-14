# Rust Debugging Cheatsheet

Scientific-method data-flow diagnosis for Rust. Portable across projects.

## Iron law

No code change without a confirmed root cause. If you feel the urge to
"try a fix and see," stop: form a hypothesis, run an experiment,
observe evidence, then propose a change.

## Phase 1 — Observation

Collect raw facts before interpreting.

**Locating the code**: if you know the behavior under test but not the
symbol implementing it, start with `semble.search` (pass `repo` = project
root, e.g. `/workspace`), then hand the hit to `codegraph_explore` for
structure and callers. Known symbol/relationship → codegraph directly;
literal string → shell `grep` last.

**Compiler errors**: capture exact codes (`E0308`, `E0277`, `E0495`,
`E0382`), every span, every `note:` and `help:` (they contain rustc's
reasoning), and the expected-vs-found types (including lifetimes).

**Runtime bugs**: capture what the spec/test/doc requires, what the
actual output is, and the precise delta.

**Data-flow chain**: trace source → sink as a numbered list of
transformations.
```
[1] input: Request<Body>          — handler param
[2] body_bytes: Bytes             — hyper::body::to_bytes
[3] raw: &[u8]                    — body_bytes.as_ref()
[4] parsed: serde_json::Value     — from_slice(raw)
[5] order: Order                  — Order::try_from(parsed)  ← ⚠️
[6] validated: ValidatedOrder     — order.validate()
[7] output: Response<Body>        — response_from(validated)
```
For each step, state the specified input type, output type, and
invariants; cite where the spec lives (doc comment, trait bound, test).

## Phase 2 — Hypotheses

Generate 3–5 competing explanations. Each must be specific, testable,
falsifiable.

```
### H[N]: <one-line description>
Suspect step:   <index from the data-flow chain>
Mechanism:      <trait resolution / lifetime elision / implicit
                coercion / wrong From impl / generic instantiation /
                type-parameter mismatch / ...>
Prediction:     If correct, then <observable consequence not yet
                checked>.
Falsification:  Wrong if <specific observation>.
Test:           <exact command / assertion / code inspection>
Prior:          HIGH / MEDIUM / LOW
```

## Phase 3 — Experiments (lightweight first)

Test hypotheses in prior-probability order. Prefer the cheapest test
that can confirm or refute.

**Type-annotation test** — cheapest. Force an explicit type at the
suspect step; does the error change?
```rust
let parsed: serde_json::Value = serde_json::from_slice(raw)?;
```

**UFCS disambiguation** — force a specific trait impl.
```rust
let order = <Order as TryFrom<serde_json::Value>>::try_from(parsed)?;
```

**Isolation test** — extract the suspect transformation into a
standalone function with explicit input/output types. If it compiles
in isolation but not in context, the problem is lifetimes / generic
instantiation, not the transformation itself.

**Assertion test** — for runtime bugs, `assert_eq!` or `dbg!()` at the
suspect step.
```rust
let intermediate = transform(input);
dbg!(&intermediate);
assert_eq!(intermediate.field, expected, "spec violation at step N");
```

**Minimal reproduction** — smallest `fn main()` or `#[test]` that
exercises ONLY the suspect step with hardcoded input.

**Record every experiment**:
```
### E[N] — testing H[M]
Action:         <what you did>
Raw output:     <exact compiler / runtime output>
Interpretation: <what this says about H[M]>
Verdict:        CONFIRMED / REFUTED / INCONCLUSIVE
```

After each experiment, mark refuted hypotheses ELIMINATED. If all
refuted, return to Phase 2 with new candidates informed by the
evidence.

## Phase 4 — Root cause

```
Broken step:   <index>
Mechanism:     <precise technical explanation>
Why wrong:     <spec reference>
Why happened:  <how the code came to be in this state — refactor
                incomplete? upstream type changed? missing trait impl?>
Evidence:      <experiments confirming + ones eliminating alternatives>
```

## Phase 5 — Prescription (minimal fix)

1. State the fix hypothesis: "If we `<change>`, then `<outcome>`,
   because `<mechanism>`."
2. Show the minimal diff (>10 lines → question whether you're fixing
   the root cause or a symptom).
3. Predict side effects (other transformation steps? public API?).
4. Verify: `cargo check`, `cargo clippy -- -D warnings`, `cargo test`.
5. **Cleanup**: remove every `dbg!()`, diagnostic assertion, temporary
   type annotation. Check via the Bash tool with
   `grep -rn "dbg!" src/` and `grep -rn "// DIAGNOSTIC" src/` — Claude
   Code v2.1.117+ removed the standalone `Grep`/`Glob` tools on native
   macOS/Linux builds; shell `grep` (invoked through `Bash`) is the
   canonical replacement.

## Architectural stop rule

If 3+ hypotheses tested and all refuted, OR the root cause traces to a
type-design decision (wrong trait hierarchy, wrong ownership model,
wrong error-type structure), STOP and escalate to the architect with:
- Description of the design-level problem.
- Affected modules and types.
- Recommended structural change.
- Scope estimate (small / medium / large).

Do NOT try to patch a design bug locally.

## Report shape

```markdown
# Diagnostic: <one-line symptom>

## Observation
<Phase 1 findings>

## Hypotheses
<Phase 2, all with full template>

## Experiments
<Phase 3, each E[N] fully recorded>

## Diagnosis
<Phase 4 root cause>

## Prescription
<Phase 5 minimal fix + verification, OR architectural stop-rule report>

## Prevention
- Type-level: <newtype wrapper / trait bound / enum-instead-of-string>
- Test-level: <property test / type-checking assertion>
- Process-level: <explicit annotation at boundary / lint rule>
```
