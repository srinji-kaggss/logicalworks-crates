# 4.5 Decoupled — components independently replaceable

Part of [production readiness](../production-readiness.md), §4, nine axes.

**✅ — the seams are real and are load-bearing.**

- `lgwks_deps` is a dependency storefront with a closed tier vocabulary
  (`boundary` / `vendor`) and a checked `APPROVED.toml`. The tier is enforced
  in one respect only: a `vendor` approval over a registry or git edge is
  refused (`VendorTierConflict`). Admission is otherwise decided by owner,
  source, origin, requirement, features, scope and licence, not by tier. No crate authors a
  `tokio` edge; the async surface is a facade reached through the storefront.
  A build cannot acquire an unaudited dependency without the gate refusing.
- `EffectJournal` is a trait, and `DurableAck::new` is public specifically
  because an adapter outside the crate is where an acknowledgement is produced.
  The seam is empty by choice, not by accident.
- `state` and `authority` are separate: `GrantSet` does not own the schedule, the
  journal does not own the effects, the verbs do not own the runtime.
- The public surface is a ratchet rather than a habit: `contract/PUBLIC_SURFACE.toml`
  records, per library file, the exact number of `pub` items it declares, and the
  `std-first` lane (`check-std-first`) refuses a file whose count rises, a file the
  table does not list, and a ceiling the tree has fallen below. A new public item is
  therefore a visible edit to the table: INV-SURFACE-1, #526.

*Not covered:* downstream compatibility rests on one consumer. Keel's migration
to `lgwks_bot` 1.x (`docs/guides/lgwks-bot/migrating-to-1-0.md`) is the only
external caller with evidence; a workspace caller search proves nothing about
others. The newest tags (`git tag --sort=-creatordate`) are `lgwks_std-v1.1.0`,
`lgwks_bot-v1.1.0`, `lgwks_deps-v1.1.0`, `lgwks_macros-v1.1.0` and
`lgwks_ast-v1.0.0`, and the manifests at `977c79d4` read the same versions. A
source tag is not a registry upload. The surface table fixes today's surface and
does not shrink it: hiding items behind `#[doc(hidden)]`, doctests through
`script!`, and the changelog entries a narrowing owes are the rest of #526.
