//! The step store's on-disk refusals: a torn final record is dropped, while an old version, a foreign file, mid-file corruption and an oversized record are refused and leave the store untouched.

use super::*;

/// A truncated final record loses only itself.
#[test]
fn a_torn_final_record_is_dropped_and_earlier_ones_survive() -> TestResult {
    let scratch = Scratch::new("torn")?;
    let store_dir = scratch.path().join("store");
    let host = stored_host("acme", &store_dir)?;
    let input = scratch.path().to_path_buf();

    let report = lgwks_bot::block_on(host.run(&three_step_task()?, input.clone()));
    let run = report.run_id().ok_or("a stored run must name its run id")?;
    let path = host
        .run_store()
        .ok_or("a stored host keeps a store")?
        .path()
        .to_path_buf();
    drop(host);

    // Cut the file inside its last frame: the shape a `SIGKILL` mid-append
    // leaves.
    let whole = std::fs::read(&path)?;
    assert!(whole.len() > 64, "the store must hold more than a header");
    let file = std::fs::OpenOptions::new().write(true).open(&path)?;
    file.set_len(u64::try_from(whole.len() - 8)?)?;
    file.sync_all()?;
    drop(file);

    let reopened = stored_host("acme", &store_dir)?;
    let store = reopened.run_store().ok_or("a stored host keeps a store")?;
    assert_eq!(
        store.record_count(run),
        2,
        "the torn tail is dropped and the two whole records survive"
    );
    let resumed = lgwks_bot::block_on(reopened.resume(run, &three_step_task()?, input));
    assert_eq!(resumed.output(), Some(&10));
    assert_eq!(ran(scratch.path(), "gamma")?, 2);
    Ok(())
}

/// The header a `\x01` run store carries.
///
/// `\x01` was a real shipped format: the run, the step key, the tenant, the path
/// and the value, with no [`DefinitionIdentity`] in the record. It is spelled out
/// here rather than produced by the crate, because the crate no longer writes it
/// and a test that built one from the current writer would only prove that the
/// current writer is what it is.
///
/// The magic is the current one — the fifteen constant bytes every version
/// shares — and the last byte is `\x01`, which is what makes this file a
/// `\x01` store and not a foreign file. A test that got that last byte wrong
/// would be refused as [`StoreError::NotAStore`] and would pass a file-corruption
/// assertion while proving nothing about the version refusal, so the byte is
/// named and asserted rather than derived.
const V1_HEADER: &[u8; 16] = b"lgwks-runstore\x00\x01";

/// The index of the version byte, which is the byte that differs between formats.
const VERSION_BYTE: usize = V1_HEADER.len() - 1;

/// The format version this build reads.
///
/// Not a re-spelling of the crate's private constant: a test that asserted the
/// store against the same constant the store uses would pass whatever that
/// constant were, including a revert to `\x01` — and the whole claim under test
/// is that the two versions are *not* the same. The literal is what makes the
/// refusal specific.
const CURRENT_FORMAT: u8 = 2;

/// A real `\x01`-format store on disk is refused naming both versions.
///
/// The store is produced by the shipped writer, so every frame in it is a real
/// frame a real run committed; only the version byte is rewritten, which is
/// exactly the edit that turns today's file into the one `origin/main` wrote.
/// Reopen it and the refusal must be [`StoreError::FormatVersion`] carrying both
/// numbers — not [`StoreError::NotAStore`], which would tell an operator their
/// own data was never theirs, and not `Corrupt`, which would send them looking
/// for rot that is not there. The bytes must also be unchanged afterwards,
/// because a refusal that trims what it cannot read is how a re-run loses a
/// system.
#[test]
fn a_pre_version_store_is_refused_naming_both_versions() -> TestResult {
    let scratch = Scratch::new("format")?;
    let store_dir = scratch.path().join("store");
    let host = stored_host("acme", &store_dir)?;
    let report = lgwks_bot::block_on(host.run(&three_step_task()?, scratch.path().to_path_buf()));
    assert_eq!(
        report.disposition(),
        Disposition::Succeeded,
        "the store must be written first"
    );
    let path = host
        .run_store()
        .ok_or("a stored host keeps a store")?
        .path()
        .to_path_buf();
    drop(host);

    // Rewrite the version byte in place: the file becomes a `\x01` store holding
    // real committed frames, which is the artifact a pre-version deployment has.
    let mut bytes = std::fs::read(&path)?;
    assert_eq!(
        &bytes[..VERSION_BYTE],
        &V1_HEADER[..VERSION_BYTE],
        "the shipped store and the pre-version header must share their magic, or this \
         test would be refused as a foreign file and would not be testing the version"
    );
    bytes[VERSION_BYTE] = V1_HEADER[VERSION_BYTE];
    std::fs::write(&path, &bytes)?;
    let before = std::fs::read(&path)?;

    match RunStore::open(&path) {
        Err(error) => {
            let (found, expected) = shared::format_version(&error).ok_or_else(|| {
                format!("a \\x01 store must be refused as a version, got: {error}")
            })?;
            assert_eq!(
                found, V1_HEADER[VERSION_BYTE],
                "the refusal names the version found"
            );
            assert_eq!(
                expected, CURRENT_FORMAT,
                "the refusal names the version this build reads"
            );
            let rendered = error.to_string();
            assert!(
                rendered.contains("format version 1") && rendered.contains("reads version 2"),
                "both versions must be in the message a caller reads, got: {rendered}"
            );
        }
        Ok(_) => return Err("a pre-version run store must not open".into()),
    }
    assert_eq!(
        std::fs::read(&path)?,
        before,
        "the refusal moved bytes in a store it could not read"
    );

    // The same refusal through the door a host actually installs, so the row is
    // reached the way a deployment meets it rather than through a store handle.
    assert!(
        matches!(
            Host::builder("acme")?.run_store(&store_dir),
            Err(StoreError::FormatVersion { .. })
        ),
        "a host installed over a pre-version store must refuse it too"
    );
    Ok(())
}

