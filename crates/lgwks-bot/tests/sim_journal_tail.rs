//! Simulation family: a length prefix that lies about an acknowledged frame (#262).
//!
//! Every scenario drives the **real** `FileJournal::open` over a file the real
//! journal wrote, through the public interface only. The seed decides how many
//! frames the journal holds (one to a thousand), which frame is damaged, how, and
//! where a file is cut; the property is the one the journal's whole durability
//! claim rests on: **an open never yields fewer acknowledged frames than the file
//! still holds unless it refuses, and a refusal never touches a byte.**
//!
//! | Family | Property it pins |
//! |---|---|
//! | `lying_lengths` | any one-fault change to a length prefix is a typed refusal with the file byte-identical |
//! | `cut_appends` | a file cut at any byte reopens with exactly the whole frames before the cut |
//! | `damaged_cut_frames` | a lying length over a damaged frame is still refused while an acknowledged frame follows it |
//! | `tenants_beside_a_refusal` | one tenant's refused journal does not move another tenant's, opened concurrently |
//!
//! A one-fault length change is never a torn append: a writer leaves a prefix of
//! the bytes it meant, so the length it finished is the length it intended.
//! The one shape this does **not** claim, a final frame whose length *and* head
//! are both damaged, is indistinguishable from a cut append and is pinned by a
//! unit test beside the resolver rather than asserted away here.
//!
//! Same seed, same trace hash, because each declared test sweeps its band twice
//! through [`sim::assert_replays`].

mod sim;

#[path = "sim/bands.rs"]
mod band_family;

use std::error::Error;
use std::path::Path;

use lgwks_bot::journal::{FileJournal, JournalError};

use sim::Band;
use sim::rig::ladder;

type TestResult = Result<(), Box<dyn Error>>;

/// The width of a frame's length prefix and of its stored head.
const LENGTH_BYTES: usize = 4;
const HEAD_BYTES: usize = 32;

/// The largest payload the journal frames; a declared length past it is refused as
/// rot before any tail question is asked, so it is outside what this family tests.
const MAX_FRAME_BYTES: u32 = 64 * 1024;

/// The big-endian length prefix at `at` in `bytes`, zero where the bytes end.
fn prefix_at(bytes: &[u8], at: usize) -> u32 {
    let mut prefix = [0u8; LENGTH_BYTES];
    for (slot, byte) in prefix.iter_mut().zip(bytes.iter().skip(at)) {
        *slot = *byte;
    }
    u32::from_be_bytes(prefix)
}

/// One journal the real writer produced, and the frame boundaries inside it.
struct Written {
    bytes: Vec<u8>,
    starts: Vec<usize>,
}

impl Written {
    fn frames(&self) -> usize {
        self.starts.len()
    }

    fn start_of(&self, index: usize) -> usize {
        self.starts.get(index).copied().unwrap_or(0)
    }

    /// The offset one past the last byte of frame `index`.
    fn end_of(&self, index: usize) -> usize {
        self.starts
            .get(index.saturating_add(1))
            .copied()
            .unwrap_or(self.bytes.len())
    }

    fn declared(&self, index: usize) -> u32 {
        prefix_at(&self.bytes, self.start_of(index))
    }

    /// How many bytes follow frame `index`'s length prefix to the end of the file.
    fn behind(&self, index: usize) -> Result<u32, Box<dyn Error>> {
        let after_prefix = self.start_of(index).saturating_add(LENGTH_BYTES);
        Ok(u32::try_from(
            self.bytes.len().saturating_sub(after_prefix),
        )?)
    }

    /// The file with frame `index`'s length prefix replaced.
    fn with_declared(&self, index: usize, declared: u32) -> Vec<u8> {
        let mut out = self.bytes.clone();
        for (slot, byte) in out
            .iter_mut()
            .skip(self.start_of(index))
            .zip(declared.to_be_bytes())
        {
            *slot = byte;
        }
        out
    }
}

