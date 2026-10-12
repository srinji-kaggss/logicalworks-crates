//! A damaged record-store file is refused, never read as an empty store.
//!
//! The deterministic companion to `sim_cas_disk`: each seed builds a real
//! store through the shipped [`RecordStore`], then damages the file the way a
//! disk or a foreign writer would — truncation, an overwritten structural
//! byte, an empty file, a foreign tag, a record or an eviction count the
//! store never writes, a top level that is not a document, a directory where
//! the file should be. The contract under test is the store's own: a file
//! that is present but is not this store's state is
//! [`CasError::Corrupt`], an unreadable path is [`CasError::Io`], and neither
//! resurrects records at version zero.
//!
//! What is out of reach, said plainly: the store carries no checksum, so a
//! byte flipped *inside* a value string decodes as a different value. Only
//! damage to the document's structure is held to a refusal here.

use std::path::Path;

use lgwks_bot::cas::{CasError, RecordStore};
use lgwks_std::json::Value;

use crate::sim::{Rng, Trace};
use crate::sim_cas_disk::{SEEDS, TestResult, seeded_store, sweep};
use crate::sweep_fixtures::refuse;

/// Open `path` and require a corruption refusal, with the file left exactly
/// as it was found.
fn refused_as_corrupt(path: &Path, trace: &mut Trace) -> TestResult {
    let before = std::fs::read(path)?;
    match RecordStore::open(path) {
        Err(CasError::Corrupt { .. }) => {}
        Err(error) => return refuse(format!("a damaged store failed as {error}, not as corrupt")),
        Ok(store) => {
            return refuse(format!(
                "a damaged store opened with {} records",
                store.state().records().len()
            ));
        }
    }
    if std::fs::read(path)? != before {
        return refuse("a refused open rewrote the damaged file");
    }
    trace.record_number("refused bytes", before.len());
    Ok(())
}

/// The offsets of the document's structural bytes: braces, brackets, colons
/// and commas outside every string.
fn structural_offsets(bytes: &[u8]) -> Vec<usize> {
    let mut inside = false;
    let mut escaped = false;
    let mut offsets = Vec::new();
    for (at, &byte) in bytes.iter().enumerate() {
        if inside {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                inside = false;
            }
            continue;
        }
        match byte {
            b'"' => inside = true,
            b'{' | b'}' | b'[' | b']' | b':' | b',' => offsets.push(at),
            _ => {}
        }
    }
    offsets
}

/// A seeded index below `len`.
fn index_below(rng: &mut Rng, len: usize) -> Result<usize, Box<dyn std::error::Error>> {
    Ok(usize::try_from(rng.below(u32::try_from(len)?))?)
}

/// The seeded store's document, parsed.
fn document(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    Ok(lgwks_std::json::from_str(&std::fs::read_to_string(path)?)?)
}

/// Write `value` back over the store file.
fn rewrite(path: &Path, value: &Value) -> TestResult {
    std::fs::write(path, lgwks_std::json::to_string_pretty(value)?)?;
    Ok(())
}

/// Any strict prefix of a store file is refused as corrupt.
#[test]
fn a_truncated_store_is_refused_as_corrupt() -> TestResult {
    sweep("truncated", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let (_dir, path, _model, _records) = seeded_store(&mut rng, "sim-cas-trunc", trace)?;
        let whole = std::fs::read(&path)?;
        let cut = index_below(&mut rng, whole.len())?;
        std::fs::write(
            &path,
            whole
                .get(..cut)
                .ok_or("a cut below the length is in range")?,
        )?;
        trace.record_number("cut", cut);
        refused_as_corrupt(&path, trace)
    })
}

/// One structural byte overwritten is refused as corrupt.
#[test]
fn an_overwritten_structural_byte_is_refused_as_corrupt() -> TestResult {
    sweep("structural", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let (_dir, path, _model, _records) = seeded_store(&mut rng, "sim-cas-struct", trace)?;
        let mut bytes = std::fs::read(&path)?;
        let offsets = structural_offsets(&bytes);
        let pick = index_below(&mut rng, offsets.len())?;
        let at = *offsets.get(pick).ok_or("a structural offset in range")?;
        *bytes.get_mut(at).ok_or("the offset is inside the file")? = b'#';
        std::fs::write(&path, &bytes)?;
        trace.record_number("overwritten at", at);
        refused_as_corrupt(&path, trace)
    })
}

/// An empty or whitespace-only file is refused, not read as a new store.
#[test]
fn an_empty_store_file_is_refused_as_corrupt() -> TestResult {
    sweep("empty", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let (_dir, path, _model, _records) = seeded_store(&mut rng, "sim-cas-empty", trace)?;
        let blank = " \n\t".repeat(usize::try_from(rng.below(6))?);
        std::fs::write(&path, &blank)?;
        refused_as_corrupt(&path, trace)
    })
}

