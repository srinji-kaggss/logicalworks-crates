# Should I use logicalworks-crates?

`logicalworks-crates` is the Rust repository of Logical Works Incorporated: five
independently versioned crates published to crates.io, each usable alone
(`lgwks_std` 3.0.2, `lgwks_bot` 3.1.1, `lgwks_ast` 2.2.0, `lgwks_macros` 1.1.4,
`lgwks_deps` 4.2.0 in this tree). It is not a framework and not a rewrite of the
ecosystem: it is the layer where a program's authority, its dependency graph and
its code reading are decided once instead of in every project.

The short answer: use a crate here when you want one of those decisions made for
you and enforced by a compiler and a gate. Do not use it when you need an
ecosystem, a support contract, or a package whose bus factor you can ignore.

## Use it if

- You want long-running automation whose fan-out is bounded, whose retries share
  one budget, whose steps are keyed by tenant, and whose shutdown leaves no
  detached task. → `lgwks_bot` (`script!`, `try_join!`, `rt::supervise`).
- You want an agent or a bot whose permissions are a checked value rather than a
  convention: every verb proves a capability before it acts, and a proof covering
  nothing authorizes nothing. → `lgwks_bot`.
- You need HTTP, JSON, retry, time and structured logging from a CLI or a build
  script without pulling an async runtime. → `lgwks_std` (blocking, rustls-only).
- You want one AST type and one diagnostic type across several languages, with
  bounded traversal and byte-accurate spans, and to compile only the grammars you
  ship. → `lgwks_ast`.
- You want "what third-party code is in this build, and who approved it?"
  answered by a command rather than an archaeology project. → `lgwks_deps`
  (`lgwks-deps init .`, `lgwks-deps check .`).

## Don't use it if

- **You need a battle-tested default.** These crates were first published in
  2026 and their install base is small. `serde`/`serde_json`, `reqwest`, `tokio`,
  `tracing` and `tree-sitter` have years of production use and a large pool of
  people who have already hit your bug. That matters more than any property here.
- **You need a support contract or an LTS.** There is no SLA, no paid support
  channel, and no long-term branch. The compatibility promise covers three
  surfaces only, listed in the [consumer guides](guides/index.md); everything
  else is `#[doc(hidden)]` support code that may change in any release.
- **You want an agent framework.** `lgwks_bot` does not call a model, own prompts,
  route tools or manage providers. It is the execution substrate beneath one.
- **You want a distributed system.** These crates refuse a network path rather
  than pretend; read [distributed boundaries](distributed-boundaries.md) first.
- **`MPL-2.0` is a problem for you.** `lgwks_bot` is file-level copyleft;
  `lgwks_std`, `lgwks_ast` and `lgwks_deps` are Apache-2.0. The rule and the
  commercial licence are in [`LICENSING.md`](../LICENSING.md).
- **You need breadth.** One crate here does not replace a stack: adopting
  `lgwks_ast` does not ask you to drop tokio or serde, and adopting `lgwks_bot`
  does not move your queue.

## Per crate

| Crate | Reach for it when | Skip it when |
|---|---|---|
| `lgwks_std` | A synchronous tool needs JSON, HTTP, retry, time, ids, hashing, globs or logging and an async runtime would be the wrong trade | You already run tokio and want its ecosystem's HTTP and JSON |
| `lgwks_bot` | Work is in-process, long-lived, must cancel cleanly, and its authority must be provable | You need a broker-backed queue (`fang`, `apalis`), or an LLM client layer |
| `lgwks_ast` | A linter, codemod or audit reads several languages into one shape | You need a parser generator, or one language with a bespoke parser |
| `lgwks_macros` | You are using `script!`; it is reached through `lgwks_bot` | Always: there is no standalone reason to depend on it |
| `lgwks_deps` | You want dependency admission, an audit command, and a storefront with no default third-party edge | You want a licence scanner or an SBOM generator; it is neither |

## What you are not buying

No telemetry backend, no hosted service, no model access, no sandbox, no
credentials broker, and no guarantee that a red build on your machine is a red
build here. The security case states its own ceiling in
[`security-posture.md`](../security-posture.md); the readiness gaps are listed in
[`production-readiness/`](../production-readiness).

## What is proven, and how to check it

- Every crate ships from one repository and one tag; the release process is in
  [`releasing.md`](releasing.md), and a published-consumer binary builds the
  published versions from crates.io.
- Dependency admission is enforced by a gate, not by review: every external edge
  names its owner and capability, and an unowned edge is refused
  (`INV-DEP-EDGE-OWNED`).
- Claims in the guides cite `path:line`, and a binary verifies every citation
  resolves ([`guides/index.md`](guides/index.md#how-these-pages-cite)).

## Start in two minutes

```sh
cargo add lgwks_std --features json,http     # primitives, no async runtime
cargo add lgwks_bot                          # automation with checked authority
cargo install lgwks_deps && lgwks-deps init . && lgwks-deps check .
```

## The name

Logical Works Incorporated publishes these crates; the repository is
`logicalworks-crates`, the packages are `lgwks_*`, and the gate binary is
`lgwks-deps`. A search for "logicalworks-crates" should land on this repository
or on the docs.rs page of one of its crates, not on LogicWorks circuit software,
which is unrelated.
