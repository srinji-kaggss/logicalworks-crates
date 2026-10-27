# Deferred dependency catalog

Part of the dependency doctrine. The register is
[`contract/DEFERRAL.toml`](../contract/DEFERRAL.toml); this page states what it
means. The gate refuses an approved edge the catalog still defers.

## What deferral is

Day 0 is 2–6 authored crates, not 25–35. The estate already owns the semantic
layer, and every crate below either duplicates an estate owner or serves a
customer the revenue slice does not have yet. Until the trigger is met, adding
the crate creates a second owner and a larger cold build.

A request that needs one of these links its issue, shows the measured gap, and
is closed with the trigger and the owner — never with "not now". The estate
owner is fixed first: a retrieval defect is fixed in the owner, not routed
around with a new vector database.

## The families

| Family | Issue | Owner pattern | Trigger pattern |
|---|---|---|---|
| Data layer | #725 | `logical-DB`, `moo`, `world-model`, `sentinel` | A paying customer or a measured recall, latency or scale gap |
| Platform | #726 | `lgwks_bot`, `Nova`, kernel, `Braid` | A workflow, backend or guest requirement the estate cannot express |
| Edge | #727 | `sentinel`, `logical-DB`, `moo`, browser engine | A first paid CDN, object, message, file or Workers requirement |
| Day-0 optional | #721, #728 | `lgwks_deps`, `lgwks_std` | Schema published, or TLS in-process with a passing provider |

## Post-revenue, not out of scope

The full mission — Logic OS replaces Microsoft's whole stack by 2027-03-21 —
stands. The Azure and Cloudflare families above are sequencing, not a scope
cut: each row keeps its trigger, owner and replacement, and the catalog is
reviewed after the revenue slice is live, not before.

## Review

The quarterly dependency review reads `lgwks-deps ledger`, closes or
re-affirms every row, and refreshes `deferred_on`. A row without a trigger or
an owner is refused at load; a trigger that states no measurement is refused
with its field.