/// Write a seeded journal through the shipped journal: one to a thousand frames,
/// mostly a handful, so the sweep is wide in count and cheap on average.
fn written_journal(
    sim: &mut sim::Sim,
    path: &Path,
    first_attempt: u64,
) -> Result<Written, Box<dyn Error>> {
    let attempts = if sim.rng().chance(30) {
        sim.rng().between(100, 500)
    } else {
        sim.rng().between(1, 8)
    };
    let mut events = Vec::new();
    for offset in 0..u64::from(attempts) {
        events.extend(ladder(first_attempt.saturating_add(offset))?);
    }
    let mut journal = FileJournal::open(path)?;
    journal.compare_and_append_all(&events)?;
    drop(journal);
    let bytes = std::fs::read(path)?;
    let mut starts = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        starts.push(at);
        let payload = usize::try_from(prefix_at(&bytes, at))?;
        at = at
            .saturating_add(LENGTH_BYTES)
            .saturating_add(payload)
            .saturating_add(HEAD_BYTES);
    }
    Ok(Written { bytes, starts })
}

/// A seeded frame index.
fn some_frame(sim: &mut sim::Sim, frames: usize) -> Result<usize, Box<dyn Error>> {
    Ok(usize::try_from(sim.rng().below(u32::try_from(frames)?))?)
}

/// Replace frame `index`'s length with `declared` on disk, then require the open
/// to refuse it as corruption and leave exactly those bytes behind.
fn refuse_declared(
    path: &Path,
    written: &Written,
    index: usize,
    declared: u32,
    why: &str,
) -> TestResult {
    let lied = written.with_declared(index, declared);
    refuse_bytes(path, &lied, why)
}

/// Put `lied` on disk, then require the open to refuse it as corruption and leave
/// exactly those bytes behind.
fn refuse_bytes(path: &Path, lied: &[u8], why: &str) -> TestResult {
    std::fs::write(path, lied)?;
    match FileJournal::open(path) {
        Err(JournalError::Corrupt(_)) => {}
        Err(other) => {
            return Err(format!("{why}: expected a corruption refusal, got {other}").into());
        }
        Ok(opened) => {
            return Err(format!(
                "{why}: reopened with {} events, so an acknowledged frame was lost",
                opened.events().count()
            )
            .into());
        }
    }
    assert_eq!(
        std::fs::read(path)?,
        lied,
        "{why}: a refusal rewrote the file"
    );
    Ok(())
}

/// A length for frame `index` that runs past the end of the file, seeded, or `None`
/// when the draw does not actually exceed the frame's true length.
fn inflated_past_the_end(
    sim: &mut sim::Sim,
    written: &Written,
    index: usize,
) -> Result<Option<u32>, Box<dyn Error>> {
    let declared = written
        .behind(index)?
        .saturating_add(sim.rng().between(0, 600))
        .min(MAX_FRAME_BYTES);
    Ok((declared > written.declared(index)).then_some(declared))
}

/// Any one-fault change to a length prefix is refused and nothing is touched.
fn lying_lengths(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("lying")?;
        let path = sim.journal_path(&dir, 0);
        let written = written_journal(sim, &path, 1)?;
        let frames = written.frames();
        let last = frames.saturating_sub(1);
        let mut cases = 0u64;

        // The final frame lengthened by every amount in the head-short and
        // payload-short ranges, then a seeded spread above it.
        let mut extras: Vec<u32> = (1..=40).collect();
        extras.extend((0..16).map(|_| sim.rng().between(41, 4096)));
        for extra in extras {
            let declared = written.declared(last).saturating_add(extra);
            refuse_declared(
                &path,
                &written,
                last,
                declared,
                &format!("seed {} final L+{extra}", sim.seed),
            )?;
            cases = cases.saturating_add(1);
        }

        // A seeded frame, final or not, inflated past the end of the file.
        for _ in 0..8 {
            let index = some_frame(sim, frames)?;
            if let Some(declared) = inflated_past_the_end(sim, &written, index)? {
                refuse_declared(
                    &path,
                    &written,
                    index,
                    declared,
                    &format!("seed {} frame {index} -> {declared}", sim.seed),
                )?;
                cases = cases.saturating_add(1);
            }
        }

        // Deflated and bit-flipped prefixes on a seeded frame.
        for _ in 0..8 {
            let index = some_frame(sim, frames)?;
            let true_len = written.declared(index);
            let deflated = sim.rng().between(1, true_len.saturating_sub(1).max(1));
            let flipped = true_len ^ (1u32 << sim.rng().below(16));
            for (declared, name) in [(deflated, "deflated"), (flipped, "flipped")] {
                if declared != true_len && declared != 0 {
                    refuse_declared(
                        &path,
                        &written,
                        index,
                        declared,
                        &format!("seed {} frame {index} {name}", sim.seed),
                    )?;
                    cases = cases.saturating_add(1);
                }
            }
        }

        sim.record("lying-lengths-refused-untouched");
        sim.trace.record_count("lying-frames", frames);
        sim.trace.record_u64("lying-cases", cases);
        Ok(())
    })
}

