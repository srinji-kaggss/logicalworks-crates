# Licensing

Two licences, one crate apart, plus a commercial option for the one case the
public licence does not cover.

## Which crate carries which

| Crate | Licence | File |
|---|---|---|
| `lgwks_std` | Apache-2.0 | [`crates/lgwks-std/LICENSE`](crates/lgwks-std/LICENSE) |
| `lgwks_ast` | Apache-2.0 | [`crates/lgwks-ast/LICENSE`](crates/lgwks-ast/LICENSE) |
| `lgwks_deps` | Apache-2.0 | [`crates/lgwks-deps/LICENSE`](crates/lgwks-deps/LICENSE) |
| `lgwks_bot` | **MPL-2.0** | [`crates/lgwks-bot/LICENSE`](crates/lgwks-bot/LICENSE) |

Copyright 2026 Logical Works Incorporated.

## Why the bot differs

`lgwks_bot` is the artefact every other repository in this estate embeds. Under a
permissive licence anyone may take it, modify it, and ship those modifications
closed, and no later release recovers that. MPL-2.0 is file-level copyleft,
which is the smallest licence that prevents exactly that and nothing else:

- **Use is unrestricted.** Closed, open, internal, hosted, sold — no condition
  attaches to running it.
- **Modification of the MPL-covered files carries one obligation.** MPL-2.0 §3.2
  requires those modified files to be made available in source form. Files added
  at another layer carry no such obligation.
- **A larger work may be distributed under other terms.** MPL-2.0 §3.3 says so
  explicitly, which is why a proprietary consumer remains permitted and why the
  three Apache-2.0 sibling crates are unaffected.

The three siblings stay Apache-2.0 because they are libraries with a different
job: they exist to be consumed as widely as possible, and copyleft on them would
cost adoption without protecting anything the bot does not already protect.

## Commercial licence

MPL-2.0 permits closed **use**. It does not permit closed **modification** —
§3.2 obliges you to publish your changes to the MPL-covered files. An
organisation that needs to modify the bot's sources without that obligation can
license it commercially instead, on terms granting exactly that relief and
nothing else.

Commercial licensing enquiries: contact the repository owner through
[srinji-kaggss/logicalworks-crates](https://github.com/srinji-kaggss/logicalworks-crates).

## Contributions, and the one thing that must be in place first

Offering a work under two licences requires the licensor to hold sufficient
rights in **every** contribution to it. A contribution accepted under the
inbound MPL alone cannot be relicensed commercially, which would make the offer
above impossible to honour for any release containing it.

A signed contributor licence agreement would therefore be required before a
non-trivial contribution could be merged. **`lgwks_bot` is closed to outside
contributions until one exists**, and that is a door rather than a queue: a patch
sent today has no path to merge and is refused on arrival rather than parked
pending a decision.

Selecting the instrument is deferred. The two standard candidates are the Apache
Individual CLA and the Harmony Contributor Assignment Agreement, and the choice
will be recorded here when it is made. Until then, do not send patches to
`lgwks_bot`. [`CONTRIBUTING.md`](CONTRIBUTING.md) states the same rule where a
contributor will see it.

This constrains nothing for the other three crates, which are Apache-2.0 with no
relicensing ambition and need no agreement.

## The published versions did not change

Licences are not retroactive. **Every version already on crates.io is
Apache-2.0 and stays Apache-2.0**, including `lgwks_bot` 0.4.2. This change takes
effect at the next version published from this tree.

That matters for the four repositories depending on the bot, because they pin
exact published versions:

| Repository | Pin | Affected |
|---|---|---|
| `maps` | `=0.1.2` | no |
| `rocco` | `=0.3.2` | no |
| `rocco-runtime` | `=0.3.2` | no |
| `forge-harness` | `0.3.0` | no |

None is affected until it moves to a version published under MPL-2.0, and MPL-2.0
permits that move: §3.3 covers distributing a larger work under other terms, so
an Apache-2.0 or proprietary consumer stays permitted. The obligation lands on
the bot's own files, not on theirs.

## What this file is not

A summary written by the project, for orientation. Not legal advice. The
MPL-2.0 text in [`crates/lgwks-bot/LICENSE`](crates/lgwks-bot/LICENSE) is the
licence, and where this file and that text disagree, that text governs.
