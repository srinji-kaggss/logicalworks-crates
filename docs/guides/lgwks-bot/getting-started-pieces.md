# Getting started with lgwks_bot: the pieces

Pages of this guide: [manifest](getting-started.md) · [the program](getting-started-program.md) · [the pieces](getting-started-pieces.md)

## What each piece is

**The source.** `QueueDepth` implements `Observe`: it produces the value. `poll`
takes an `(Auth, ())` tuple because a read can cross a trust boundary too;
`call.0.check(self.required_caps())?` is what makes the proof load-bearing rather
than decorative. `from_target` builds the source from the name the flow gives
after `of`. The flow's `observe queue::depth of name` calls that constructor and
polls the source once, as a step of its own; `let depth: u32 =` is the type the
value is asked for, and a source whose value is not a `u32` is refused with
`BotError::TypeMismatch`.

**The action.** `Page` implements `Execute`. `execute_action` takes
`(Auth, &Self::Input)`; here `Input` is `u32` and the flow passes `depth`, a `u32`,
after `with`. `required_caps` is `bot.notify`, and the check runs before
the effect, not after it. `effect_lifetime` says the page stays in this process
(`EffectLifetime::Local`). An action that does not say so defaults to
`EffectLifetime::External`, and `act` refuses it before it runs; the program as
first written, without that method, stopped at the page with `act runs only an
action whose effect stays in this process`.

**The registry.** `domains!` declares a static table from identifier to
constructor, one table for sources (`observe`) and one for actions (`execute`).
A flow can only name what its host's table declares: an identifier the table
lacks refuses the flow before its first step, naming the registry and the nearest
declared identifier (`an_unknown_identifier_names_the_registry_and_the_nearest`
in `crates/lgwks-bot/tests/it/script_domains.rs`).

**The flow.** `flow watch(name: &str) -> u32:` is one `async fn` that takes the
tenant's `Scope` first and returns `Result<u32, FlowError>`. Each line is one
step: `observe` and `act` call the registry, `if` is Rust's `if`, and `give back`
returns from the flow. The whole vocabulary is on one page,
[the lexicon](../../script-lexicon.md).

**The host.** `Host::builder("queue-watch")` names the tenant. `.domains(..)`
installs the registry the flow resolves against, `.grants(..)` is what the run is
allowed (`GrantSet::empty().grant(Cap::notify())` allows paging and nothing
else), and `.default_deadline(..)` sets the deadline a run gets (30 seconds here).
`task("watching", ..)` wraps the flow, `host.run(&task, input)` runs it, and the
`Report` it returns holds the result.

## Reading a run

`report.result()` is `Ok` with the flow's value or `Err` with a `FlowError`. A
refusal names the step that was refused by its path: `watching/watch/page` above
is the task `watching`, the flow `watch` and the step `page`, the last segment of
`notify::page`. A run short of a grant says which: `blocked: 1 ungranted:
bot.notify`. The authority is the host's, so the same flow runs on a host with the
grant and is refused on a host without it, and nothing in the flow changes.

## Where to go next

- [Features](features.md) for which cargo features you actually need.
- [Authority](authority.md) before you design anything around revoking a grant:
  `GrantSet` has no revoke operation.
- [Failures](failures.md) before you write a retry.
- [Background work](background-work.md) if the bot needs work running beside the
  flow rather than inside it.
