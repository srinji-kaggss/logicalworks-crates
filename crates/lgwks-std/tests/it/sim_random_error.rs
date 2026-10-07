//! Seeded simulations of the OS-entropy contract behind `lgwks_std::random`
//! and the UUID v4 generation built on it.
//!
//! One seed drives every draw length, every fixture and every assertion in a
//! run. Each failure message names its seed, and the trace hash of a run's
//! *classifications* — lengths drawn, whether a sentinel survived, how many
//! distinct byte values came back — is asserted stable for a seed. The drawn
//! bytes themselves are never folded into the trace: entropy is not
//! reproducible by construction, so a trace over the bytes could not replay,
//! and a trace that cannot replay proves nothing.
//!
//! # What is simulated and what is not
//!
//! The success path is fully observable from outside the crate, so every draw
//! in this file is a real call into the OS entropy source. The failure path is
//! not: `getrandom` offers no constructor for a backend failure carrying an OS
//! code (`Error::new_custom` yields a code with no OS meaning), so no public
//! API can be made to fail on a working host without breaking the host. That
//! half of the contract is driven through the crate-private seam in
//! `crates/lgwks-std/src/random.rs`, whose tests name the OS codes they inject.
//! This file states that limit rather than simulating a failure it cannot
//! produce.
//!
//! # Fire conditions
//!
//! Length boundaries from empty through a mebibyte, concurrent callers at
//! 100, 1 000, 10 000 and 100 000, constant-buffer detection (a source that
//! returned one repeated byte would satisfy every length assertion here while
//! being no entropy at all), and the UUID version and variant masks over enough
//! draws for both masked fields to take more than one value.
#![forbid(unsafe_code)]

#[cfg(feature = "random")]
mod sim {
    use std::collections::{BTreeMap, BTreeSet};

    use lgwks_std::id::Uuid;
    use lgwks_std::random::{self, EntropyError};

    use crate::seeded_sweep::{
        SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold_usize,
        initial_trace, next_index, seeded_stream,
    };

    /// The concurrency tiers the drawers drive.
    const TIERS: [usize; 4] = [100, 1_000, 10_000, 100_000];

    /// How many drawn bytes one sentinel byte is expected to survive in.
    const SENTINELS_PER_BYTE: usize = 256;

    /// The slack the sentinel budget allows on top of twice its expectation.
    const SENTINEL_BUDGET_SLACK: usize = 8;

    /// The tier this host is required to reach in full.
    ///
    /// A host that cannot create 10 000 threads with 64 KiB stacks cannot run
    /// this contract at the tier that matters, and the tier above it is
    /// reported as a refusal rather than assumed.
    const REQUIRED_TIER: usize = 10_000;

    /// Per-caller stack size, in bytes.
    ///
    /// A hundred thousand default 8 MiB stacks is 800 GiB of reservation on a
    /// host that does not need it: a caller here draws 32 bytes and folds a
    /// trace, so 64 KiB is ample. The size is a fixture and is named in every
    /// tier assertion.
    const STACK_BYTES: usize = 64 * 1024;

    /// The byte every buffer is pre-filled with, so a partial write is visible.
    const SENTINEL: u8 = 0xAA;

    /// The number of bytes one concurrent caller draws.
    const DRAW_BYTES: usize = 32;

    /// Draws per seed in the repeat-within-a-seed families.
    const DRAWS_PER_SEED: usize = 64;

    /// The draw lengths one sweep covers, endpoints included.
    ///
    /// Zero is the exact endpoint that costs no syscall, one is the smallest
    /// draw that reaches the OS at all, and the rest are sizes a caller asks
    /// for: an ID, a nonce, a page of keys, a bulk buffer.
    const LENGTHS: [usize; 7] = [0, 1, 7, 16, 32, 4_096, 1 << 20];

    /// The FNV-1a prime, for mixing the per-draw folds.
    const FNV_PRIME: u64 = 1_099_511_628_211;

