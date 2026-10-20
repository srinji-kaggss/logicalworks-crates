# Governance §5 — completion and decision ledger release cuts, 2026-09-28 to 2026-10-06

Part of [`GOVERNANCE.md`](../../GOVERNANCE.md) §5, which lists every part,
newest first. Each entry names what changed, the receipts, and what it does
**not** claim.

### 2026-10-06 — Release cut: lgwks_std 2.1.0, lgwks_bot 2.1.0

- **Director-ordered** (same order as the 2.0.0 cut below). The cut is
  `origin/main` at c5aababd (PR #350 merged; its last PR run 37494277818 green,
  slowest job 4 m 22 s) plus this commit.
- Both crates add public items only and take the minor position
  (`docs/releasing.md` §3): #318's orphaned-group reap, #344's `seeded` stream,
  and #347's `ResidualRisk::LeaderExited` on a non-exhaustive enum. The
  behaviour change a caller will see — an exited leader's `Containment` is no
  longer complete — is a stricter report, which §3 places at minor, and the
  CHANGELOG's upgrade list names it. `lgwks_ast`, `lgwks_deps` and
  `lgwks_macros` have no source change since 2.0.0 and are not cut.

**Does not claim:** that anything is uploaded when this entry lands, or that
#351 (the neighbour-throughput test at 11% on GitHub runners) is resolved.

### 2026-10-06 — Release cut: lgwks_std 2.0.0, lgwks_ast 1.1.0, lgwks_deps 3.0.0, lgwks_bot 2.0.0, lgwks_macros 1.1.1

- **Director-ordered** ("U WILL ENSURE LOGICALWORKS-CRATES is GOOGLE AND APPLE
  LEVEL COMPLETE AND RELEASED ONTO CARGO", 2026-10-06). The cut is `origin/main`
  at 44c94326 (PR #339 merged, all 32 CI checks green, run 37475980214, 4 m 42 s
  wall) plus this commit.
- Versions follow `docs/releasing.md` §3: `lgwks_std`, `lgwks_deps` and
  `lgwks_bot` each remove or retype a public item and take the major position;
  `lgwks_ast` adds public items only and takes a minor; `lgwks_macros` changes
  no public item and takes a patch. CHANGELOG.md's release section opens with
  an upgrade list naming each break and its replacement.
- The `[Unreleased]` block had collected five headings and a 369-line verbatim
  duplicate of ten sections (#264, #269, #271, #272, #276, #277); the release
  section keeps one copy of each.

**Does not claim:** that anything is uploaded when this entry lands. The upload,
tags and GitHub releases follow the merge, in `docs/releasing.md` §1 and §4 order.

### 2026-09-30 — Release cut: lgwks_std 0.10.0, lgwks_ast 0.4.0, lgwks_deps 0.4.0, lgwks_bot 0.8.0, lgwks_macros 0.1.2

- **Director-ordered** ("SO FINISH THE WORK DUDE, LIKE WHY ARE U LEAVING
  STUFF", 2026-09-30, under the standing "Once done, if required, update cargo
  release"). #199 landed the #191-#194 review fixes. `lgwks_std`, `lgwks_deps`,
  `lgwks_ast` and `lgwks_bot` each break against the version on crates.io, so
  each takes the minor position; `lgwks_macros` takes a patch for its
  requirement. The reasons for each crate are in CHANGELOG.md.
- The credentials were confirmed before the chain started
  (`~/.cargo/credentials.toml`). The cut was verified with
  `cargo publish --workspace --dry-run --locked` before upload.

**Does not claim:** that anything is uploaded when this entry lands. The upload,
tags and GitHub releases follow the merge, in `docs/releasing.md` §1 and §4 order.

### 2026-09-30 — Release cut: lgwks_std 0.9.0, lgwks_deps 0.3.0, lgwks_bot 0.7.0, lgwks_macros 0.1.1

- **Director-ordered** ("SO FINISH THE WORK DUDE, LIKE WHY ARE U LEAVING
  STUFF", 2026-09-30, under the standing "Once done, if required, update cargo
  release"). #197 landed the unmerged work for #157, #158, #159, #161, #162, #167
  and #168 and the #195 tooling fixes. `lgwks_std` and `lgwks_deps` each break
  against the version on crates.io, so each takes the minor position. `lgwks_bot`
  moves with them, and `lgwks_macros` takes a patch for its requirement.
  `lgwks_ast` does not move. The reasons for each crate are in CHANGELOG.md.
- The credentials were confirmed before the chain started (`~/.cargo/credentials.toml`). The
  cut was verified with `cargo publish --workspace --dry-run --locked` before
  upload.

**Does not claim:** that anything is uploaded when this entry lands. The upload,
tags and GitHub releases follow the merge, in `docs/releasing.md` §1 and §4 order.

### 2026-09-30 — Release cut: lgwks_std 0.8.0, lgwks_ast 0.3.0, lgwks_deps 0.2.0, lgwks_bot 0.6.0, lgwks_macros 0.1.0

- **Director-ordered release cut** ("Once done, if required, update cargo
  release", 2026-09-30). #190 integrated eleven open PRs (#173, #175–#183,
  #188) plus its review fixes. Each moved crate carries a break against what
  crates.io holds, so each takes the minor position; the per-crate reasons are
  in CHANGELOG.md. `lgwks_macros` is new and is published between
  `lgwks_deps` and `lgwks_bot`; `docs/releasing.md` now names it in the order.
- Publish is still manual and needs a human-held crates.io token; this cut is
  the manifests, lockfiles (workspace and the storefront fixtures), README and
  guide pins, and changelog, verified by `cargo publish --workspace --dry-run`,
  not an upload.

**Does not claim:** that anything is on crates.io; tags or GitHub releases,
which follow the upload per `docs/releasing.md` §4.

### 2026-09-28 — Release cut: lgwks_std 0.7.0, lgwks_bot 0.5.0, lgwks_deps 0.1.13

- **Director-authorized release cut** ("Yes, do it", 2026-09-28). The PR #91
  cut (0.6.7 / 0.5.0 / 0.1.13) was never uploaded, and `main` gained 86
  commits afterwards, including `lgwks_std` breaking renames and removals. Uploading
  that tree as 0.6.7 would publish a break under a version Cargo resolves as
  compatible with 0.6.6, so `lgwks_std` moves to **0.7.0**. `lgwks_bot` 0.5.0 and
  `lgwks_deps` 0.1.13 keep their cut numbers; the reasons are in CHANGELOG.md.
  `lgwks_ast` does not move.
- Publish is still manual and needs a human-held crates.io token; this cut is
  the manifest, lockfile, README pins and changelog, not an upload.

**Does not claim:** that anything is on crates.io; tags or GitHub releases, which
follow the upload per `docs/releasing.md` §4.