/// A document under any other tag, or none, is refused.
#[test]
fn a_foreign_document_tag_is_refused() -> TestResult {
    sweep("tag", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let (_dir, path, _model, _records) = seeded_store(&mut rng, "sim-cas-tag", trace)?;
        let mut value = document(&path)?;
        let object = value.as_object_mut().ok_or("the store is an object")?;
        let choice = rng.below(4);
        match choice {
            0 => drop(object.remove("cas")),
            1 => drop(object.insert("cas".to_owned(), Value::from("lgwks-bot-cas/2"))),
            2 => drop(object.insert("cas".to_owned(), Value::from(1_u64))),
            _ => drop(object.insert("cas".to_owned(), Value::from("LGWKS-BOT-CAS/1"))),
        }
        trace.record_number("tag variant", choice);
        rewrite(&path, &value)?;
        refused_as_corrupt(&path, trace)
    })
}

/// A record whose version is missing, zero or not a number is refused.
#[test]
fn a_record_without_a_positive_version_is_refused() -> TestResult {
    sweep("version", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let (_dir, path, _model, _records) = seeded_store(&mut rng, "sim-cas-version", trace)?;
        let mut value = document(&path)?;
        let records = value
            .get_mut("records")
            .and_then(Value::as_object_mut)
            .ok_or("the store holds a records object")?;
        let names: Vec<String> = records.keys().cloned().collect();
        let name = names
            .get(index_below(&mut rng, names.len())?)
            .ok_or("a record name in range")?;
        let entry = records
            .get_mut(name)
            .and_then(Value::as_object_mut)
            .ok_or("every record is an object")?;
        let choice = rng.below(3);
        match choice {
            0 => drop(entry.remove("version")),
            1 => drop(entry.insert("version".to_owned(), Value::from(0_u64))),
            _ => drop(entry.insert("version".to_owned(), Value::from("1"))),
        }
        trace.record_number("version variant", choice);
        rewrite(&path, &value)?;
        refused_as_corrupt(&path, trace)
    })
}

/// A document without a numeric eviction count is refused.
#[test]
fn a_document_without_an_eviction_count_is_refused() -> TestResult {
    sweep("evicted", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let (_dir, path, _model, _records) = seeded_store(&mut rng, "sim-cas-evicted", trace)?;
        let mut value = document(&path)?;
        let object = value.as_object_mut().ok_or("the store is an object")?;
        let choice = rng.below(3);
        match choice {
            0 => drop(object.remove("evicted")),
            1 => drop(object.insert("evicted".to_owned(), Value::from(-1_i64))),
            _ => drop(object.insert("evicted".to_owned(), Value::from("0"))),
        }
        trace.record_number("evicted variant", choice);
        rewrite(&path, &value)?;
        refused_as_corrupt(&path, trace)
    })
}

/// A file holding valid JSON that is not an object is refused.
#[test]
fn a_top_level_that_is_not_an_object_is_refused() -> TestResult {
    sweep("top-level", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let (_dir, path, _model, _records) = seeded_store(&mut rng, "sim-cas-top", trace)?;
        let value = document(&path)?;
        let choice = rng.below(5);
        let replaced = match choice {
            0 => Value::Array(vec![value]),
            1 => Value::from(rng.below(1_000_000)),
            2 => Value::from("lgwks-bot-cas/1"),
            3 => Value::Null,
            _ => Value::Bool(true),
        };
        trace.record_number("top-level variant", choice);
        rewrite(&path, &replaced)?;
        refused_as_corrupt(&path, trace)
    })
}

/// A directory where the store file should be is an I/O refusal: neither an
/// empty store nor a corrupt one.
#[test]
fn a_directory_at_the_store_path_is_an_io_refusal() -> TestResult {
    sweep("directory", SEEDS, |seed, trace| {
        let mut rng = Rng::new(seed);
        let (_dir, path, _model, _records) = seeded_store(&mut rng, "sim-cas-dir", trace)?;
        std::fs::remove_file(&path)?;
        let depth = rng.between(0, 3);
        let mut inner = path.clone();
        for level in 0..depth {
            inner = inner.join(format!("level-{level}"));
        }
        std::fs::create_dir_all(&inner)?;
        match RecordStore::open(&path) {
            Err(CasError::Io { .. }) => {}
            Err(error) => {
                return refuse(format!("a directory at the store path failed as {error}"));
            }
            Ok(_) => return refuse("a directory at the store path opened as a store"),
        }
        trace.record_number("depth", depth);
        Ok(())
    })
}