/// A file cut at any byte reopens with exactly the whole frames before the cut.
fn cut_appends(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("cut")?;
        let path = sim.journal_path(&dir, 0);
        let written = written_journal(sim, &path, 1)?;
        let frames = written.frames();
        let total = written.bytes.len();

        // A repair is an fsync, so the sweep is a seeded sample rather than every
        // byte: the first and last byte of the final frame, which are the two edges
        // of "cut inside it", and a seeded handful anywhere in the file. Every byte
        // of a final frame is swept by the unit test beside the resolver.
        let final_start = written.start_of(frames.saturating_sub(1));
        let mut cuts = vec![final_start.saturating_add(1), total.saturating_sub(1)];
        for _ in 0..10 {
            cuts.push(usize::try_from(sim.rng().below(u32::try_from(total)?))?);
        }
        let mut repaired = 0u64;
        for cut in &cuts {
            std::fs::write(&path, &written.bytes[..*cut])?;
            let whole = (0..frames)
                .filter(|index| written.end_of(*index) <= *cut)
                .count();
            let boundary = whole
                .checked_sub(1)
                .map_or(0, |index| written.end_of(index));
            let reopened = FileJournal::open(&path).map_err(|error| {
                format!(
                    "seed {} cut at {cut}/{total} was refused: {error}",
                    sim.seed
                )
            })?;
            assert_eq!(
                reopened.events().count(),
                whole,
                "seed {} cut at {cut}/{total} did not keep exactly the whole frames before it",
                sim.seed
            );
            assert_eq!(
                reopened.torn_tail_repaired(),
                *cut != boundary,
                "seed {} cut at {cut}: the repair flag disagrees with the cut",
                sim.seed
            );
            drop(reopened);
            assert_eq!(
                std::fs::read(&path)?,
                written.bytes[..boundary],
                "seed {} cut at {cut}: the open left something other than the acknowledged prefix",
                sim.seed
            );
            repaired = repaired.saturating_add(u64::from(*cut != boundary));
        }
        sim.record("cut-appends-keep-the-whole-frames");
        sim.trace.record_count("cut-frames", frames);
        sim.trace.record_count("cut-cases", cuts.len());
        sim.trace.record_u64("cut-repaired", repaired);
        Ok(())
    })
}

/// A lying length over a damaged frame is still refused while an acknowledged
/// frame follows it: failing to authenticate one candidate is not proof that none
/// exists in the suffix.
fn damaged_cut_frames(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("damaged")?;
        let path = sim.journal_path(&dir, 0);
        let mut written = written_journal(sim, &path, 1)?;
        // A journal needs a frame behind the damaged one.
        while written.frames() < 2 {
            written = written_journal(sim, &path, 1)?;
        }
        let frames = written.frames();
        let mut cases = 0u64;
        for _ in 0..16 {
            let index = some_frame(sim, frames.saturating_sub(1))?;
            let Some(declared) = inflated_past_the_end(sim, &written, index)? else {
                continue;
            };
            let mut lied = written.with_declared(index, declared);
            // One byte of the cut frame's own payload or head, damaged as well.
            let span = usize::try_from(written.declared(index))?.saturating_add(HEAD_BYTES);
            let hit = written
                .start_of(index)
                .saturating_add(LENGTH_BYTES)
                .saturating_add(usize::try_from(sim.rng().below(u32::try_from(span)?))?);
            if let Some(byte) = lied.get_mut(hit) {
                *byte ^= 0xa5;
            }
            refuse_bytes(
                &path,
                &lied,
                &format!("seed {} frame {index} damaged and lengthened", sim.seed),
            )?;
            cases = cases.saturating_add(1);
        }
        sim.record("damaged-cut-frames-refused");
        sim.trace.record_count("damaged-frames", frames);
        sim.trace.record_u64("damaged-cases", cases);
        Ok(())
    })
}