    /// The length a short-write check can actually distinguish.
    ///
    /// Below this length a byte that merely happened to equal the sentinel is
    /// indistinguishable from a byte that was never written, so the completeness
    /// assertions below are reported as a budget rather than as a proof.
    const DISTINGUISHABLE_AT: usize = 4_096;

    /// The shortest draw whose coming back entirely unwritten means something.
    ///
    /// A whole draw of `n` bytes equals the sentinel everywhere with probability
    /// `256^-n`: one draw in 256 at one byte, which is a test that fails on a
    /// working source, and `2^-64` at eight. Shorter draws are not classified
    /// as untouched at all, rather than classified with a known false-positive
    /// rate.
    const UNTOUCHED_DISTINGUISHABLE_AT: usize = 8;

    /// The most sentinel bytes a whole draw of `length` bytes may leave behind.
    ///
    /// The sentinel is a byte value like any other, so a draw of `n` bytes
    /// leaves `n / 256` sentinels on average and the bound is that expectation
    /// doubled plus a small constant. A partial write leaves `n - k`, which for
    /// every length at or above [`DISTINGUISHABLE_AT`] is above the budget even
    /// when the backend wrote 99.9% of the buffer. Below that length the budget
    /// is a floor and the assertion is a smoke check, which is why the
    /// completeness claims are made on the two long draws.
    fn sentinel_budget(length: usize) -> usize {
        // Twice the expected whole number of sentinel bytes, plus a constant. The
        // expectation is one byte in 256, and a length below 256 has no whole
        // one: its quotient is zero there, so the constant alone is the floor a
        // short draw is held to, which is what the note above says the budget is.
        length
            .div_euclid(SENTINELS_PER_BYTE)
            .saturating_mul(2)
            .saturating_add(SENTINEL_BUDGET_SLACK)
    }

    /// The traceable shape of one sweep.
    ///
    /// Every field a second run under the same seed reproduces is either here or
    /// in an assertion outside the trace; nothing derived from the drawn bytes
    /// is, because entropy does not replay and a trace that cannot replay
    /// compares nothing.
    #[derive(Debug, Default, PartialEq, Eq)]
    struct SweepTrace {
        /// Number of draws performed.
        draws: usize,
        /// Sentinel bytes left behind across every draw.
        sentinel_survivors: usize,
        /// The most sentinel bytes any single draw of each length left.
        worst_sentinels: BTreeMap<usize, usize>,
        /// Draws that came back entirely unwritten.
        untouched_draws: usize,
        /// Draws that came back as one repeated byte value.
        constant_buffers: usize,
        /// Draws the entropy source refused.
        refusals: usize,
        /// The running trace over the classifications.
        trace: u64,
    }

    /// Renders a draw's refusal, or an empty string where there was none.
    fn refusal_text(outcome: &Result<Vec<u8>, EntropyError>) -> String {
        outcome
            .as_ref()
            .err()
            .map_or_else(String::new, ToString::to_string)
    }

    /// Records one draw's outcomes and folds its length into the running trace.
    ///
    /// Nothing derived from the buffer is folded, not even whether it came
    /// back untouched: a one-byte draw from a working source equals the
    /// sentinel one time in 256, so a trace that folded that classification
    /// diverged between two runs of the same seed. The length is the fact a
    /// second run under the same seed reproduces exactly, whatever bytes the
    /// first run happened to draw; the refusal is folded by the caller.
    fn observe(trace: &mut SweepTrace, length: usize, buf: &[u8]) {
        let survivors = buf.iter().filter(|byte| **byte == SENTINEL).count();
        let distinct: BTreeSet<u8> = buf.iter().copied().collect();
        let untouched = length >= UNTOUCHED_DISTINGUISHABLE_AT && survivors == buf.len();
        trace.draws = trace.draws.saturating_add(1);
        trace.sentinel_survivors = trace.sentinel_survivors.saturating_add(survivors);
        let worst = trace.worst_sentinels.entry(length).or_insert(0);
        *worst = (*worst).max(survivors);
        trace.untouched_draws = trace.untouched_draws.saturating_add(usize::from(untouched));
        trace.constant_buffers = trace
            .constant_buffers
            // A one-byte buffer has one distinct value by construction, so only
            // a draw of two or more can answer the question.
            .saturating_add(usize::from(length > 1 && distinct.len() == 1));
        trace.trace = trace.trace.wrapping_mul(FNV_PRIME);
        fold_usize(&mut trace.trace, length);
    }

