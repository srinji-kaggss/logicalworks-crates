# Upstream patches and the TLS provider path

Part of the dependency doctrine. The register is
[`contract/PATCHES.toml`](../contract/PATCHES.toml). An expired pin is a gate
refusal, and a blocked TLS provider with no live exception is refused with the
chain that pulled it in.

## Patch upstream, never fork

When an OSS crate does not work, the answer is an upstream patch, not a
permanent fork. A fork becomes a second product with its own security, license
and merge burden.

1. Reproduce the failure in a test that drives the real crate through the
   product path, naming the requirement and the real-world source.
2. File an upstream issue with the minimal repro, using the security policy
   for security bugs, and open an upstream PR.
3. Record the pin in the ledger: crate, kind `upstream-pin`, owner, reason,
   review, recorded and expiry dates, version, upstream issue and PR, and the
   patch digest.
4. Keep the regression test in the estate; it fails without the patch and
   passes with it.
5. When upstream merges, remove the pin, update the lockfile and delete the
   ledger entry. The regression test stays.

`[patch]` and `[replace]` tables stay refused, and no source replacement may
swap the bytes behind an approved edge. A temporary vendor tree needs a patch
digest, provenance, expiry and a removal test. If upstream rejects the patch,
the estate either replaces the crate or records the gap with an owner and a
date. Keeping the fork is not an option.

## TLS: terminate at the proxy, gate the provider

Day 0 terminates TLS at the OS reverse proxy with zero new crates. In-process
TLS needs `rustls` with a crypto provider whose license passes, and no
`ring` or `webpki-roots` by default. Both providers carry licence terms the
register does not accept, so the licence audit already refuses them as direct
edges; the ledger's transitive scan covers the resolved tree behind the
admitted `ureq` edge.

When no provider passes, the exception is sealed, not silent: owner, date,
blast radius and removal path live in the ledger as kind
`provider-exception`, with an expiry the quarterly review renews or removes.
The day-0 exceptions cover `ring` and `webpki-roots` behind the default-off
`http` feature; either crate without a live row is refused with its chain.
