# lgwks_bot: non-goals and next steps

Pages of this guide: [overview](index.md) · [non-goals and next steps](index-scope.md)

## Non-goals

These are absent, and the absence is stated in the source rather than left for
you to discover.

- **No model client.** The crate runs your logic and gates its authority. It
  does not call a model, hold a prompt, or know what an LLM is.
- **No sandbox.** Capability gating "stops confused-deputy calls and accidental
  ungated use; it is not a sandbox, since in-process code can always dial out
  directly" (`crates/lgwks-bot/src/cap.rs:15`). See [authority](authority.md).
- **No revocable authority.** `GrantSet` has no `revoke`, and a built bot holds
  a clone of the set it was admitted with. See [authority](authority.md).
- **A spec is a document, not the DSL.** `Bot::from_spec` materializes a
  validated `BotSpec` against a `DomainRegistry` — see [domains](domains.md) —
  through the same `assemble`/`build` path a native bot uses. It is
  all-or-nothing and reports every presently knowable unmet need in one
  attributed `NeedSet`; authority still comes only from the `GrantSet` the
  caller holds. A condition is named from a closed wire vocabulary (`changed`,
  `always`, `threshold::above(<n>)`, `threshold::below(<n>)`); anything the
  document cannot express stays on the builder chain.
- **No `#[tokio::main]` equivalent.** Entry to the async surface is
  `Runtime::block_on`.
- **No exactly-once delivery.** An action that may have taken effect after its
  request was sent fails as `BotError::EffectIndeterminate`, and reconciling
  that is your decision. See [failures](failures.md). The durable dispatch and
  recovery kernel behind that boundary is described for implementers in
  [`docs/effect-kernel.md`](../../effect-kernel.md).

## Where to go next

| Page | Answers |
|---|---|
| [Getting started](getting-started.md) | One source, one condition, one effect, compiled and run |
| [Features](features.md) | Every feature, what it adds, and what it does not |
| [Authority](authority.md) | Admission, the `Auth` snapshot, and the no-revocation boundary |
| [Failures](failures.md) | Poll failure, partial action failure, unknown effect |
| [Background work](background-work.md) | Bounded fan-out, supervision, and the cancellation limits |
| [Migrating to 1.0.0](migrating-to-1-0.md) | What `spawn`, `JoinHandle`, `Child` and `Command` became, and the traps the compiler cannot show |
| [Sessions](sessions.md) | Unreleased. Validated guidance flows |
| [Resolution](resolution.md) | Unreleased. Lexical and semantic resolution, and what `Degraded` means |
| [Domains](domains.md) | Unreleased. The `domain_id -> constructor` registry a spec's identifiers resolve through |