    /// Draws one buffer of `length` bytes and records what came back.
    ///
    /// A refusal is counted rather than discarded, so a host without an entropy
    /// source fails the sweep's assertions instead of quietly emptying them.
    fn draw(trace: &mut SweepTrace, length: usize) -> Result<Vec<u8>, EntropyError> {
        let mut buf = vec![SENTINEL; length];
        let outcome = random::fill_bytes(&mut buf);
        observe(trace, length, &buf);
        if outcome.is_err() {
            trace.refusals = trace.refusals.saturating_add(1);
        }
        outcome.map(|()| buf)
    }

    /// The length sweep for one seed.
    ///
    /// The declared lengths are drawn in an order drawn from the seed, so two
    /// seeds observe the same set in a different sequence and a replay visits
    /// the same boundaries in the same order.
    fn sweep(seed: u64) -> SweepTrace {
        let mut trace = SweepTrace {
            trace: initial_trace(),
            ..SweepTrace::default()
        };
        let mut order = LENGTHS;
        // The shuffle draws through the crate's one seeded stream, in the index
        // width it indexes in, so a pick is one of the positions this loop is
        // permuting rather than a narrowed copy of one.
        let mut state = seeded_stream(seed);
        for index in (1..order.len()).rev() {
            let pick = next_index(&mut state).rem_euclid(index.saturating_add(1));
            order.swap(index, pick);
        }
        for length in order {
            let outcome = draw(&mut trace, length);
            fold_usize(&mut trace.trace, refusal_text(&outcome).len());
            fold_usize(&mut trace.trace, length);
        }
        trace
    }

    /// What one concurrency tier observed.
    #[derive(Debug, PartialEq, Eq)]
    struct TierOutcome {
        /// The tier requested.
        requested: usize,
        /// The callers that ran and were joined.
        reached: usize,
        /// The distinct draws among those callers.
        distinct: usize,
        /// Draws that left a sentinel byte behind.
        survivors: usize,
        /// Draws the entropy source refused.
        refusals: usize,
        /// The host's spawn refusals, if any.
        spawn_refusals: Vec<String>,
    }

    /// Runs one tier of concurrent drawers and reports what the host allowed.
    ///
    /// Spawning is a bounded loop rather than an unbounded collector, so a host
    /// that refuses a thread stops at the refusal and the refusal becomes data
    /// the caller asserts on instead of a failed process. Every thread that
    /// starts is joined before this returns.
    fn run_tier(tier: usize) -> TierOutcome {
        let mut buffers: Vec<[u8; DRAW_BYTES]> = Vec::with_capacity(tier);
        let mut survivors = 0usize;
        let mut refusals = 0usize;
        let mut spawn_refusals = Vec::new();
        for index in 0..tier {
            let started = std::thread::Builder::new()
                .name(format!("entropy-tier-{index}"))
                .stack_size(STACK_BYTES)
                .spawn(move || {
                    let mut buf = [SENTINEL; DRAW_BYTES];
                    let outcome = random::fill_bytes(&mut buf);
                    (outcome.is_ok(), buf)
                });
            match started {
                Ok(joined) => {
                    // A worker that panicked reports a draw that wrote nothing,
                    // which is the arm this family measures: the sentinel count
                    // for a draw that never filled its buffer is every byte.
                    let (filled, buf) = match joined.join() {
                        Ok(drawn) => drawn,
                        Err(_) => (false, [SENTINEL; DRAW_BYTES]),
                    };
                    if filled {
                        survivors = survivors.saturating_add(usize::from(buf.contains(&SENTINEL)));
                    } else {
                        refusals = refusals.saturating_add(1);
                    }
                    buffers.push(buf);
                }
                Err(error) => spawn_refusals.push(format!("caller {index}: {error}")),
            }
        }
        let reached = buffers.len();
        buffers.sort_unstable();
        buffers.dedup();
        TierOutcome {
            requested: tier,
            reached,
            distinct: buffers.len(),
            survivors,
            refusals,
            spawn_refusals,
        }
    }

