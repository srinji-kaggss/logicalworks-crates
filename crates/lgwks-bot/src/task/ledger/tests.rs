//! The run ledger's tail rules: a lying prefix over an acknowledged entry is
//! refused untouched, and only a torn final append is repaired.

use super::*;
use crate::journal::frame::probe::{Scratch, declared_at, frame_starts, with_prefix};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The ledger file of `count` charges, written to `path`, and its bytes.
fn written(path: &Path, count: u64) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let run = RunId::from_hex(&format!("b{}", "0".repeat(31)))?;
    let mut bytes = LEDGER_MAGIC.to_vec();
    let mut previous = genesis_head();
    for charge in 1..=count {
        let entry = Entry {
            run,
            tenant: "acme".to_owned(),
            attempts: charge,
            spend: charge.saturating_mul(3),
            epoch: charge,
            applied: Some(vec![u8::try_from(charge)?; 24]),
        };
        let (framed, head) = frame(&entry, &previous)?;
        bytes.extend_from_slice(&framed);
        previous = head;
    }
    std::fs::write(path, &bytes)?;
    Ok(bytes)
}

/// An acknowledged final entry whose prefix grows is refused as `Corrupt` at
/// its index, and the file is byte-identical afterwards.
#[test]
fn a_lengthened_acknowledged_final_entry_is_refused_not_trimmed() -> TestResult {
    let scratch = Scratch::new("ledger-lengthened")?;
    let bytes = written(scratch.path(), 3)?;
    let last = frame_starts(&bytes, LEDGER_MAGIC.len())?[2];
    let declared = declared_at(&bytes, last);
    for extra in (1u32..=40).chain([100, 1024]) {
        let lied = with_prefix(&bytes, last, declared + extra);
        std::fs::write(scratch.path(), &lied)?;
        assert!(
            matches!(
                RunLedger::open(scratch.path()),
                Err(StoreError::Corrupt { at: 2 })
            ),
            "L+{extra}: an acknowledged entry must be refused as corrupt at 2"
        );
        assert_eq!(
            std::fs::read(scratch.path())?,
            lied,
            "L+{extra}: bytes move"
        );
    }
    Ok(())
}

/// A lying prefix over an entry that is itself damaged still refuses: the entry
/// behind it authenticates, from the head stored before it, at a payload offset
/// the archive was not written at.
#[test]
fn a_damaged_lengthened_entry_with_an_acknowledged_one_behind_it_is_refused() -> TestResult {
    let scratch = Scratch::new("ledger-damaged")?;
    let bytes = written(scratch.path(), 3)?;
    let middle = frame_starts(&bytes, LEDGER_MAGIC.len())?[1];
    let mut lied = with_prefix(&bytes, middle, u32::try_from(bytes.len() - middle)? + 9);
    if let Some(byte) = lied.get_mut(middle + 10) {
        *byte ^= 0x55;
    }
    std::fs::write(scratch.path(), &lied)?;
    assert!(matches!(
        RunLedger::open(scratch.path()),
        Err(StoreError::Corrupt { at: 1 })
    ));
    assert_eq!(std::fs::read(scratch.path())?, lied, "refused bytes move");
    Ok(())
}

/// The control: an append cut inside the final entry is repaired to exactly the
/// acknowledged prefix.
#[test]
fn an_append_cut_inside_the_final_entry_is_repaired() -> TestResult {
    let scratch = Scratch::new("ledger-cut")?;
    let bytes = written(scratch.path(), 3)?;
    let last = frame_starts(&bytes, LEDGER_MAGIC.len())?[2];
    let whole = bytes.len() - last;
    // Inside the prefix, inside the payload, and on each side of the 32-byte
    // head that closes the frame.
    let cuts = [1, 3, 4, 5, whole >> 1, whole - 33, whole - 32, whole - 1];
    for cut in cuts {
        assert_eq!(
            length_after_reopening(scratch.path(), &bytes, last + cut)?,
            u64::try_from(last)?,
            "cut {cut}: repaired to the acknowledged prefix"
        );
    }
    Ok(())
}

/// Leave only the first `end` bytes of `bytes` at `path`, as a crash mid-append
/// would, open the ledger over them (which repairs a torn tail), and report the
/// file's length afterwards.
fn length_after_reopening(
    path: &Path,
    bytes: &[u8],
    end: usize,
) -> Result<u64, Box<dyn std::error::Error>> {
    let torn = bytes
        .get(..end)
        .ok_or("the cut lies past the end of the file")?;
    std::fs::write(path, torn)?;
    drop(RunLedger::open(path)?);
    Ok(std::fs::metadata(path)?.len())
}
