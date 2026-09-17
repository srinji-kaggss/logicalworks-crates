---
type: Decision
title: AppCUI native terminal storefront admission
description: Director-selected AppCUI ownership, default-off feature boundary and release evidence.
resource: /docs/appcui-admission.md
tags: [dependencies, appcui, terminal, rocco]
generated: {by: agent:rust-coder, at: 2026-09-17}
verified: {by: agent:rust-coder, method: explicit Director direction and policy source inspection}
status: locally-verified-release-pending
stale_after: 2026-10-17
sources: [Director conversation 2026-09-17, /AGENTS.md, /docs/dependency-doctrine.md, /contract/APPROVED.toml, https://github.com/gdt050579/AppCUI-rs, https://crates.io/crates/appcui/0.5.1]
---

# Decision and actual authority

The Director selected "native AppCUI-rs terminal visual mockup" for Rocco,
then explicitly required "Make sure logicalworks-crates is being used".
The continuation authorizes "necessary minimal logicalworks-crates edits and
required commit/push/PR workflow under task authority" and says "Otherwise
add optional default-off AppCUI feature/reexport and necessary
register/closure/docs/tests/CI, verify through real gates."

This is the actual human decision recorded by `approved_by = "Director"`
and `approved_on = "2026-09-17"`. This document is agent-authored evidence of
that direction, not a human-authored document or cryptographic signature.
The authorized admission commit records it per dependency-doctrine lines
71–75 and the register header. The earlier Rocco report incorrectly turned
a delegation no-commit boundary into a human-signoff requirement. No such
extra cryptographic requirement was found: live main is unprotected, rulesets
and applicable branch rules are empty, and current release commit 30ae69d
is reported unsigned by GitHub. Git configuration was not changed.

# Ownership and alternatives

`std` and current `lgwks_std`/`lgwks_bot`/`lgwks_ast` do not supply a terminal
widget engine. Current `lgwks_deps` GPUI is a desktop renderer, not a native
terminal replacement. Direct AppCUI or a second custom terminal engine would
violate the selected framework and estate owner. Optional `lgwks_deps` AppCUI
is the BOUNDARY rung, with only `lgwks_deps` as normal-kind authored consumer.
Published appcui 0.5.1 reports MIT. Its platform closure includes clipboard,
terminal and OS dependencies; transitive code is upstream, not newly authored
estate wrappers. No universal safety or advisory-clean claim is implied.

# Assertions and evidence plan

1. No AppCUI in the default or no-default-feature resolved graph; inspect
   `cargo tree` before/after feature selection.
2. Explicit AppCUI feature resolves pinned 0.5.1 and the native re-export;
   feature build, macro doctest and clippy check it.
3. The real admission gate accepts the owned edge; its existing negative
   controls reject missing/wrong-owner/requirement approvals. Closure test
   continues to require optional dependencies; no baseline threshold relaxed.
4. Default estate tests and existing package smoke remain mandatory.
5. Native CI checks Linux/macOS/Windows separately. Local macOS compilation
   does not prove those other targets or an interactive terminal journey.

AppCUI prelude consumers must bring `use lgwks_deps::appcui;` into scope for
macro resolution. A re-export does not promise a full editor or remove the
need for user-facing input/focus/resize/crash tests. Rocco must wait for an
actual published estate version; no local path or fictional release allowed.

# Nine-axis and lifecycle limits

The exact nine-axis invariants remain those declared in the session and
canonical assurance contract. Manifest/register/lock/library changes predict
single ownership and opt-in compatibility; closure tests/CI verify those
invariants; README/changelog record release truth. Package-smoke cleanup is
changed only because its mandatory execution would otherwise irreversibly
delete artifacts. Test processes are finite, artifacts owned, disposal Trash-only.
All fleet, physical Ryzen profiles, human outcomes, full-path performance,
production isolation and crash/recovery acceptance remain unmeasured. No
frontier or fastest-correct-Rust claim follows from adopting this library.
Final command receipts and unresolved failures belong in the PR; publication
status must be observed separately from passing local tests.

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

The initial edit inserted duplicate YAML and changelog headings; the editor
diagnostic caught the YAML key before CI. Both duplicates were removed. One
multi-file correction patch failed on stale log context without applying; it
was retried against the exact source. These are retained failures, not passes.
The package-smoke change is a necessary safety repair for a mandatory command,
not permission to clean unrelated artifacts. Actual native UI journey, other
platforms, advisories and all broader assurance measurements remain unverified.
