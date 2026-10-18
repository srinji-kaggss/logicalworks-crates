# 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above (4/13)

Release: 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above. Page 4 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

- **The `domain_id -> constructor` registry exists.** A `BotSpec` carries
  identifiers rather than code, and something has to turn `"github::pr_status"`
  back into a running `Observe`. `DomainRegistry` is that list, and `domains!`
  is the one entry point that declares it:
  ```rust
  domains! {
      pub DOMAINS {
          observe { "github::pr_status" => GithubPrStatus::from_target }
          execute { "notify::slack"     => SlackNotify::from_target }
      }
  }
  ```
  The list is a `static` built at compile time — no constructor to call, no
  global to mutate, so two components cannot race to register a domain and the
  set a binary can run is readable from its source. `Source` and `Action` are
  the erased handles a constructor returns, and `build_source`/`build_action`
  refuse an unlisted identifier with `BotError::UnregisteredDomain`, escaped
  because the name came from the document.
  - **A declarative list, not a proc macro.** The mapping is data a reader can
    see whole. `domains!` is `macro_rules!`, so this adds no dependency and
    leaves the repo's stated position on the `syn` stack untouched —
    `lgwks-std/Cargo.toml` records that policy, and it still holds. A `bot!`
    proc-macro remains deliberately absent for the second reason it always had:
    it would hide the per-call `Auth::check` that auditors read.
  - **The registry is not a permission.** It answers which constructor an
    identifier names, never what a bot may reach. Authority still comes from the
    caller-held `GrantSet`, so a spec cannot grant itself anything by naming a
    domain.
  - **`from_spec` is still open, and now for stated reasons.** It is not
    plumbing: `ChainSpec::on` is `Vec<(String, ActionSpec)>`, so a condition is a
    bare identifier with no slot for `Above<u16>`'s threshold, and an erased
    `Box<dyn ObserveAny>` carries no serializable statement of its `Output` type
    (`spec::Witness` is a `TypeId`, process-local). Both are recorded in
    `experience/invariants/sdk.yaml`, which gains a `registry` lane.
  - `crates/lgwks-bot/tests/registry.rs` enforces the above, including that a
    source identifier never resolves as an action, and
    `docs/guides/lgwks-bot/domains.md` is the guide.

- **A flow can be written in RON, as well as JSON.** `FlowSpec::from_ron` and
  `FlowSpec::to_ron` are the codec, over the same serde types, so no second set
  of impls exists. RON is the estate's notation for human-facing documents and a
  flow is one, so a hand-written flow now takes comments, trailing commas and
  unquoted keys.
  - **The two notations are one document.** Both parsers converge on one
    validation, so the `MAX_FLOW_BYTES` size limit, the
    `BotError::UnknownNodeKind` diagnostic and the structural checks are
    identical on either path. A document cannot be acceptable in one notation and
    refused in the other.
  - **A variant is spelled with its own name.** Every enum in a flow document is
    externally tagged, so a unit variant is its own name (`end`) and a variant
    carrying fields takes them in parentheses (`say(text: "hello")`); in JSON the
    same two are the bare string `"end"` and `{"say": {"text": "hello"}}`. This
    is a change to the wire spelling rather than to the notation: the seven enums
    a document carries (`NodeKind`, `FlowEdge`, `Terminal`, `Predicate`,
    `ValueExpr`, `Value`, `VarType`) previously took a `kind` field, and a
    document written in the older spelling is now refused rather than
    reinterpreted.
  - **The unknown-kind diagnostic reads differently on each path.** JSON checks a
    variant name against nothing, so `BotError::UnknownNodeKind` names the node
    and the kind it carried; RON checks the name against the variant list it is
    handed and refuses before our own visitor runs, so its refusal names the
    variant and the enum but cannot name the node. Both refuse; only what each
    can say differs.
  - `crates/lgwks-bot/tests/flow_ron.rs` pins all of the above, including that
    the internally tagged spelling no longer decodes.
