# 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above (7/13)

Release: 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above. Page 7 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

- **`Observe::fingerprint`, and with it the lazy seam: a source that holds still
  is no longer polled.** Change detection is an *equality* question — the
  substrate reduces every observation to one bit and discards the value — so
  making a source produce the value in order to ask the bit is the eager part of
  the loop. `fn fingerprint(&self) -> Option<u128>` lets a source answer the
  equality question from a key it already holds (an `ETag`, an `mtime`, a row
  version, a mutation counter, a hash of a framebuffer), and the tick then
  **never calls `poll`**: no value is constructed, nothing is boxed, and no
  future is built. The decision is taken *before* the future exists — a check
  inside the future would still have allocated the box carrying it, which on a
  quiet tick is the only thing there was to allocate.
  - **The contract is exact: equal fingerprints must imply equal values.** A
    digest that collides across two different values makes the substrate skip a
    real movement, which is a silent no-op rather than a wrong effect — the one
    failure this can introduce. The digest is recorded at the moment it is read,
    *before* the poll, so a source that moves in between leaves a digest that no
    longer matches and the next tick polls again: the error is one redundant
    poll, never a missed movement. A poll that fails clears the digest, so a
    source that errors still reports every tick, exactly as before.
  - **The default is `None`, which is today's path exactly** — every source
    written before this method existed keeps its behaviour and its cost, and
    there is no configuration in which a domain silently loses an observation.
  - Measured, `bench/` A/B/A in one session, seam versus the tick-allocations
    change beneath it: `poll-only-64x100` **1.65x faster**, `steady-64x100`
    **1.68x**, `wide-256x10` **1.46x**. `fanout-1x64` and `churn-64x1` are
    unchanged: their moves (1.8% and 7.6%) are smaller than the control's own
    drift at the same scenario (6.9% and 10.8%). Allocations per tick:
    **167.6 -> 22.9** steady, **171.0 -> 21.4** poll-only, **650.9 -> 141.7**
    wide. Against the base before either change: **2.71x**, **1.99x** and
    **1.66x**.
  - The rig's own source implements it as the identity (`u128::from(value)`),
    deliberately: a real source would use a key cheaper than the value, so the
    reported gain is a **floor** and not a best case.
  - Three tests, each verified to fail against the seam removed: a quiet source
    is polled once over twenty-one ticks (it reports 21 without the seam), a
    digest that moves is polled again and fires, and a source with no digest is
    polled every tick exactly as before.
- **The tick no longer allocates its own bookkeeping, and a source that holds
  still is no longer boxed.** A steady-state tick at 64 chains made **171 heap
  allocations**; it now makes **97**, and the tick is up to **1.6x faster**.
  Four changes, all measured by the `bench/` rig rather than argued:
  - `fire_plan` cleared the plan and then assigned it freshly collected `Vec`s,
    so the `clear` bought nothing and every tick paid two allocations per chain
    to rebuild buffers it was about to overwrite. The per-tick scratch
    (`Plan::steps`, `Plan::failures`, `Polled`, `Observed`, `Moving`, `Moved`)
    is now taken, cleared and handed back, and the intermediate `Vec<usize>` of
    moved chains and the per-tick `Order::clone()` are gone.
  - **`ObserveAny::poll_any` takes the payload the substrate currently holds for
    its chain and returns `Ok(None)` when the source produces an equal value.**
    The comparison now happens where the output is still a concrete
    `T::Output`, which is the only place it can happen without boxing: a
    caller-side `==` on two `Box<dyn Any>` costs an allocation, a memcpy and a
    free apiece, spent to answer a question that could be asked of the value in
    hand — and in a steady-state bot the answer is "equal" on nearly every tick.
    The blanket `impl ObserveAny` therefore carries `T::Output: PartialEq`,
    which is not a new requirement: every chain is closed by
    `EcsObserveBuilder::observe`, which already demands it.
  - **A source whose equality is narrower than its identity now holds the
    *earlier* of two equal values.** This is a real semantic tightening and it
    is the one to read carefully. `Observed` keeps the payload it already has
    when the source reports equality, rather than being overwritten with the
    newer-but-equal instance. Nothing an effect observes changes — a retained
    transition already refuses to let an equal-valued observation displace its
    binding, so the action was receiving the older instance anyway. What changes
    is a caller reading the observation back after an equal-valued tick. A type
    whose `PartialEq` ignores a field it nevertheless carries should not be a
    chain's output type.
  - The measurement, and the per-tick allocation trace that gives it, are
    `bench/`'s `--alloc-report` mode, which counts through a forwarding
    `#[global_allocator]` in that rig only. It is a diagnostic: no timing in
    `results.json` is taken from a counting run, and no `unsafe` reaches
    `lgwks_bot` or `lgwks_std`.
- **`ObserveBuilder` is now generic over its source, and a chain that does not
  type against that source no longer builds.** `EcsObserveBuilder::on` tied the
  condition to a *free* type parameter connected to nothing else, so the builder
  accepted a wiring that could not work and the tick found out instead: a `u16`
  source, a `u16` condition, and an action whose `Execute::Input` was `u32`
  compiled, ran, and failed every attempt as a domain error the retry policy
  then retried. `on<C, A, T>` is now `on<C, A>` with `C: Evaluate<S::Output>`
  and `A: Execute<Input = S::Output>`, and the builder holds `source: S` until
  the erasure boundary. **This is a deliberate source-breaking tightening**, not
  an accident of the rewrite:
  `Bot::builder("x").observe(source).on(condition, action)` now fails to compile
  when `condition`'s `Evaluate<T>` is not `Evaluate<S::Output>` or `action`'s
  `Execute::Input` is not `S::Output`. Both rejected shapes only ever failed at
  tick time, so nothing that worked stops working — but code that compiled and
  misbehaved now does not compile at all, and a caller who was relying on the
  old looseness must fix the wiring rather than the budget. `observe`/`build`
  remain the erasure boundary and still box into `Box<dyn ObserveAny>`. Sound
  because `Evaluate::check` returns `bool` — a pure predicate with no derived
  output — so `Source::Output == Condition::Input == Action::Input` is already
  the real semantics. The rule a future verb has to keep: no stage may introduce
  a caller-selected type parameter disconnected from its input; a transform must
  carry `Transform<I>::Output` as an associated type. Every `.on` call site in
  the estate already annotated its closure or named its condition type, so no
  call site needed a turbofish added.
- **The erasure boundary carries a witness, and a mis-pairing across it is a
  loud invariant violation rather than a domain error.** `Observed` was a
  `Vec<Option<Box<dyn Any>>>` paired to `Chains` by *index*, and nothing about
  that pairing was checked, so a value delivered to the wrong chain produced a
  downcast miss reported as a domain failure — which the retry policy then
  retried. The staging slot now holds an `Erased` — the boxed value and the
  `TypeId` witness of the type it was erased from, one allocation rather than
  two — and the witness is compared at the rendezvous before any downcast. A
  miss is `BotError::TypeMismatch`, naming the site, the chain, and both types.
  The witness rides with the value rather than only on the chain, so the
  comparison is a producer's *claim* against a consumer's *expectation* rather
  than ground truth against an expectation. `TypeId` is process-local and not
  serializable, so the witness serves the Rust path only; the durable schema key
  belongs to the registry, and `Erased::witness` is the field it will land in.
  One limitation is documented and not solved here: the witness distinguishes
  *types*, not *chains*, so two chains that both produce a `u16` are
  indistinguishable and a mis-pairing between them still passes.