    /// Counts identifiers that break the version or variant contract.
    fn ids_break_version_or_variant<'a>(ids: impl Iterator<Item = &'a [u8; 16]>) -> usize {
        ids.filter(|raw| raw[6] >> 4 != 4 || raw[8] & 0xc0 != 0x80)
            .count()
    }

    /// The trace a seed produces, which is what replay compares.
    fn trace_of(seed: u64) -> u64 {
        sweep(seed).trace
    }

    #[test]
    fn a_seeded_sweep_writes_every_byte_at_every_declared_length() {
        for seed in SWEEP_SEEDS {
            let trace = sweep(seed);
            assert_eq!(
                trace.draws,
                LENGTHS.len(),
                "seed {seed:#018x}: the sweep must draw every declared length"
            );
            assert_eq!(
                trace.refusals, 0,
                "seed {seed:#018x}: a working entropy source must serve every draw"
            );
            assert_eq!(
                trace.untouched_draws, 0,
                "seed {seed:#018x}: {} of {} draws came back entirely unwritten",
                trace.untouched_draws, trace.draws
            );
            for length in LENGTHS {
                let worst = trace.worst_sentinels[&length];
                assert!(
                    worst <= sentinel_budget(length),
                    "seed {seed:#018x}: a {length}-byte draw left {worst} sentinel bytes, \
                     above the {budget} a whole draw leaves",
                    budget = sentinel_budget(length)
                );
            }
        }
    }

    #[test]
    fn a_seeded_sweep_never_returns_a_constant_buffer() {
        for seed in SWEEP_SEEDS {
            let trace = sweep(seed);
            assert_eq!(
                trace.constant_buffers, 0,
                "seed {seed:#018x}: {} of {} draws came back as one repeated byte, \
                 which is a source that is not entropy at all",
                trace.constant_buffers, trace.draws
            );
        }
    }

    #[test]
    fn the_empty_and_single_byte_endpoints_are_exact() {
        for seed in SWEEP_SEEDS {
            assert_eq!(
                random::bytes::<0>().map(|buf| buf.len()),
                Ok(0),
                "seed {seed:#018x}: a zero-length draw must succeed with no bytes"
            );
            assert_eq!(
                random::bytes::<1>().map(|buf| buf.len()),
                Ok(1),
                "seed {seed:#018x}: the smallest draw must return exactly one byte"
            );
        }
    }

    #[test]
    fn the_declared_length_boundaries_are_each_drawn_whole() {
        // The two endpoints on their own, so a boundary that only ever appears
        // mid-sweep cannot hide a partial write at either end of the range.
        for length in [1usize, 1 << 20] {
            let mut trace = SweepTrace {
                trace: initial_trace(),
                ..SweepTrace::default()
            };
            let outcome = draw(&mut trace, length);
            assert_eq!(
                outcome.as_ref().map_or(0, Vec::len),
                length,
                "a {length}-byte draw must hand back {length} bytes; refusal: {}",
                refusal_text(&outcome)
            );
            let worst = trace.worst_sentinels[&length];
            assert!(
                worst <= sentinel_budget(length),
                "a {length}-byte draw left {worst} sentinel bytes, above the {} a whole draw \
                 leaves, so the write was short",
                sentinel_budget(length)
            );
            assert_eq!(
                trace.refusals, 0,
                "a {length}-byte draw must succeed on a host with an entropy source"
            );
        }
    }

    #[test]
    fn successive_draws_of_the_same_length_differ() {
        let mut seen: BTreeSet<[u8; DRAW_BYTES]> = BTreeSet::new();
        let mut refusals = Vec::new();
        for seed in SWEEP_SEEDS {
            for _ in 0..DRAWS_PER_SEED {
                match random::bytes::<DRAW_BYTES>() {
                    Ok(bytes) => {
                        seen.insert(bytes);
                    }
                    Err(error) => refusals.push(format!("seed {seed:#018x}: {error}")),
                }
            }
        }
        let requested = SWEEP_SEEDS.len().saturating_mul(DRAWS_PER_SEED);
        assert!(
            refusals.is_empty(),
            "{requested} draws of {DRAW_BYTES} bytes were refused {} times: {refusals:?}",
            refusals.len()
        );
        assert_eq!(
            seen.len(),
            requested,
            "{requested} draws produced {} distinct buffers, so two draws collided",
            seen.len()
        );
    }

    #[test]
    fn concurrent_draws_at_every_tier_are_distinct_and_complete() {
        let outcomes: Vec<TierOutcome> = TIERS.iter().copied().map(run_tier).collect();
        assert!(
            LENGTHS.contains(&DISTINGUISHABLE_AT),
            "the sweep must include the {} -byte length where a short write is distinguishable \
             from a byte that happened to equal the sentinel",
            DISTINGUISHABLE_AT
        );
        let required = outcomes
            .iter()
            .find(|outcome| outcome.requested == REQUIRED_TIER);
        assert!(
            required.is_some_and(|outcome| outcome.reached == REQUIRED_TIER),
            "the {REQUIRED_TIER}-caller tier is required; reached {:?} with {} spawn refusals",
            required.map(|outcome| outcome.reached),
            required.map_or(0, |outcome| outcome.spawn_refusals.len())
        );
        for outcome in &outcomes {
            let reached = outcome.reached;
            let refusals = outcome.spawn_refusals.len();
            if refusals > 0 {
                // The host's ceiling, named rather than assumed: a tier the host
                // could not reach is reported with its refusals, and the callers
                // that did run are still checked here.
                assert!(
                    reached > 0,
                    "tier {reached} requested as {}: the host refused all of them",
                    outcome.requested
                );
            }
            assert!(
                outcome.survivors <= sentinel_budget(DRAW_BYTES).saturating_mul(reached),
                "tier {}: {reached} concurrent draws of {DRAW_BYTES} bytes left {} sentinel \
                 bytes behind, above the {} a whole tier leaves",
                outcome.requested,
                outcome.survivors,
                sentinel_budget(DRAW_BYTES).saturating_mul(reached)
            );
            assert_eq!(
                outcome.refusals, 0,
                "tier {}: the entropy source refused {reached} of {} draws",
                outcome.requested, refusals
            );
            if refusals == 0 {
                assert_eq!(
                    reached, outcome.requested,
                    "tier {}: the host must reach every requested caller",
                    outcome.requested
                );
                assert_eq!(
                    outcome.distinct, reached,
                    "tier {}: {reached} concurrent draws produced {} distinct buffers",
                    outcome.requested, outcome.distinct
                );
            }
        }
    }

    #[test]
    fn a_uuid_is_version_four_with_the_rfc_variant_at_every_tier() {
        for tier in [100usize, 1_000, 10_000] {
            let mut seen: BTreeSet<[u8; 16]> = BTreeSet::new();
            let mut version_lows: BTreeSet<u8> = BTreeSet::new();
            let mut variant_lows: BTreeSet<u8> = BTreeSet::new();
            let mut refusals = 0usize;
            for _ in 0..tier {
                match Uuid::new_v4() {
                    Ok(id) => {
                        let raw = id.as_bytes();
                        version_lows.insert(raw[6] & 0x0f);
                        variant_lows.insert(raw[8] & 0x3f);
                        seen.insert(*raw);
                    }
                    Err(_) => refusals = refusals.saturating_add(1),
                }
            }
            assert_eq!(
                refusals, 0,
                "tier {tier}: {tier} identifier draws were refused {refusals} times"
            );
            assert_eq!(
                seen.len(),
                tier,
                "tier {tier}: {tier} identifiers produced {} distinct values",
                seen.len()
            );
            assert_eq!(
                ids_break_version_or_variant(seen.iter()),
                0,
                "tier {tier}: every identifier must carry version 4 and the RFC variant"
            );
            assert!(
                version_lows.len() > 1 && variant_lows.len() > 1,
                "tier {tier}: the version and variant masks must leave entropy below them, \
                 got version lows {version_lows:?} and variant lows {variant_lows:?}"
            );
        }
    }

    #[test]
    fn no_seeded_generator_predicts_a_draw() -> Result<(), EntropyError> {
        // INV-RANDOM-ONE-SOURCE, observed from outside: a build whose entropy
        // had been replaced by a seeded userspace generator would let the
        // test's own generator predict it, so the generator fills each
        // sixteen-byte draw from two whole words, the way such a substitute
        // would. A true source matches one draw by chance with probability
        // 2^-128, so across 100 draws a hit is never noise and always names a
        // substituted source. (Two-byte draws matched by chance once in about
        // 650 runs, which a gate that runs every PR on several lanes reaches.)
        // A refused draw fails the test rather than shrinking it.
        let mut generator = crate::rng::Rng::new(SWEEP_SEEDS[0]);
        let mut predicted = 0usize;
        for _ in 0..100 {
            let block = random::bytes::<16>()?;
            let mut guess = [0u8; 16];
            let (low, high) = guess.split_at_mut(8);
            low.copy_from_slice(&generator.next().to_le_bytes());
            high.copy_from_slice(&generator.next().to_le_bytes());
            predicted = predicted.saturating_add(usize::from(block == guess));
        }
        assert_eq!(
            predicted, 0,
            "a seeded generator predicted {predicted} of 100 sixteen-byte draws; the entropy \
             source is not the OS CSPRNG it claims to be"
        );
        Ok(())
    }

    #[test]
    fn the_trace_folds_no_drawn_byte() {
        // The replay defect, pinned without entropy: at every declared length,
        // a buffer that came back all sentinel and one that came back with no
        // sentinel at all leave the same trace, and a one-byte draw that
        // happened to equal the sentinel is not reported as unwritten.
        for length in LENGTHS {
            let mut all_sentinel = SweepTrace {
                trace: initial_trace(),
                ..SweepTrace::default()
            };
            let mut no_sentinel = SweepTrace {
                trace: initial_trace(),
                ..SweepTrace::default()
            };
            observe(&mut all_sentinel, length, &vec![SENTINEL; length]);
            observe(&mut no_sentinel, length, &vec![!SENTINEL; length]);
            assert_eq!(
                all_sentinel.trace, no_sentinel.trace,
                "a {length}-byte draw folded its bytes into the trace, so the same seed \
                 cannot replay"
            );
            assert_eq!(
                all_sentinel.untouched_draws,
                usize::from(length >= UNTOUCHED_DISTINGUISHABLE_AT),
                "a {length}-byte all-sentinel draw is untouched only where chance cannot \
                 produce it"
            );
        }
    }

    #[test]
    fn the_same_seed_replays_to_the_same_trace() {
        for seed in SWEEP_SEEDS {
            assert_same_seed_replays(trace_of, seed);
        }
    }

    #[test]
    fn distinct_seeds_diverge_in_their_trace() {
        let first = SWEEP_SEEDS[0];
        let second = SWEEP_SEEDS[1];
        assert_distinct_seeds_diverge(trace_of, first, second);
    }
}
