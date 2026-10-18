# 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above (8/13)

Release: 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above. Page 8 of 13. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

- **A decision carries its provenance, and its receipt is written before the
  transition.** `Resolver::resolve` returns `Verdict`: the `Resolution` and its
  `Provenance` in one value, because "this score came from that model under that
  rule" is one fact and a second call can only re-derive it. `Provenance` names a
  `PolicyVersion` always — content-addressed over the tier's own parameters, so
  retuning changes the revision — and an `EmbedderIdentity` only when a model was
  consulted. `Session` writes a versioned, serializable `DecisionReceipt` per
  decision through a required
  `Journal::record_decision -> Result<ReceiptAcceptance, JournalError>`, binding
  session and flow revision, node, the digest of the *ordered* option list, the
  verdict verbatim, provenance, the selected option's **text** rather than an
  index that moves, and the route. Because the receipt is written first, a
  refused write aborts the answer with `BotError::ReceiptNotRecorded` and leaves
  cursor, scope, transcript and receipt list untouched.


- **`Deficit`, `Shortage` and `Demand`: the whole capability shortfall, and the
  repair it derives.** `Auth::check` and `GrantSet::admit` return
  `BotError::CapabilityDenied` naming **every** ungranted requirement rather than
  the first, each `Shortage` carrying the `Demand` — the domain that declared it
  — where the check site knows it. `Bot::build` walks every source and every
  action and reports the whole bot's unmet requirements from one pass. New
  methods: `Auth::uncovered`, `Auth::covers_cap`, `GrantSet::uncovered`,
  `GrantSet::grants`, `Deficit::shortages/len/is_empty/first/to_grant_set`.
- **`Deficit::to_grant_set` derives the repair.** The shortfall already names
  every capability that would close it, so a caller hands back the set the
  deficit derived instead of writing a repair from the message — the step where a
  hand-written repair covers the first line and misses the rest. The repair is
  the *shortfall*, not the requirement: an already-granted capability is not in
  it, and closing the requirement is that set folded into the held one.
- **`Cap` names are `Cow<'static, str>`.** The shipped four were `String`-backed,
  so `Cap::net()` allocated and — because `GrantSet::issue` mints a proof by
  copying the requirement list — every effect execution re-allocated the same
  constants. `Cow::Borrowed` makes a shipped capability a pointer copy and
  authority for it allocation-free. Equality, ordering and hashing compare
  contents, so a `Cap` deserialized from a spec and one built from a constant are
  one capability; a test asserts that in both directions, because if they
  compared unequal the gate would deny a bot it had granted.
- **`Auth::check` is logarithmic in the granted set.** The covered set is sorted
  and de-duplicated at mint and queried by binary search, replacing a linear scan
  inside a loop over the requirement list. The measured defect and its numbers
  are in `bench/README.md`: 12.9 microseconds for 128 capabilities against 3.4
  nanoseconds for one, growing with the *product* of the two counts.

### lgwks_bot Fixed
