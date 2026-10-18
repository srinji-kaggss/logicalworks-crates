# lgwks_bot

Pages of this README: [overview](README.md) · [execution model](README-execution.md) · [failure](README-failure.md) · [authority, domains and specs](README-authority.md) · [script!](README-script.md) · [async runtime](README-runtime.md) · [supervision](README-supervision.md)

Capability-gated automation bots.

A bot framework built on four fixed verbs (Observe, Evaluate, Execute, Query)
that run as systems on a [Bevy ECS](https://bevy.org) schedule. Three properties
distinguish it from a task runner:

- **Authority is proof-carrying.** Every effect takes an `(Auth, input)` tuple,
  and only `GrantSet::issue` mints the `Auth` half. Capabilities are checked at
  build time *and* on every call. The grants are a **snapshot**, not a live
  lease — see [Authority is a snapshot](README-authority.md#authority-is-a-snapshot).
- **A condition is change detection, not re-evaluation.** A chain fires on the
  tick its source value *moves*. A source that holds still fires nothing, and
  the framework tells you which sources moved (`bot.revisions()`).
- **A nondeterministic schedule is refused at build.** Ambiguity detection runs
  as an error, so an ordering the engine cannot fix is a build failure rather
  than a misordering discovered at 3am.

```sh
cargo add lgwks_bot
```

**Version boundary.** `crates/lgwks-bot/Cargo.toml` reads `1.0.0`, the first
stable cut. `0.8.0` is the previous tag and crates.io version, and it exports
every module this README names, `script` included. `1.0.0` changes no API
relative to `0.8.0` and moves with `lgwks_std 1.0.0`, whose `json` it
re-exports. Check `CHANGELOG.md` for the
release that carries a symbol before depending on it.

## Quick start

`a_chain_fires_once_on_the_tick_its_source_moves_past_the_condition`
(`crates/lgwks-bot/tests/it/readme_front_door.rs`) is the program for it, and it
runs under nextest with the rest of the suite. It implements one source,
`QueueDepth`, whose `poll` returns the depth the test stored (42), and one effect,
`PageOnCall`, which counts its runs; chains them
as "page when the depth is above 10"; builds the bot against
`GrantSet::empty().grant(Cap::net())` with an effect scope that names the run,
its environment and its flow revision; and ticks once. `bot.tick()` returns 1.

The run's identity is required rather than defaulted, because a bot that cannot
name its own run cannot recover against one after a restart. The test's journal
is `MemoryJournal`, which only promises `Ephemeral`, so its effect says
`EffectLifetime::Local`; a real external handoff (an email, a payment, a page)
stays on the default `EffectLifetime::External`, so a journal that cannot
outlive the process refuses it before it runs.
`tick` is the synchronous adapter, which drives the non-`Send` verb futures with
a thread-parking executor, so there is nothing to await; from inside an async
runtime, `await bot.tick_async()` instead.

`bot.tick()` returns the number of actions fired. A built bot is `mut`
because a tick advances its world.

## Relationship to tokio

It runs on tokio rather than replacing it. The async engine is
sourced from the `lgwks_deps` storefront, and `lgwks_bot` exports it as
`rt::*` so a consumer never names `tokio` directly. If what you want is
futures, timers, sockets, and channels, `rt::` gives you exactly that.

What tokio does not give you is the layer above: what to poll, when, in what
order, and with what authority. Concretely:

| You would otherwise | Here |
|---|---|
| Re-read every source each tick and diff it yourself | `Changed<Revision>` on the source entity, maintained by the schedule |
| Thread an `Auth`/permission check through every call site by hand | `(Auth, input)` on every verb; only `GrantSet::issue` mints `Auth` |
| Discover a nondeterministic system order in production | `ScheduleBuildSettings { ambiguity_detection: Error }` refuses it at `build()` |
| Define your own notion of "an action" per project | Four verbs, closed. No fifth exists |
| Hope a config file's meaning matches the code's | `BotSpec` is the serializable contract, with typed parse errors |

The honest boundary: this is **not a sandbox**. In-process code can always dial
out directly, and capability gating is auditable authority, not isolation.

Against other bot frameworks, the distinction is the same one restated: most
are either a hosted GUI workflow editor (n8n, Zapier, Make) or an actor library
with no notion of authority (ractor, actix). This one is a compiled,
capability-gated execution model with a serializable spec, in Rust.

## The four verbs

| Verb | Trait | Purpose |
|------|-------|---------|
| **Observe** | `verb::Observe` | Watch a source: poll, listen, stream. Async; produces a value each tick. |
| **Evaluate** | `verb::Evaluate<T>` | Gate on a condition. Boolean over observed state. Synchronous and pure. Closures implement this automatically. |
| **Execute** | `verb::Execute` | Perform a side effect. Async and capability-gated. The action half of the chain. |
| **Query** | `verb::Query` | Read without side effects. Async, direct call, no chain required. |

No fifth verb exists. New domains implement these four rather than adding verbs.


## License

MPL-2.0 — Copyright 2026 Logical Works Incorporated

Deliberately not the workspace's Apache-2.0. The bot is the artefact the rest of
the estate embeds, so it carries file-level copyleft while its three siblings
stay permissive. A proprietary consumer is still permitted — MPL-2.0 §3.3 — and
only modification of the MPL-covered files carries an obligation. See
[`LICENSING.md`](../../LICENSING.md).
