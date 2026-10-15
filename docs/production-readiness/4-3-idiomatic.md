# 4.3 Idiomatic — ownership, errors, lifetimes sound

Part of [production readiness](../production-readiness.md), §4, nine axes.

**✅ — compiler-enforced, with one named hole in the proof chain.**

- `unsafe_code = "forbid"` at workspace scope. Not `deny`. A `deny` is defeated
  by any single `#[allow]` anywhere, including one smuggled in by a macro this
  workspace does not own; `forbid` cannot be lowered from source and a
  conflicting allow is a hard `E0453`.
- The anti-pattern corpus's 75 entries against 75 configured lints (69 `clippy`,
  5 `rustc`, 1 `rustdoc`); the four names in the corpus that silently do nothing
  are corrected. The rest need Miri, perf counters, allocation profiling or
  human review, and the corpus and the lint table are not a partition — see
  `CODEBOOK.md` §2.3. The ceiling is published rather than claimed.
- Every `#[allow]` and `#[expect]` must carry `reason = "..."`.
- `clippy --workspace --all-targets --locked -- -D warnings` is a required gate
  lane and is green, across a feature matrix and a `--no-default-features` pass.
- 15 integration test files covering authority, tick semantics, the effect
  journal, durable dispatch, identity, flow parsing, locator eligibility,
  process ownership, the async tier, and the no-default build.

**The named hole.** [`proofs/`](../../proofs/README.md) kernel-checks seven theorems
about the condition language and the capability gate. It is **not a refinement
proof**. The theorems are about a hand-written abstract model; nothing proves
`spec.rs` implements that model. The honest form of the claim is "the design has
these properties, and the implementation is argued not to express the states
that would break them." That second half is an argument.
