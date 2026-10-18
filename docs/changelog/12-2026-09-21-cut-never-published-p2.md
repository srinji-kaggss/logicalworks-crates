# 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above (2/13)

Release: 2026-09-21 cut, never published; ships as lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 above. Page 2 of 13. [Changelog index](../../CHANGELOG.md)

### Decision (continued)

- **`lgwks_bot` is relicensed to MPL-2.0**, from Apache-2.0. The bot is the
  artefact the rest of the estate embeds, and a permissive licence on it lets
  anyone modify it and ship the modifications closed with no later release able
  to recover that. MPL-2.0 is file-level copyleft: use stays unrestricted
  including in proprietary products (MPL-2.0 §3.3), and only modification of the
  MPL-covered files carries the §3.2 source obligation. `lgwks_std`, `lgwks_ast`
  and `lgwks_deps` remain Apache-2.0.
- **No published version changed, and the new terms land at `lgwks_bot` 0.5.0.**
  Licences are not retroactive; every version on crates.io including `lgwks_bot`
  0.4.2 stays Apache-2.0. The release this section is part of is the first cut
  from this tree under MPL-2.0, so 0.5.0 is the version the terms take effect at.
  The four dependent repositories pin exact published versions and none is
  affected until it moves.
- **The repository root now carries the MPL-2.0 text.** There was no root
  `LICENSE`, so the repository's own licence was unstated while the bot's was
  not, and a reader arriving at the tree rather than at a crate had nothing to
  read. The root file covers the documentation, the scripts and the CI
  configuration; each crate is still governed by the `LICENSE` in its own
  directory, so `lgwks_std`, `lgwks_ast` and `lgwks_deps` remain Apache-2.0 as
  consumed artefacts. `LICENSING.md` states which file governs what.
- **`LICENSING.md` records the model**, including the commercial licence that
  grants relief from §3.2 for organisations that need to modify the bot without
  publishing those modifications. Offering two licences requires holding rights
  in every contribution, so a contributor licence agreement has to be in place
  before a non-trivial `lgwks_bot` contribution can be merged.
- **`lgwks_bot` is closed to outside contributions, and the contributor licence
  agreement that would reopen it is deferred.** A patch sent today has no path
  to merge, so `CONTRIBUTING.md` and `LICENSING.md` now say *closed* rather than
  *not merged until an agreement exists*. The earlier phrasing left a
  contributor to send a patch that would wait on a decision no one has made; the
  block is stated as a refusal on arrival instead. It lifts when an instrument is
  chosen and recorded in `LICENSING.md`. The other three crates are unaffected.
- **A capability refusal states the whole shortfall, in real time, and derives
  its own repair** (owner direction, 2026-09-21). The check returned at the first
  ungranted capability, so repairing a bot was a loop: grant what you were told,
  rebuild, be told the next word. Each individual refusal was true and the
  sequence was still the wrong instrument, because the gate had already computed
  the whole difference — the required set minus the granted set — and reported
  only its head. `Deficit` is that difference reported in full, `Shortage` names
  the domain that declared each requirement, and `Deficit::to_grant_set` turns
  the diagnostic back into the grant set that closes it, so the repair is derived
  rather than transcribed. This is the shape the owner asked for explicitly:
  isolate what is missing at the point of the attempt, hand back the missing
  pieces, and continue — not a build loop that refuses one word at a time.
- **A run-time capability hold was designed, built, and then removed as
  unreachable.** The step after a total refusal is the natural one: deny an
  action at run time, hold the entry, let the caller supply the capability, and
  continue from that point rather than restarting the chain. It cannot be
  reached, and the reason is structural rather than incidental. `assemble` admits
  every declared requirement before the world exists, and every per-call proof is
  minted from **the same list** — `run_any` issues
  `grants.issue(self.0.required_caps())` and the action checks
  `call.0.check(self.required_caps())` — so the declaration and the check cannot
  disagree, and nothing narrows `Grants` after `assemble`. `Auth::check` cannot
  fail inside a running bot. The hold was removed rather than shipped behind a
  contrived test, and the finding is recorded in
  `docs/guides/lgwks-bot/authority.md` with its line citations.
- **The gap the hold was reaching for is the opposite one, and it is real: an
  action that declares `&[]` and performs a side effect passes the gate
  silently.** The gate checks the declared set and `&[]` is trivially covered, so
  "this action needs nothing" and "this action's author never said" are the same
  value and the crate cannot distinguish them. Every guarantee about authority is
  a guarantee about capabilities a domain *declares*. Closing that, or making a
  run-time hold meaningful, is a design change rather than a repair, and neither
  is made here.
- **Four `lgwks_bot` questions were put to the project owner on 2026-09-21 and
  decided.** They are recorded here because each was previously stated in a
  first-party document as *open*, and those documents now say what was decided.

  - **The scheduler is a first-party `lgwks_std` module**, not zed's vendored
    `scheduler` crate. `docs/bot-on-ecs.md` §9 and §10 reserved the choice,
    because the doctrine and the instruction to reuse existing open source point
    at different rungs. It was taken against a finding recorded in full in §9:
    the tick is already deterministic by construction, so a seeded scheduler has
    no caller until the recorder lands. One consequence is named there too — a
    seeded PRNG needs a carve-out from INV-RANDOM-ONE-SOURCE, argued in the
    module rather than assumed at the call site.
  - **The `Time<Virtual>` clock root (§10 step 4) is closed as already
    satisfied.** Two time layers exist and both are tested: `lgwks_std::time`
    (RFC 3339 and calendar arithmetic, INV-TIME-PURE) and `lgwks_bot::rt::time`
    (`sleep`, `timeout`, `interval`, `Instant`). No `Clock` trait or mock time
    source exists anywhere in the workspace, so the two `Instant::now()` reads in
    `rt::supervise` can still only be tested by genuinely waiting. That is
    accepted and stated, not overlooked.
  - **The `from_spec` gap is a gap, and the registry is scheduled work.** The
    crate doc argued that the absent materializer was a deliberate design
    position; that reading was rejected. `experience/invariants/sdk.yaml` records
    the adjudication. Two constraints bind the implementation, because they are
    what made the absence defensible: grants keep coming from a caller-held
    `GrantSet` and never from the spec, so wire data cannot choose what a bot
    reaches, and there is still no `bot!` proc-macro.
  - **No release is cut yet.** Five public modules (`session`, `language`,
    `semantic`, `interface`, `frontier`) and the `lgwks_bot` MPL-2.0 relicense
    are on `main` and unpublished. They ship in one release once the registry and
    the scheduler have landed.

### lgwks_std Added

- **`wire` re-exports the `rkyv` crate, so a consumer crate can derive against
  it.** The module re-exported the three derive macros but not the crate those
  macros expand against, and the generated code names `::rkyv::…` absolutely. A
  type outside `lgwks_std` that derived `Archive` through this module therefore
  failed with `cannot find 'rkyv' in the crate root` before it ever reached a
  layout question, which made the estate's binary surface usable only by its own
  tests. A consumer now writes `#[rkyv(crate = lgwks_std::wire::rkyv)]` and
  derives the macros from here as before.
- **`Digest` carries the archive derives behind `wire`.** Thirty-two bytes with
  no indirection, so it archives in place and any record that contains one
  reaches it without a pointer — which is the property that makes the journal's
  chain head a value a reader can compare against without decoding the record.
