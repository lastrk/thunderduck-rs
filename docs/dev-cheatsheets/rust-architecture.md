# Rust Architecture Cheatsheet

Portable discipline for architecture-plan output. Read-only: produce
designs, not code. Implementation belongs to the coder role.

## Deliverable shape

Every architecture plan has these sections. Skip a section only if the
task genuinely has nothing to say there — do not pad.

```markdown
# Architecture Plan — <feature or refactor>

## Context
<Why this change is being made — problem, prompted-by, intended
outcome, links to prior discussion / diagnostic / issue.>

## Domain constraints
<Domain-level rules that the design must satisfy: spec references,
performance targets, compat requirements, invariants inherited from
elsewhere.>

## Lifecycle / data flow
<Trace the primary data through the system: source → transformations
→ sink. Numbered chain like the debugger uses, but at the type level.
State what invariants hold after each step.>

## Module / package layout
<Which modules gain new types / functions / traits. Which existing
modules change. Directory tree diff when useful. Cite the file paths
that will be touched, not the exact lines (that's the coder's job).>

## Type skeletons
<For every new public type: name, brief purpose, field list with types.
No method bodies — signatures only. Cite the trait bounds you expect
to hold. If a field has non-obvious ownership (Arc, Cow, Box<dyn>),
justify it.>

    ```rust
    /// One-line purpose. Cite the invariant it enforces.
    pub struct FooRequest {
        id: FooId,
        payload: Bytes,           // owns the wire bytes; parsed lazily
        source: Arc<dyn Source>,  // trait obj — hot-swappable per test
    }

    impl FooRequest {
        pub fn new(id: FooId, payload: Bytes, source: Arc<dyn Source>) -> Self { ... }
        pub fn parse(&self) -> Result<Foo, ParseError> { ... }
    }
    ```

## Interface / trait boundaries
<Where the seam is. What each side owns. Which side is stable, which is
allowed to evolve. If you're introducing a trait, state its object
safety, its default methods, and its expected impl count. If you're
introducing an enum for closed-set polymorphism, list the variants and
say why enum-over-trait-object is the right call here (or vice versa).>

## Concurrency / lifetime model
<For async or threaded code: which thread owns what, which locks
protect what, which channels connect what. For borrowed types: which
lifetime outlives which. If !Send/!Sync types are involved, name them
and state where they must stay.>

## Error strategy
<What errors this design can emit. Which layer wraps them. Where the
loud-fail boundary is (user input? external API?). Which layer converts
between error types via ?. Whether the error is a library-visible enum
(thiserror) or an application-only anyhow chain.>

## Testing strategy
<Which behaviors need unit tests, which need integration tests, which
need property tests. Where mocks / test doubles live. Do NOT specify
individual test names — leave that to the coder.>

## Open questions
<Anything the coder should NOT decide unilaterally. Frame each as a
question the architect + user need to answer before implementation.>
```

## Design discipline

- **Prefer enums over trait objects for closed sets.** Trait objects
  when the set is genuinely open (plugins, user-provided impls) or
  when heterogeneous storage matters. Enums when the variants are
  fixed and exhaustive matching is desirable.
- **Prefer composition over inheritance.** Rust has no inheritance;
  simulate via generics with trait bounds or via delegation. If a
  design is fighting the type system, revisit the shape.
- **Return early via `?`; avoid deeply nested Result chains** in the
  design shape.
- **Type-drive invariants**. If a value has an invariant (non-empty,
  UTC timestamp, validated), encode it in a newtype so illegal states
  are unrepresentable.
- **Ownership defaults**: struct owns its data; borrow only in fn args
  and short-lived views. `Cow` only when the caller genuinely may want
  either.
- **Concurrency defaults**: message-passing (`mpsc`) before shared
  state; `Arc<RwLock<T>>` before `Arc<Mutex<T>>` when reads dominate;
  `spawn_blocking` for any blocking call in an async context.

## Blast-radius verification (before proposing)

For every proposed API change on an existing symbol, run
`codegraph_impact` and cite the caller count. Surprises here mean the
design needs revisiting before implementation. State impact in the plan:

```
Impact of proposed rename `Foo::execute` → `Foo::run`:
- 3 call sites (all in crate::runtime); trivial rename.
- 1 impl of trait `Executor` in tests; also updates.
```

## Read-only rule

The architect does not `Write` or `Edit` source files. Deliverables are
plans and diagrams. If a plan requires a code experiment to validate,
prescribe it as an experiment for the coder, not a change to land.

## Reject partial-arm shortcuts

Every new match arm, trait impl, or enum variant introduced by the plan
must have a corpus test, integration test, or unit test that exercises
it in the coder's implementation. Dead code lands only when the arm is
part of a documented staged rollout with a follow-up plan that unblocks
it.

## Escalation

If the diagnostic (or the plan itself) traces back to a fundamental
design decision already made (wrong trait hierarchy, wrong ownership
model, wrong error taxonomy), do not paper over it. Flag it explicitly
as an ADR-level decision the user must resolve. Reference the existing
ADR set and the specific ADR that would need amendment.

## Report shape

Deliverable file lives under a project-chosen path (e.g.,
`.agent-output/architecture-<name>.md`). Return a one-paragraph summary
of the key architectural decisions in the final assistant message so
the orchestrator can decide whether to proceed.
