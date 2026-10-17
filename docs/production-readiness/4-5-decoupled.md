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
- The front page says what the parser does: the rustdoc front page of `lgwks_std`,
  `lgwks_bot`, `lgwks_ast` and `lgwks_deps` opens with one generated `src/lexicon.md`
  (the way in, the first lexicon example and the block table) rendered from `LEXICON`,
  and `lgwks_macros` carries the same table in its own crate documentation. A page
  that drifts from the lexicon, or a `lib.rs` that does not begin with the include,
  fails `lexicon_docs::every_front_page_opens_with_the_lexicon`: INV-FRONT-1, #526.
  The `lgwks_ast` root was measured once, when it was hidden: its all-features
  rustdoc fell from 172 pages to three (`cargo doc -p lgwks_ast --all-features`,
  commit `725a3c88`). No test holds that count, so #526 owns it.
- The `lgwks_bot` getting-started guide runs a `script!` program: its pages follow
  `crates/lgwks-bot/examples/queue_watch.rs`, which CI builds with every
  example and which was run once for the pages (three printed lines, three
  asserted page counts). No test runs it, so #526 owns that too. The example
  ceilings for the guide's `index.md`, `getting-started-pieces.md` and
  `getting-started-program.md` fell to zero (INV-EXAMPLE-1).
- The registry and front-door examples hold a flow too: the `task` module's
  front-door doctest, `task::task` and `DomainRegistry` compile one and pass
  with the other 45 under `cargo test --doc -p lgwks_bot` (48 of 48), and the
  `domains.md` block was built and run once (`open prs: 0`). The ceilings for
  `registry.rs`, `task.rs`, `domains.md` and `authority-names.md` fell to zero
  (INV-EXAMPLE-1). No doctest runs those flows, so #526 owns that. The first CI
  run of that edit (run 39001774357, commit `8c280ac0`) lost four jobs at the
  300 s cap with no test failing: in the two job logs read (workspace and
  bot-full) `Compiling lgwks_bot` began at 04:14:23Z and was still running when
  the cancel landed at 04:19:05Z. A doc comment in a Rust file recompiles the
  crate cold, which #272 owns. Run 2 (39002421637, `3ff90d01`, markdown only)
  lost two: every step of the workspace job had passed by 04:29:04Z, 296.7 s
  after it started at 04:24:08Z, and the job still ended `cancelled` at
  04:29:16Z; bot-full had run 2,294 of 4,721 tests when the cancel landed at
  04:29:20Z. #272 owns both walls.
- The poll watchdog gives a source the reaper owes a turn that turn: `WavePoll::turn`
  decides a stall from the expiry it read at the start of the turn, so a deadline that
  passes while a poll runs no longer cuts off a poll whose waker was installed before it
  (INV-BOT-123, `ecs::tests::test_part_04`). The push run after PR 586 (39003404318)
  had failed `the_same_seed_replays_a_deadline_wave` on the old reading. The fix's
  deterministic test failed on the unfixed tree, and 117 of 117 tests in the `ecs`
  and `observe_refresh` filters pass on the fixed one. The first CI run of the fix
  (39004795544, commit `71077831`) lost four jobs at the 300 s cap with no test
  failing: in the two job logs read (workspace and bot-full) `Compiling lgwks_bot`
  began at 05:02:58Z and 05:02:59Z and was still running when the cancel landed at
  05:07:43Z. Run 2 (39005381369, `d1928149`, markdown only) lost two: the workspace
  job printed `1548 tests run: 1548 passed` at 05:17:14Z and was cancelled in the same
  second, and bot-full finished its compile in 3 min 30 s at 05:17:06Z and had run 30
  of 4,724 tests when the cancel landed at 05:17:18Z. #272 owns both walls.
  Main run 39008733262 (`c51df6bd1`, 31 of 31 green, 315 s) splits its 291 s
  workspace job as `Pin Rust 1.99.0` 20 s, of which the RAM-disk, machine-share,
  toolchain and nextest sub-action took 19.6 s; `cargo test --no-run` 64 s, with
  four `Blocking waiting for file lock on package cache` lines between 06:08:00Z and
  06:08:39Z and the compile finishing at 06:08:52Z; the tests 158 s (nextest 138.5
  s). The RAM-disk recipe alone, run on an idle host, took 2.3 s (attach 0.16 s,
  APFS erase 1.84 s, eject 0.20 s), so the 19.6 s is that recipe with 13 runners
  starting together, a cause this page has not shown, and no run named the process
  that held the package-cache lock. #272 owns both.

*Not covered:* downstream compatibility rests on one consumer. Keel's migration
to `lgwks_bot` 1.x (`docs/guides/lgwks-bot/migrating-to-1-0.md`) is the only
external caller with evidence; a workspace caller search proves nothing about
others. The newest tags (`git tag --sort=-creatordate`) are `lgwks_std-v1.1.0`,
`lgwks_bot-v1.1.0`, `lgwks_deps-v1.1.0`, `lgwks_macros-v1.1.0` and
`lgwks_ast-v1.0.0`, and the manifests at `977c79d4` read the same versions. A
source tag is not a registry upload. The surface table fixes today's surface and
every root has shrunk: the modules and re-exports of `lgwks_bot`, `lgwks_std`,
`lgwks_deps` and `lgwks_ast` sit behind `#[doc(hidden)]`, so `lgwks_bot`'s rustdoc
lists `script!`, `try_join!` and the `script` module, `lgwks_ast`'s lists the
`script` module, and the other two list prose only. `lgwks_ast`'s `README.md`
(291 lines, past the page limit, so it cannot be edited) and `parser.md` carry the
direct-use examples, so they are no longer its front page: they stay compiled on a
hidden item at its root, which keeps their examples running as doctests. A
`#[doc(hidden)]` on `mod x;` hides that line only, so the files of a
hidden module keep their own ceilings and a new `pub` item inside one is still
refused; no test asserts the cross-file half, and #526 owns it. Doctests through
`script!`, and the changelog entries a narrowing owes are the rest of #526.

*#272, measured since 2026-10-17:* #590 (`8571c2622`) moves the RAM-disk detach
behind the job. Thirteen jobs started in one second were past their disk after
4.09 s and 4.77 s, against 10.31 s. Its PR run (39011834453) was 31 of 31 green at
294 s, and the bot-full job's setup step took 2,901 ms where main run 39010245322
took 15,846 ms. The merge's own main run (39012345363) was cancelled at the job
cap: bot-full's nextest step took 280 s (230 s on the PR run), the workspace job
met 300 s, and the one-minute load average read 155.7 at 07:09:44Z. One tree took
294 s once and was cancelled once, so a setup piece is not margin. The full local
gate on `8571c2622` passed 47 lanes and skipped two in 871.6 s, of which
`storefront-consumers` alone took 703.1 s. The bot-full lane alone on a quiet host
took 119.1 s: 70 s of compile and `Summary [48.122s] 4724 tests run`. Warm, beside
45 busy loops (three per core), it took 77.7 s with nextest at 68.1 s, so CPU load
alone adds about 20 s to the run, against the CI step's 230 s and 280 s. What
else slows a CI job had not been measured when this was written: with no job
running, swap read 19.3 GB of 20.5 GB used, the compressor held 15.5 GB, and
`fseventsd` was 4.9 GB resident at 100% CPU.