/// One tenant's journal on disk.
struct Tenant {
    path: std::path::PathBuf,
    written: Written,
}

/// Open every tenant's journal at once, one thread each, and report what each held.
fn open_concurrently(tenants: &[Tenant]) -> Vec<Result<usize, JournalError>> {
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for tenant in tenants {
            handles.push(scope.spawn(move || {
                FileJournal::open(&tenant.path).map(|opened| opened.events().count())
            }));
        }
        let mut outcomes = Vec::new();
        for handle in handles {
            outcomes.push(handle.join().unwrap_or_else(|_| {
                Err(JournalError::Storage(std::io::Error::other(
                    "an opener panicked",
                )))
            }));
        }
        outcomes
    })
}

/// One tenant's refused journal does not move another's, opened concurrently.
fn tenants_beside_a_refusal(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let dir = sim.scratch("tenants")?;
        let count = usize::try_from(sim.rng().between(2, 8))?;
        let mut tenants = Vec::new();
        for tenant in 0..count {
            let path = sim.journal_path(&dir, u32::try_from(tenant)?);
            let first = u64::try_from(tenant)?
                .saturating_mul(1000)
                .saturating_add(1);
            let written = written_journal(sim, &path, first)?;
            tenants.push(Tenant { path, written });
        }
        // Tenant zero's final frame is lengthened; every other tenant is untouched.
        let victim = tenants.first().ok_or("no tenants")?;
        let last = victim.written.frames().saturating_sub(1);
        let declared = victim
            .written
            .declared(last)
            .saturating_add(sim.rng().between(1, 64));
        let lied = victim.written.with_declared(last, declared);
        std::fs::write(&victim.path, &lied)?;

        let outcomes = open_concurrently(&tenants);
        for (index, (tenant, outcome)) in tenants.iter().zip(&outcomes).enumerate() {
            if index == 0 {
                assert!(
                    matches!(outcome, Err(JournalError::Corrupt(_))),
                    "seed {}: the victim was not refused: {outcome:?}",
                    sim.seed
                );
                assert_eq!(
                    std::fs::read(&tenant.path)?,
                    lied,
                    "seed {}: the refusal rewrote the victim",
                    sim.seed
                );
            } else {
                assert_eq!(
                    outcome.as_ref().ok().copied(),
                    Some(tenant.written.frames()),
                    "seed {}: tenant {index} was moved by another tenant's refusal",
                    sim.seed
                );
                assert_eq!(
                    std::fs::read(&tenant.path)?,
                    tenant.written.bytes,
                    "seed {}: tenant {index}'s file was rewritten",
                    sim.seed
                );
            }
        }
        sim.record("a-refusal-stayed-with-its-tenant");
        sim.trace.record_count("tenants", count);
        Ok(())
    })
}

band_family::band_family! {
    lying_lengths_band_00 => lying_lengths, 0;
    lying_lengths_band_01 => lying_lengths, 1;
    lying_lengths_band_02 => lying_lengths, 2;
    lying_lengths_band_03 => lying_lengths, 3;
    cut_appends_band_04 => cut_appends, 4;
    cut_appends_band_05 => cut_appends, 5;
    cut_appends_band_06 => cut_appends, 6;
    cut_appends_band_07 => cut_appends, 7;
    damaged_cut_frames_band_08 => damaged_cut_frames, 8;
    damaged_cut_frames_band_09 => damaged_cut_frames, 9;
    damaged_cut_frames_band_10 => damaged_cut_frames, 10;
    damaged_cut_frames_band_11 => damaged_cut_frames, 11;
    tenants_beside_a_refusal_band_12 => tenants_beside_a_refusal, 12;
    tenants_beside_a_refusal_band_13 => tenants_beside_a_refusal, 13;
}
