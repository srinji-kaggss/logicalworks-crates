# The registry: what a spec's identifiers resolve to

> **Needs `lgwks_bot` 0.5.0 or later.** `DomainRegistry`, `Source`, `Action` and `domains!` first shipped in the
> `lgwks_bot-v0.5.0` tag; the `lgwks_bot-v0.4.2` tag does not export them.

A `BotSpec` carries identifiers, not code. `ChainSpec::source` is a string such
as `"github::pr_status"`, `ActionSpec::domain` is a string such as
`"notify::slack"`, and the `target` beside each is a parameter whose meaning the
domain itself defines — `"owner/repo"`, `"#deploys"`. Something has to turn
those strings back into a running `Observe` or `Execute`. That is
`DomainRegistry`.

## One list, in one place

A registry is declared once with `domains!`, and that is the only entry point:

```rust
use lgwks_bot::{Auth, BotError, Cap, Observe, Source, domains};

/// A source that reports an open pull request count.
struct GithubPrStatus;

impl GithubPrStatus {
    /// Build one from the `target` its spec names.
    fn from_target(target: &str) -> Result<Source, BotError> {
        let _repo = target;
        Ok(Source::new(Self))
    }
}

impl Observe for GithubPrStatus {
    type Output = u16;
    fn required_caps(&self) -> &[Cap] { &[] }
    async fn poll(&self, call: (Auth, ())) -> Result<u16, BotError> {
        call.0.check(&[])?;
        Ok(0)
    }
    fn domain_id(&self) -> &str { "github::pr_status" }
}

domains! {
    /// The domains this bot can run.
    pub DOMAINS {
        observe {
            "github::pr_status" => GithubPrStatus::from_target,
        }
        execute {}
    }
}
```

The list is a `static` built at compile time. There is no constructor to call and
no global to mutate, so two components cannot race to register a domain, and the
set of domains a binary can run is readable from its source rather than from its
behaviour. Adding an adapter is one line in that block plus its constructor.

The two halves are separate because a source and an action are built into
different erased traits. An empty half is written `{}` and is not an error: a bot
that only observes is a bot.

## An identifier is not a permission

The registry answers *which constructor* an identifier names. It never decides
what a bot may reach. Authority comes from the `GrantSet` the caller holds, and
every erased verb checks its own caps at the call site — `call.0.check(&[])` in
the example above. A spec therefore cannot grant itself anything by naming a
domain, which is what makes accepting a spec from wire data safe at all.

## When the identifier is not in the list

`DomainRegistry::build_source` and `build_action` return
`BotError::UnregisteredDomain` carrying the identifier **as the document spelled
it**, escaped so a hostile spec cannot forge a log line with a newline in a
domain name. This is a mismatch between the document and the program it was
handed to, not a runtime failure: the domain may exist and be registered
elsewhere, and no amount of retrying adds it.

A constructor's own refusal surfaces unchanged. A domain that cannot parse its
`target` returns its own typed error from `build_source`, and the registry passes
it through rather than wrapping it — the domain is the only thing that knows what
its target means.

The two lists are looked up separately, so a source identifier never resolves as
an action and an action identifier never resolves as a source. That is what keeps
a read from becoming a write when a spec is written carelessly.

## The materializer

`Bot::from_spec(spec, registry, grants, effects)` walks a `BotSpec` against this
registry and produces a runnable `Bot`, through the **same** `assemble`/`build`
path a native bot uses. The two obstacles that once stood in front of it are
closed, and how they are closed is worth knowing because it fixes the shape of
the wire contract:

- **A condition carries its parameters in its identifier.** `ChainSpec::on` is
  still `Vec<(String, ActionSpec)>`, and the identifier is what names the
  condition: `always`, `changed`, `threshold::above(5)`, `threshold::below(5)`.
  The vocabulary is closed and resolved by the source itself, against the type
  it actually produces.
- **A source states its own durable type.** `Source::new` captures the output's
  `InputIdentity::SCHEMA_ID` — a declared, versioned string, not a process-local
  `TypeId` — alongside the change filter and the admitted-input identity, and it
  is the source that answers `condition(identifier)` for its own output type.
  A condition therefore resolves against the type it will really see.

Because admission is all-or-nothing, a document with any unmet need is refused
with one attributed `NeedSet`: an unknown source or action domain, a constructor
that rejects its target, a missing capability, or an unknown condition. Nothing
is polled and nothing is built. `experience/invariants/sdk.yaml` records the
landed materializer. `crates/lgwks-bot/tests/registry.rs` and
`crates/lgwks-bot/tests/spec_materialize.rs` enforce what the registry and the
materializer claim.
