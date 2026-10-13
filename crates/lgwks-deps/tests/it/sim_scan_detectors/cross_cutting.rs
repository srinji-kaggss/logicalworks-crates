//! Cross-cutting sweeps: clean files report nothing, findings follow their items under permutation and inserted lines, ordering, concurrent tenants, path independence, malformed and on-disk files, and seed replay.

use super::*;

// ── Cross-cutting ───────────────────────────────────────────────────────────

/// Files drawn only from the clean side of every rule report nothing: no
/// false positive across the seed space.
#[test]
fn clean_files_report_nothing() -> TestResult {
    let swept = sweep(0xc1ea_0001, |rng| {
        several(rng, |rng, index| match rng.below(4) {
            0 => {
                let placement = *rng.pick_named("placements", &PLACEMENTS)?;
                allow_block(rng, index, placement, "allow", Lints::OtherOnly)
            }
            1 => chain_block(rng, index, false, &SHAPES, MAX_TRY_CHAIN),
            2 => doc_block(
                rng,
                index,
                DocSpec {
                    fewest: 7,
                    most: 10,
                    ..PUBLIC_DOC
                },
            ),
            _ => filler_block(rng),
        })
    })?;
    assert!(
        swept.iter().all(|swept| swept.1.is_empty()),
        "a clean file reported a finding"
    );
    Ok(())
}

/// Each finding as `(block, offset into it, rule)`: what stays fixed when the
/// blocks move.
fn by_block(
    blocks: &[Block],
    file: &File,
    found: &[Finding],
) -> Drawn<Vec<(usize, usize, &'static str)>> {
    let mut placed = Vec::new();
    for &(line, rule, _) in found {
        let owner = file
            .starts
            .iter()
            .zip(blocks)
            .position(|(&start, block)| {
                line >= start && line < start.saturating_add(block.lines.len())
            })
            .ok_or("a finding outside every block")?;
        let start = file.starts.get(owner).copied().ok_or("a start per block")?;
        placed.push((owner, line.saturating_sub(start), rule));
    }
    placed.sort_unstable();
    Ok(placed)
}

/// A finding belongs to its item, not to its position: the same blocks in a
/// shuffled order report the same findings, each inside its own block.
#[test]
fn findings_follow_their_items_under_any_permutation() -> TestResult {
    for index in 0..SEEDS {
        let seed = 0xc1ea_0002 ^ index.wrapping_mul(SPREAD);
        let mut rng = Rng::new(seed);
        let blocks = mixed_blocks(&mut rng)?;
        let natural: Vec<usize> = (0..blocks.len()).collect();
        let mut shuffled = natural.clone();
        shuffle(&mut rng, &mut shuffled)?;
        let gaps = vec![0; blocks.len()];
        let mut placements = Vec::new();
        for order in [&natural, &shuffled] {
            let file = assemble(&blocks, order, &gaps)?;
            let found = agree(seed, &file)?;
            placements.push(by_block(&blocks, &file, &found)?);
        }
        assert_eq!(
            placements.first(),
            placements.last(),
            "seed {seed:#x}: findings moved between blocks under {shuffled:?}"
        );
    }
    Ok(())
}

/// Lines inserted before a block shift every finding in it by exactly their
/// count, and its function lines with it.
#[test]
fn inserted_lines_shift_every_finding_by_exactly_their_count() -> TestResult {
    for index in 0..SEEDS {
        let seed = 0xc1ea_0003 ^ index.wrapping_mul(SPREAD);
        let mut rng = Rng::new(seed);
        let blocks = mixed_blocks(&mut rng)?;
        let order: Vec<usize> = (0..blocks.len()).collect();
        let tight = assemble(&blocks, &order, &vec![0; blocks.len()])?;
        let loose_gaps: Vec<u32> = order.iter().map(|_| rng.between(1, 6)).collect();
        let loose = assemble(&blocks, &order, &loose_gaps)?;
        let tight_found = agree(seed, &tight)?;
        let loose_found = agree(seed, &loose)?;
        assert_eq!(
            by_block(&blocks, &tight, &tight_found)?,
            by_block(&blocks, &loose, &loose_found)?,
            "seed {seed:#x}: a gap moved a finding inside its block"
        );
    }
    Ok(())
}

/// Findings come back by line, then by rule, so a printer reads them in file
/// order without sorting.
#[test]
fn findings_are_ordered_by_line_then_rule() -> TestResult {
    for index in 0..SEEDS {
        let seed = mixed_seed(index ^ 0x0de4);
        let found = scanned(&mixed_file(seed)?.source, "sim.rs")?;
        assert!(
            found.windows(2).all(|pair| match *pair {
                [ref left, ref right] => (left.0, left.1) <= (right.0, right.1),
                _ => false,
            }),
            "seed {seed:#x}: findings out of order: {found:#?}"
        );
    }
    Ok(())
}

