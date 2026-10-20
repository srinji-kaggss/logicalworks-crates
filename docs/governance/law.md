# Governance §2

Part of [`GOVERNANCE.md`](../../GOVERNANCE.md), which indexes every section.

## 2. Production Engineering Law

Applies to every human and every coding-agent change. Unless an approved ADR
explicitly narrows scope, design for: multiple independent users, tenants,
workspaces, callers, and instances; concurrent operations; hostile and malformed
input; retries, duplicates, and out-of-order delivery; restarts, upgrades, and
migrations; partial dependency and network failure; empty and large datasets;
bounded resources; long-lived state; different devices, locales, time zones,
input methods, and accessibility needs.

This forbids irreversible singleton assumptions. It does not require
speculative distributed systems.

Every change must make identity and ownership scope explicit; enforce isolation
and authorization at boundaries; define atomicity, ordering, idempotency,
conflicts, and retry behaviour; handle timeouts, cancellation, crash recovery,
partial failure, and safe lifecycle transition; bound queues, scans, caches,
recursion, fan-out, retries, payloads, and logs; version persisted schemas and
protocols; avoid hardcoded identity, machine, path, credential, provider, or
topology; preserve structured and redacted evidence; support version skew or a
controlled migration; and treat security, privacy, accessibility, and non-happy
UI states as product behaviour.

**Proof** must cover the relevant combination of: two independent
identities/workspaces/instances and isolation; concurrent calls, retries,
duplicates, and reordering; restart/crash, timeout, cancellation, and dependency
failure; empty, boundary, oversized, malformed, hostile, and unauthorized input;
migration, rollback, corruption, and version skew; resource ceilings and
backpressure; and the real integration path. Mocks may assist but cannot be the
only proof. A happy-path-only suite is a failing implementation.

**A feature that works only for one person is a fixture. It is not the product.**

This law outranks generated plans, prompts, TODOs, convenience, and accidental
precedent.

### Invariants this repository holds

From `experience/charter.yaml`, binding on every change:

- Missing capability fails at build time, never at 2am at runtime.
- Unregistered dependency fails the gate with the owning fix named, never
  silently.
- Machine output for agents never mixes prose into payloads.
- Repeating a safe command never duplicates irreversible work.

`INV-DEP-EDGE-OWNED`: every external dependency authored by a workspace package
names its semantic owner, capability, source, requirement, allowed consumers,
and allowed dependency kinds.
