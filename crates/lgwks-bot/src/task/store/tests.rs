//! A frame whose length prefix lies is refused, and an append that was cut is
//! repaired (#262), driven on the run store's own framing.

use super::record::frame;
use super::*;
use crate::journal::frame::HEAD_BYTES;
use crate::journal::frame::probe::{Scratch, declared_at, frame_starts, with_prefix};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The store file of `count` records, written to `path`, and its bytes.
fn written(path: &Path, count: u8) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let run = RunId::from_hex(&format!("a{}", "0".repeat(31)))?;
    let identity =
        DefinitionIdentity::new("tail", 1, Digest::from_bytes([1; 32]), usize::from(count));
    let mut bytes = STORE_HEADER.to_vec();
    let mut previous = genesis_head();
    for step in 0..count {
        let stored = Stored {
            run,
            key: vec![step; 32],
            tenant: "acme".to_owned(),
            path: format!("s{step}"),
            definition: StoredDefinition::of(&identity),
            value: vec![step; 40 + usize::from(step)],
        };
        let (framed, head) = frame(&stored, &previous)?;
        bytes.extend_from_slice(&framed);
        previous = head;
    }
    std::fs::write(path, &bytes)?;
    Ok(bytes)
}

/// Open `path` and require a refusal as `Corrupt` at frame `at`, with the file
/// byte-identical to `before`.
fn require_refused(path: &Path, before: &[u8], at: u64, why: &str) -> TestResult {
    let outcome: TestResult = match RunStore::open(path) {
        Err(StoreError::Corrupt { at: named }) => {
            assert_eq!(named, at, "{why}");
            Ok(())
        }
        Err(other) => Err(format!("{why}: expected Corrupt, got {other}").into()),
        Ok(opened) => Err(format!(
            "{why}: reopened at {} committed bytes, so an acknowledged frame was lost",
            opened.committed_bytes()
        )
        .into()),
    };
    outcome?;
    assert_eq!(std::fs::read(path)?, before, "{why}: refused bytes move");
    Ok(())
}

/// The acknowledged final record whose prefix grows is the defect: `extra` up to
/// 32 leaves the payload whole and the head short, more ends inside the payload.
#[test]
fn a_lengthened_acknowledged_final_record_is_refused_not_trimmed() -> TestResult {
    let scratch = Scratch::new("store-lengthened")?;
    let bytes = written(scratch.path(), 3)?;
    let last = frame_starts(&bytes, STORE_HEADER.len())?[2];
    let declared = declared_at(&bytes, last);
    for extra in (1u32..=40).chain([100, 255, 1024, 4096]) {
        let lied = with_prefix(&bytes, last, declared + extra);
        std::fs::write(scratch.path(), &lied)?;
        require_refused(scratch.path(), &lied, 2, &format!("final record L+{extra}"))?;
    }
    Ok(())
}

/// An inflated length on a record with records behind it: the true record is still
/// first behind the prefix, so it authenticates and the frames after it are kept.
#[test]
fn an_inflated_record_with_records_behind_it_is_refused_untouched() -> TestResult {
    let scratch = Scratch::new("store-inflated")?;
    let bytes = written(scratch.path(), 3)?;
    let middle = frame_starts(&bytes, STORE_HEADER.len())?[1];
    let remaining = u32::try_from(bytes.len() - middle)?;
    for declared in [remaining, remaining + 1, remaining + 31, remaining + 500] {
        let lied = with_prefix(&bytes, middle, declared);
        std::fs::write(scratch.path(), &lied)?;
        require_refused(scratch.path(), &lied, 1, &format!("declared {declared}"))?;
    }
    Ok(())
}

/// A lengthened prefix over a record whose own bytes are also damaged still
/// refuses, because the record behind it authenticates.
#[test]
fn a_damaged_cut_record_with_an_acknowledged_one_behind_it_is_refused() -> TestResult {
    let scratch = Scratch::new("store-damaged")?;
    let bytes = written(scratch.path(), 3)?;
    let middle = frame_starts(&bytes, STORE_HEADER.len())?[1];
    let remaining = u32::try_from(bytes.len() - middle)?;
    let mut lied = with_prefix(&bytes, middle, remaining + 7);
    if let Some(byte) = lied.get_mut(middle + 12) {
        *byte ^= 0x55;
    }
    std::fs::write(scratch.path(), &lied)?;
    require_refused(scratch.path(), &lied, 1, "damaged middle, lengthened")
}

/// The control: an append cut at any place in the final record is still repaired,
/// back to exactly the acknowledged prefix. Sampled at each boundary and between
/// them, since each repair is an `fsync`.
#[test]
fn an_append_cut_inside_the_final_record_is_repaired() -> TestResult {
    let scratch = Scratch::new("store-cut")?;
    let bytes = written(scratch.path(), 3)?;
    let last = frame_starts(&bytes, STORE_HEADER.len())?[2];
    let kept = u64::try_from(last)?;
    let whole = bytes.len() - last;
    for cut in [
        1,
        3,
        4,
        5,
        whole >> 1,
        whole - 33,
        whole - 32,
        whole - 31,
        whole - 1,
    ] {
        std::fs::write(
            scratch.path(),
            bytes.get(..last + cut).ok_or("cut past the end")?,
        )?;
        let store = RunStore::open(scratch.path())?;
        assert_eq!(
            store.committed_bytes(),
            kept,
            "cut {cut}: the two records survive"
        );
        drop(store);
        assert_eq!(std::fs::metadata(scratch.path())?.len(), kept, "cut {cut}");
    }
    Ok(())
}

/// The worst tail a repair can be asked to examine, a full-ceiling prefix over
/// noise, is decided in bounded time: the first search tries a payload length per
/// byte and each try decodes, so the cost is the thing to bound.
#[test]
fn a_ceiling_sized_noise_tail_is_trimmed_in_bounded_time() -> TestResult {
    let scratch = Scratch::new("store-noise")?;
    let mut bytes = written(scratch.path(), 1)?;
    let kept = u64::try_from(bytes.len())?;
    bytes.extend_from_slice(&u32::try_from(MAX_RECORD_BYTES)?.to_be_bytes());
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..MAX_RECORD_BYTES + HEAD_BYTES - 1 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        bytes.push(state.to_be_bytes()[0]);
    }
    std::fs::write(scratch.path(), &bytes)?;
    let started = std::time::Instant::now();
    let store = RunStore::open(scratch.path())?;
    let elapsed = started.elapsed();
    assert_eq!(
        store.committed_bytes(),
        kept,
        "noise holds no acknowledged record"
    );
    assert!(elapsed.as_secs() < 5, "the search took {elapsed:?}");
    Ok(())
}