/// A foreign file is still refused as not a store, and the two refusals stay apart.
///
/// The companion to the row above: separating "this is an older version" from
/// "this was never ours" is only worth doing if the second answer survives. If
/// both collapsed into one arm, the version test above would pass on a check that
/// also refused every unrelated file, and an operator would be told their data is
/// corrupt for a store this build is merely too old to read.
#[test]
fn a_foreign_file_is_still_refused_as_not_a_store() -> TestResult {
    let scratch = Scratch::new("foreign")?;
    let store_dir = scratch.path().join("store");
    std::fs::create_dir_all(&store_dir)?;
    let path = store_dir.join("acme.runstore");
    // Shares the version byte with the `\x01` store and differs in the first one,
    // which is the byte that says "this is some version of this format".
    std::fs::write(&path, b"lgwks-runstorX\x00\x01")?;
    assert!(
        matches!(RunStore::open(&path), Err(StoreError::NotAStore)),
        "a file that was never a run store is refused as not one"
    );
    std::fs::write(&path, b"not a store at all")?;
    assert!(
        matches!(RunStore::open(&path), Err(StoreError::NotAStore)),
        "a short file is refused as not one"
    );
    Ok(())
}

/// A committed frame that does not follow is refused, not trimmed.
#[test]
fn a_store_corrupt_before_the_tail_is_refused_not_trimmed() -> TestResult {
    let scratch = Scratch::new("corrupt")?;
    let store_dir = scratch.path().join("store");
    let host = stored_host("acme", &store_dir)?;
    let report = lgwks_bot::block_on(host.run(&three_step_task()?, scratch.path().to_path_buf()));
    assert!(report.run_id().is_some());
    let path = host
        .run_store()
        .ok_or("a stored host keeps a store")?
        .path()
        .to_path_buf();
    drop(host);

    // Flip a byte inside the first record's payload. Its stored head no longer
    // follows, so the frame is committed evidence and is refused.
    let mut bytes = std::fs::read(&path)?;
    bytes[20] ^= 0xff;
    std::fs::write(&path, &bytes)?;

    match Host::builder("acme")?.run_store(&store_dir) {
        Err(error) => {
            assert!(
                matches!(error, StoreError::Corrupt { .. }),
                "a chain break is corruption, got: {error}"
            );
            assert!(
                error.to_string().contains("not trimmed"),
                "the refusal must say the bytes were not trimmed"
            );
        }
        Ok(_) => return Err("a store whose chain does not verify must not open".into()),
    }
    Ok(())
}

/// A value past the per-record ceiling is refused, and nothing is written.
#[test]
fn an_oversized_record_is_refused_naming_the_ceiling() -> TestResult {
    let scratch = Scratch::new("caps")?;
    let store_dir = scratch.path().join("store");
    let host = stored_host("acme", &store_dir)?;

    let huge = task("huge", |scope: Scope, _dir: PathBuf| async move {
        remember(&scope, "huge", || async {
            Ok::<_, FlowError>(vec![7u8; lgwks_bot::task::MAX_RECORD_BYTES + 1])
        })
        .await
    })?;
    let report = lgwks_bot::block_on(host.run(&huge, scratch.path().to_path_buf()));
    assert_eq!(report.disposition(), Disposition::Failed);
    let text = format!("{:?}", report.error());
    assert!(
        text.contains("exceeds the ceiling"),
        "the refusal must name the ceiling, got: {text}"
    );
    let run = report.run_id().ok_or("a stored run must name its run id")?;
    assert_eq!(
        host.run_store()
            .ok_or("a stored host keeps a store")?
            .record_count(run),
        0,
        "a refused record must leave the store empty, not half-written"
    );
    Ok(())
}
