# The registry: what a spec's identifiers resolve to

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

## What is not here yet

`BotSpec::from_spec` — the materializer that would walk a spec against a registry
and produce a runnable `Bot` — does not exist. Two obstacles stand in front of it,
and neither is plumbing:

- **A condition has nowhere to put its parameters.** `ChainSpec::on` is
  `Vec<(String, ActionSpec)>`, so a condition is a bare identifier. `Above<u16>`
  needs a threshold, and the wire format has no slot for one. Expressing the
  shipped conditions therefore needs a change to the spec format itself.
- **An erased source states no durable type.** Once a source is a
  `Box<dyn ObserveAny>`, nothing serializable says what its `Output` was.
  `spec::Witness` is a `TypeId`, which is process-local, so a condition
  constructor resolved from a document has no key to check against. Such a key
  would be declared in this registry, and is not.

These are recorded as open in `experience/invariants/sdk.yaml` rather than left
to be rediscovered. `crates/lgwks-bot/tests/registry.rs` enforces what the
registry does claim.
