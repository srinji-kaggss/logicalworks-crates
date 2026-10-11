# Every call presents a proof

Part of [Authority in lgwks_bot](../authority.md).


`GrantSet::issue` (`crates/lgwks-bot/src/gate.rs:131`) is the only path that
constructs an `Auth`. Its constructor is crate-private
(`crates/lgwks-bot/src/cap.rs:406`), and `Auth` is not `Serialize`, so authority
cannot round-trip through JSON.

Each verb takes an `(Auth, input)` tuple. `poll`, `execute_action`, and `query`
call `Auth::check(&self.required_caps())?` before touching anything. `Evaluate`
does not, because it takes no `Auth` at all: it is a boolean over already-observed
state with no side effect to gate.

Coverage is exact set membership, not subsumption
(`crates/lgwks-bot/src/cap.rs:526`). A proof scoped to `bot.fs` presented to a
source requiring `bot.net` is denied. A proof covering nothing authorizes
nothing. Both cases have tests in `crates/lgwks-bot/src/spec.rs`
(`wrong_scope_proof_is_denied_confused_deputy`, `call_with_empty_proof_is_denied_at_the_callee`).

The framework mints a fresh proof per call from the grant set it retained, so you
never thread an `Auth` through your own call sites for the chained path.
