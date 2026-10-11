//! Seeded corruption of the cross-host grant channel's frames (#319).
//!
//! One seed drives which byte of a genuine acquire frame is damaged and how:
//! a single flipped bit (a noisy link or a tampering relay, CWE-345) or a cut
//! (a dropped connection mid-frame). Every damaged frame goes to a real
//! [`AuthorityServer`](lgwks_bot::dist_lease::AuthorityServer) over loopback.
//!
//! The invariants held for every frame of every seed:
//!
//! - **No damaged frame is answered.** A flipped bit breaks the magic, the op,
//!   the length or the keyed tag; a cut ends the read. Each is refused with
//!   no reply.
//! - **No damaged frame moves the epoch.** The authority's epoch stays zero, so
//!   corruption can never mint a lease or stale one.
//! - **Exact replay.** The same seed gives the same trace hash, and two seeds
//!   diverge.

use lgwks_bot::dist_lease::AuthorityHandle;
use lgwks_std::hash::Hasher;
use lgwks_std::seeded::Seeded;

use crate::dist_lease_tcp::{TestResult, captured_acquire, send_raw, with_server};

/// Damaged frames sent per seed.
const FRAMES: usize = 16;

/// Seeds the sweep runs.
const SEEDS: u64 = 64;

/// Damage one frame as the seed directs: a bit flip at a drawn offset, or a
/// cut at a drawn length. Returns the damaged frame and its trace record.
fn damage(stream: &mut Seeded, genuine: &[u8]) -> TestResult<(Vec<u8>, [u8; 9])> {
    let offset = stream.index(genuine.len())?;
    let mut record = [0u8; 9];
    record[1..].copy_from_slice(&u64::try_from(offset)?.to_le_bytes());
    if stream.below(4)? == 0 {
        record[0] = 0xff;
        return Ok((
            genuine
                .get(..offset)
                .map(<[u8]>::to_vec)
                .ok_or("cut past the frame")?,
            record,
        ));
    }
    let bit = u8::try_from(stream.below(8)?)?;
    record[0] = bit;
    let mut frame = genuine.to_vec();
    let byte = frame.get_mut(offset).ok_or("offset past the frame")?;
    *byte ^= 1u8
        .checked_shl(u32::from(bit))
        .ok_or("a bit past the byte")?;
    Ok((frame, record))
}

/// Run one seed against a fresh authority and return its trace hash.
fn run(seed: u64, genuine: &[u8]) -> TestResult<String> {
    let authority = AuthorityHandle::new();
    let mut stream = Seeded::from_seed(seed);
    let mut trace = Hasher::new();
    trace.update(&seed.to_le_bytes());
    let ((), report) = with_server(&authority, FRAMES, |addr| {
        for _ in 0..FRAMES {
            let (frame, record) = damage(&mut stream, genuine)?;
            let reply = send_raw(addr, &frame)?;
            assert!(
                reply.is_empty(),
                "seed {seed}: a damaged frame {record:?} was answered"
            );
            trace.update(&record);
        }
        Ok(())
    })?;
    assert_eq!(
        report.refused(),
        FRAMES,
        "seed {seed}: every damaged frame must be refused"
    );
    assert_eq!(
        report.answered(),
        0,
        "seed {seed}: no damaged frame may be answered"
    );
    assert_eq!(
        authority.epoch(),
        0,
        "seed {seed}: corruption must never move the epoch"
    );
    Ok(trace.finalize().to_hex())
}

/// Every seed's damaged frames are refused, the epoch never moves, and the
/// trace replays exactly.
#[test]
fn sim_damaged_frames_are_refused_and_replay() -> TestResult {
    let genuine = captured_acquire("coord-a")?;
    for seed in 0..SEEDS {
        run(seed, &genuine)?;
    }
    assert_eq!(
        run(7, &genuine)?,
        run(7, &genuine)?,
        "the same seed must replay exactly"
    );
    assert_ne!(
        run(0, &genuine)?,
        run(1, &genuine)?,
        "two seeds must diverge"
    );
    Ok(())
}
