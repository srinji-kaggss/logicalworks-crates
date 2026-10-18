# lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 - 2026-09-28 (5/5)

Release: lgwks_std 0.7.0 / lgwks_bot 0.5.0 / lgwks_deps 0.1.13 - 2026-09-28. Page 5 of 5. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Fixed (continued)

- Durable outcome settlement binds a receipt to the exact `OutcomeObserved`
  position before releasing work. An unrelated or forged receipt holds
  `RecordingFailed` and retries recording only; it never re-enters the action
  (`tests/durable_dispatch.rs::an_unrelated_outcome_receipt_is_refused_without_reentering_the_effect`).
- A ladder-complete `OutOfOrder` is idempotent success only when the *same*
  evidence is already recorded. `ensure_outcome` used to treat any such refusal
  as success without looking, so a contradictory `Applied` could be
  acknowledged. `RecordingFailed` retries now go through the same
  `ensure_outcome` write, which is what makes an ambiguous commit-then-error
  retry land rather than loop.
- `classify_settlement` checks the environment half of ownership identity, not
  only run and flow. A key cloned from the pending key with a foreign
  environment used to settle the live entry and journal the foreign key.

### lgwks_bot Documentation

- Five citations in `docs/guides/lgwks-bot/` were re-anchored to the lines this
  change moved in `ecs.rs`. `getting-started.md` also credited the
  `Observe::Output: PartialEq` requirement to `EcsBuilder::observe`, which
  carries no such bound; it lands on `EcsObserveBuilder::observe`, the call that
  closes a chain, and the citation now points at the bound.
- The `ephemeral` scope grew `ecs.rs` by 113 lines and `spec.rs` by four, and the
  guides cite both by line, so all twenty-four citations under them landed on
  whatever now occupied the number. Nine were caught, having resolved to a
  closing brace, a blank line or an empty `///`. The other fifteen resolved to a
  plausible line that was not the one cited, which is the class the checker
  deliberately does not judge. All twenty-four were re-anchored to the line they
  named before the growth, matched by the text of that line rather than by the
  numbers the offset would predict; the five whose text is not unique in the
  file were confirmed by reading them.

### lgwks_deps Added

- `lgwks-deps debug [PATH] [--json]`, a cargo-doctor-style command for the
  `lgwks_std::trace` lifecycle. It installs the default debugger in its own
  process, emits lifecycle events, inspects `crates/lgwks-std/Cargo.toml`, and
  refuses if the SDK debugger no longer rides the default `trace` feature.

### Repository Added

- `scripts/check-std-first.py`, and a CI step that runs it. `lgwks-deps check`
  enforces the manifest half of the `std`-first rule and cannot see the other
  half, which lives in the source: a `use` of a crate no manifest declares
  (buildable only because something else in the graph re-exports it), and a
  capability hand-rolled beside the `lgwks_std` module that already provides it.
  Every crate reached past `std` and the four surfaces is reported with the
  approval record behind it — owner, capability and the reason the approver
  wrote — so `--justify` prints the answer to "why is this edge here" instead of
  leaving it to a reviewer's memory.

  It carries four written exemptions, each pinning the exempted line's text as
  well as its number, because a `path:line` key alone would silently cover
  whatever later occupied that line. A moved exemption is a `STALE EXEMPTION`
  finding, not a silent yes.


### Repository Changed

- **CI runs the matrix once per change instead of twice.** `on.push` is scoped
  to `main` now, because a pull request already runs all eleven jobs for its head
  commit and the branch push ran every one of them again for the same tree. A
  `concurrency` group supersedes a run on a branch that has been pushed again,
  and never supersedes a run on `main`.
- **The rustdoc gate is runnable outside CI.** `scripts/doc-lanes.sh` holds the
  four lanes the Docs job runs, and the job, `AGENTS.md` and `docs/releasing.md`
  all call it instead of restating the commands. Which links break depends on which features are on,
  so the gate is four lanes and not one command, and none of them was reachable
  from the `Checks that must pass` list, which named no doc build at all: a
  broken intra-doc link is a warning rather than an error, so the crate built
  green and the break surfaced only after the push.

### Repository Changed

- **A citation that moves is now a failure rather than a silent pass.**
  `scripts/check-doc-citations.py` pins the text of every cited line in
  `scripts/doc-citations.lock`, so a citation whose line no longer holds what it
  was pinned to fails the Docs job. The check before this one rejected only a
  citation that landed on a bare delimiter, which is a floor: on 2026-09-21
  `ecs.rs` grew by 1,070 lines, and 22 of the 25 citations under it kept
  resolving onto unrelated code while CI reported green. Demonstrated on a copy
  of this tree with `cap.rs` grown by one line: the previous check exits 0 with
  "107 citations resolve", this one exits 1 and names all seven that moved.
  `--update` rewrites the lock and prints every line whose text changed under a
  citation and which page cites it, so the review is the diff.

### Repository Fixed

- `crates/lgwks-bot/src/domain/mod.rs` is deleted. It declared the nine domain
  modules, but `lib.rs` declares `domain` as an inline module, so that file was
  never in the module tree: the build's dep-info lists every `src/domain/*.rs`
  and not it. It was a second copy of the module list that nothing compiled and
  nothing kept in step.
