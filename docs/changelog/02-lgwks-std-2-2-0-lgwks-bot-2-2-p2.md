# lgwks_std 2.2.0 / lgwks_bot 2.2.0 / lgwks_deps 3.0.1 - 2026-10-07 (2/2)

Release: lgwks_std 2.2.0 / lgwks_bot 2.2.0 / lgwks_deps 3.0.1 - 2026-10-07. Page 2 of 2. [Changelog index](../../CHANGELOG.md)

### Fixed (continued)

- **lgwks_std — a JSON number reads back as the bits that wrote it.**
  `serde_json`'s default parser read some exact decimals one ULP off
  (`997272.1318359375` came back as the next float up); `lgwks_std` now builds
  it with `float_roundtrip`, which adds no crate. Found by the locale sweep below,
  which draws an arbitrary finite `f64` as raw bits (subnormals included) on
  every seed and fails on its first seeds without the feature. Parse cost with
  the feature on was not measured here.
- **lgwks_deps — a declared `A/B` licence is read as `A OR B` (#354).** Cargo
  documents `/` in `license` as the deprecated spelling of `OR`, and crates.io
  still serves packages written that way (`foundationdb` 0.11 declares
  `MIT/Apache-2.0`). `lgwks-deps check` read the slash literally: one unknown
  identifier, and a drift from an approval recorded as `MIT OR Apache-2.0`. Both
  licence checks now read the declared side as the `OR` expression, so that
  package passes against its SPDX approval. Every alternative is still judged:
  `GPL-3.0/MIT` with only `MIT` accepted is still refused naming `GPL-3.0`. The
  register itself stays SPDX and its parser still refuses a slash
  (INV-DEP-16).

### Tests

- **lgwks_std — readers of human text are locale-independent (#278 row 4,
  INV-STD-LOCALE-1).** `tests/it/sim_locale.rs` sweeps 1,000 seeds through
  RFC 3339, hex, percent and JSON/RON numbers, refuses Arabic-Indic and Persian
  digits and a decimal comma, and re-runs the sweep and a process-start read in
  child processes under `C`, `de_DE`, `ar_SA`, `fa_IR` and `ja_JP`. Removing
  `run_ps`'s `TZ=UTC0 LC_ALL=C` pin fails it.

### lgwks_bot — a run still queued when the host stops is refused, whatever the wake order (#278 row 7)

- **Behaviour change:** a `Host::run` waiting for a permit when `Host::cancel` fires is now always `Disposition::Refused`. Before, a waiter that got a permit released by a stopped run before it observed the stop was counted as admitted, charged its root budget, and reported `Cancelled` without running. Which waiters that happened to depended on the wake order of the channel under the cancellation token, so one schedule could report a run either way. A permit acquired after the stop is now handed back (INV-BOT-20).
- Tests: `tests/it/sim_cancel_under_load.rs`, 1,000 flows cancelled at a seeded point over 1,000 seeds, each band replayed for one trace hash.
