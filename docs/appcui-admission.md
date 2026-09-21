---
type: Decision
title: AppCUI native terminal storefront admission
description: AppCUI ownership, default-off feature boundary and release evidence.
resource: /docs/appcui-admission.md
tags: [dependencies, appcui, terminal, rocco]
decision: 2026-09-17
stale_after: 2026-10-17
sources: [/docs/dependency-doctrine.md, /contract/APPROVED.toml, https://github.com/gdt050579/AppCUI-rs, https://crates.io/crates/appcui/0.5.1]
---

# AppCUI native terminal storefront admission

This document records the approval of AppCUI as the native terminal rendering
capability for Rocco, with the alternatives measured against it, the ownership
boundary, and the release evidence observed to date.

## Decision

The approved approach is a native AppCUI-rs terminal visual mockup for Rocco,
rendered through the crates in this workspace rather than through a separate
implementation. The authorized scope is an optional, default-off AppCUI feature
with the required register, closure, documentation, test and CI changes,
verified through the real workspace gates.

The approval is recorded with the date `2026-09-17`. The admission commit
records it, per `docs/dependency-doctrine.md` §2 and the register header. This
document is evidence of that approval; it is not a signed artifact. An earlier
Rocco report incorrectly treated a delegation boundary as a requirement for
human sign-off. No such cryptographic requirement was found: live `main` is
unprotected, the rulesets and applicable branch rules are empty, and release
commit `30ae69d` is reported unsigned by GitHub. Git configuration was not
changed.

## Ownership and alternatives

`std` and the current `lgwks_std`/`lgwks_bot`/`lgwks_ast` do not supply a terminal
widget engine. Current `lgwks_deps` GPUI is a desktop renderer, not a native
terminal replacement. Direct AppCUI or a second custom terminal engine would
violate the selected framework and the ownership model. Optional `lgwks_deps`
AppCUI is the BOUNDARY rung, with only `lgwks_deps` as a normal-kind authored
consumer. Published appcui 0.5.1 reports MIT. Its platform closure includes
clipboard, terminal and OS dependencies; transitive code is upstream, not newly
authored wrappers in this workspace. No universal safety or advisory-clean claim
is implied.

## Verification plan

1. No AppCUI in the default or no-default-feature resolved graph; inspect
   `cargo tree` before/after feature selection.
2. Explicit AppCUI feature resolves pinned 0.5.1 and the native re-export;
   feature build, macro doctest and clippy check it.
3. The real admission gate accepts the owned edge; its existing negative
   controls reject missing/wrong-owner/requirement approvals. Closure test
   continues to require optional dependencies; no baseline threshold relaxed.
4. Default workspace tests and the existing package smoke remain mandatory.
5. Native CI checks Linux/macOS/Windows separately. Local macOS compilation
   does not prove those other targets or an interactive terminal journey.

AppCUI prelude consumers must bring `use lgwks_deps::appcui;` into scope for
macro resolution. A re-export does not promise a full editor and does not remove
the need for user-facing input, focus, resize and crash tests. Rocco requires an
actual published version of these crates; a local path or a fictional release is
not acceptable.

## Assurance limits

This admission claims single ownership and opt-in compatibility. The
manifest, register, lockfile and library changes predict those two properties;
the closure tests and CI verify them; `README.md` and `CHANGELOG.md` record
release truth. The package-smoke cleanup is changed only because its mandatory
execution would otherwise irreversibly delete artifacts. Test processes are
finite, artifacts are owned, disposal is Trash-only. All fleet behaviour,
physical Ryzen profiles, human outcomes, full-path performance, production
isolation and crash/recovery acceptance remain unmeasured. No frontier or
fastest-correct-Rust claim follows from adopting this library. Final command
receipts and unresolved failures belong in the pull request; publication status
is observed separately from passing local tests.

## Local execution receipts

On the edited source, macOS arm64, Rust 1.98.0:

- Dependency gate: exit 0, 19 semantic approvals, every authored edge owned.
- Workspace all-target tests: 206 passed (16 AST, 22 bot, 20 async tier,
  56 deps and 92 std); no failures.
- AppCUI no-default-feature library tests: 38 passed. Re-export macro doctest:
  1 passed. Both normal and AppCUI-feature clippy runs passed with warnings denied.
- Default/selected `cargo tree` assertions: AppCUI absent by default, pinned
  appcui 0.5.1 present when explicitly selected.
- Required package-smoke script: package verification and fresh extracted
  consumer both passed, printed `lgwks_std package smoke passed`; disposal
  used the available OS Trash tool, not irreversible removal.
- `cargo publish --dry-run --locked --allow-dirty -p lgwks_deps` succeeded:
  14 files packaged and verified, upload explicitly aborted due to dry run.
  This still uses 0.1.7 and warns it already exists; no release happened.
- `cargo owner --list lgwks_deps` returned `srinji-kaggss`. Credential environment
  variables are absent; a Cargo credentials file exists but no secret was read
  into the transcript. Dry-run/owner listing do not establish upload authorization.

Two process failures are recorded here rather than omitted. The initial edit
inserted duplicate YAML and changelog headings, which the editor diagnostic
caught before CI; both duplicates were removed. A multi-file correction patch
then failed to apply against stale context, and was retried against the exact
source. These are retained failures, not passes. The package-smoke change is a
necessary safety repair for a mandatory command, not permission to clean
unrelated artifacts. The actual native UI journey, other platforms, advisories
and all broader assurance measurements remain unverified.

## Publication correction and observed release

Release continuation was explicitly authorized once the question arose of
continuing beyond the open-PR boundary. `cargo publish --locked -p
lgwks_deps` then uploaded and published 0.1.8 successfully from clean commit
822307da2c7adefccd09b449af2998c75deea42b. A fresh `cargo info lgwks_deps@0.1.8`
download confirmed the AppCUI feature. Tag `lgwks_deps-v0.1.8` peels to that
commit on origin. This supersedes the earlier dry-run-only status; no credential
was disclosed and no signing configuration changed. CI run 35281748262 completed
successfully at that exact commit, including all three native feature targets.
Independent review is still pending; CI is not that review. Rocco consumes the
published capability, not the checkout path.