/// Sixty-four files scanned at once on their own threads, eight times each,
/// each report exactly their own findings: the scanner keeps no state between
/// calls for one tenant's file to leak into another's.
#[test]
fn concurrent_scans_of_distinct_files_each_get_their_own_findings() -> TestResult {
    let mut files = Vec::new();
    for index in 0..TENANTS {
        let seed = mixed_seed(index ^ 0x7e4a_0000);
        files.push((seed, mixed_file(seed)?));
    }
    let failures = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for &(seed, ref file) in &files {
            handles.push(scope.spawn(move || -> Result<(), String> {
                for round in 0..ROUNDS {
                    let observed = scanned(&file.source, &format!("tenant-{seed:x}.rs"))
                        .map_err(|error| format!("seed {seed:#x} round {round}: {error}"))?;
                    let verdict = if observed == file.expected {
                        Ok(())
                    } else {
                        Err(format!("seed {seed:#x} round {round}: {observed:#?}"))
                    };
                    verdict?;
                }
                Ok(())
            }));
        }
        let mut failures = Vec::new();
        for handle in handles {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(failure)) => failures.push(failure),
                Err(_) => failures.push("a scanner thread panicked".to_owned()),
            }
        }
        failures
    });
    assert!(failures.is_empty(), "tenants disagreed: {failures:#?}");
    Ok(())
}

/// The findings depend on the source alone: the same text under two path
/// labels, scanned twice each, reports one list.
#[test]
fn a_scan_depends_on_the_source_alone() -> TestResult {
    for index in 0..SEEDS {
        let seed = mixed_seed(index ^ 0x1de0);
        let file = mixed_file(seed)?;
        let first = scanned(&file.source, "a.rs")?;
        for path in ["a.rs", "tenant/b.rs"] {
            assert_eq!(
                scanned(&file.source, path)?,
                first,
                "seed {seed:#x}: {path} reported differently"
            );
        }
    }
    Ok(())
}

/// A torn file — one unclosed parenthesis at a seeded block boundary — is
/// refused with its own path, never passed as clean.
#[test]
fn a_malformed_file_is_refused_with_its_path() -> TestResult {
    for index in 0..SEEDS {
        let seed = 0xbad0_0001 ^ index.wrapping_mul(SPREAD);
        let mut rng = Rng::new(seed);
        let mut blocks = mixed_blocks(&mut rng)?;
        let at = usize::try_from(rng.below(u32::try_from(blocks.len().saturating_add(1))?))?;
        let mut torn = Block::default();
        torn.line("fn torn(");
        blocks.insert(at, torn);
        let order: Vec<usize> = (0..blocks.len()).collect();
        let file = assemble(&blocks, &order, &vec![0; blocks.len()])?;
        let path = format!("tenant-{seed:x}.rs");
        let result = scan_source(&file.source, &path);
        assert!(
            matches!(result, Err(ScanError::Unparseable { path: ref named, .. }) if *named == path),
            "seed {seed:#x}: a torn file must be refused, not passed: {result:?}"
        );
    }
    Ok(())
}

/// A file on disk reports what its text reports, and a path that does not
/// exist is refused rather than passed.
#[test]
fn scan_path_reads_what_scan_source_reads() -> TestResult {
    let directory = std::env::temp_dir();
    // A drawn nonce, not the process id: the OS reuses ids, and several
    // checkouts on one host share this directory.
    let nonce = hex::encode(random::bytes::<8>()?);
    for index in 0..64_u64 {
        let seed = mixed_seed(index ^ 0xd15c);
        let file = mixed_file(seed)?;
        let path = directory.join(format!("lgwks-sim-scan-{nonce}-{seed:x}.rs"));
        std::fs::write(&path, &file.source)?;
        let from_disk = scan_path(&path);
        std::fs::remove_file(&path)?;
        let from_disk: Vec<Finding> = from_disk?
            .iter()
            .map(|hit| (hit.line(), hit.rule(), hit.snippet().to_owned()))
            .collect();
        assert_eq!(
            from_disk,
            agree(seed, &file)?,
            "seed {seed:#x}: disk and text differ"
        );
    }
    let missing = directory.join(format!("lgwks-sim-scan-{nonce}-missing.rs"));
    assert!(
        matches!(scan_path(&missing), Err(ScanError::Unparseable { .. })),
        "a missing file is refused"
    );
    Ok(())
}

/// The same seed replays to the same trace; distinct seeds diverge.
#[test]
fn the_same_seed_replays_and_distinct_seeds_diverge() -> TestResult {
    let mut hashes = BTreeSet::new();
    for index in 0..SEEDS {
        let seed = mixed_seed(index ^ 0x5eed_0000);
        let first = mixed_receipt(seed)?;
        assert_eq!(
            first,
            mixed_receipt(seed)?,
            "seed {seed:#x} replayed differently"
        );
        hashes.insert(first);
    }
    assert!(
        u64::try_from(hashes.len())? == SEEDS,
        "{SEEDS} seeds gave {} traces",
        hashes.len()
    );
    Ok(())
}
