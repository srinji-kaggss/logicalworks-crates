# lgwks_std 3.0.1 / lgwks_bot 3.1.0 / lgwks_ast 2.1.0 / lgwks_deps 4.1.0 / lgwks_macros 1.1.3 - 2026-10-21

Release: lgwks_std 3.0.1 / lgwks_bot 3.1.0 / lgwks_ast 2.1.0 / lgwks_deps 4.1.0 / lgwks_macros 1.1.3 - 2026-10-21.
[Changelog index](../../CHANGELOG.md)

### Upgrading

No item is removed or renamed. `lgwks_bot` takes a minor bump because a host's
retry budget is now shared by every run it admits, which can make a run
`Throttled` where it previously held a per-run reserve. `lgwks_ast` and
`lgwks_deps` gain a subcommand and a binary. `lgwks_std` and `lgwks_macros`
carry documentation corrections only.

### lgwks_bot — one effect per caller key (#676)

- **Added** the doc-hidden `keyed` module: `KeyedReceiver`, `open_once`,
  `Settlement` and `KeyedError` keep one effect per caller key on a receiver
  that has no idempotency key. It reads the key, waits out the receiver's
  in-flight bound, reads again, creates once, and reconciles a lost create
  answer with a bounded read-back (at least eight reads over 15.75 s, then
  until the in-flight bound has passed).
- A create that may have landed and is not found is `KeyedError::Indeterminate`
  and never a second create. `gh::issue` is the first adapter; its find reads
  past a lagging listing by issue number.

### lgwks_bot — a step key carries its run (#646)

- `Scope::key` hashes the tenant, the run and the structural path, so one run's
  attempts and restarts share a key and two runs of one flow never do.
  `Scope::business_key` names work that is one act across runs and stays
  run-free.
- The retry jitter byte comes from the step key, so two runs of one flow no
  longer wait the same interval and retry in lockstep.

### lgwks_bot — one retry budget per host (#647)

- **Changed** a `Host` now admits every run under one shared retry budget
  (INV-BOT-171): the reserve is the host's, credit above it is earned by the
  host's first attempts, expires after ten seconds of clock time without one,
  and never pools past twice the reserve. A run the budget refuses is
  `FlowError::Throttled`.
- `Scope::root` and a standalone `FanOut::run` keep a per-run budget; a run
  admitted by a `Host` shares the host's. Seven new seeded simulations cover
  the pool ceiling, the income bound, expiry, earning after expiry and racing
  runs; a journey test drives two real runs on one host.

### lgwks_bot — other

- **Changed** `script!` flows settle on the flow's clock, and a keyed create
  settles before the create.
- **Added** `act` carries external effects, and `gh::issue` opens one issue per
  key.

### lgwks_ast — the script diff (#655)

- **Added** `lgwks-ast script diff <base> <head> [--json]` (feature `tool`),
  which reads two checked-out source roots from disk and classifies the
  architecture change between them. It exits 0 when nothing widens or every
  widening carries `// lgwks-architecture-approved: <reason>` in the head
  source, 1 on an unapproved widening, and 2 on a usage or parse error.
- The command makes no git call; the caller supplies the two roots.

### lgwks_deps — published-consumer in Rust (#633, #634)

- **Added** the `published-consumer` binary, which builds every published crate
  from crates.io at its newest tag with the README's own quickstart and prints
  one receipt per consumer (eight kinds). It exits non-zero when one fails or a
  tag names a version crates.io does not serve as its newest.
- The seeded simulation family covers the program builder that decides what each
  consumer compiles and runs.

### lgwks_std and lgwks_macros — documentation

- **lgwks_std 3.0.1**: the process modules name both `unsafe` modules and
  INV-STD-UNSAFE-1, and the proof runs under the `miri-unsafe` gate lane. No
  code or public item changed.
- **lgwks_macros 1.1.3**: the crate documentation states that a step key carries
  its run and that `Scope::business_key` is the run-free name. The expansion is
  unchanged.
