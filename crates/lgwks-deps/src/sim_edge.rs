//! Seeded model check of the origin and label predicates an approval is
//! judged by.
//!
//! One seed draws Cargo source strings — both crates.io spellings, other
//! registries, Git repositories with and without a pinned revision, path
//! authorities — and near-misses of each (a case flip, a trailing slash, a
//! moved revision), and every family compares the shipped predicate with a
//! reference written here from the module's documentation:
//!
//! - only the three crates.io spellings name crates.io;
//! - a Git source splits into its repository and its revision policy with
//!   nothing lost, and the repository carries no `?` or `#`;
//! - origin identity is reflexive and symmetric, and agrees with the
//!   reference on every drawn pair;
//! - a moved revision inside the approved repository is drift, and a path
//!   authority admits only itself, byte for byte;
//! - a scope with no text prints `<none>` whichever vocabulary spelled it,
//!   and an authored bit prints as itself;
//! - the same seed replays the same verdicts and distinct seeds diverge.
//!
//! Sources: Cargo's source-id grammar (`registry+`, `sparse+`, `git+` with
//! `?rev=`/`?branch=`/`?tag=` and a `#` resolved revision, `path+`), RFC 3986
//! §3.4–3.5 for where a query and a fragment begin, and INV-DEP-13 for the
//! register's `target = ""` spelling.

use lgwks_std::seeded::Seeded;

use crate::edge::{
    CRATES_IO_INDEX, CRATES_IO_SPARSE, bit_label, declared_scope, git_policy, git_repository,
    is_crates_io, same_origin, scope_label,
};

/// The seeds every family replays.
const SEEDS: [u64; 4] = [
    0xED6E_0000_0000_0001,
    0xED6E_0000_0000_0002,
    0xED6E_FFFF_0000_0003,
    0x0121_ED6E_0000_0004,
];

/// Origins each family draws per seed.
const DRAWS: usize = 256;

/// Registry sources: the three crates.io spellings and two other registries.
const REGISTRIES: [&str; 5] = [
    CRATES_IO_INDEX,
    CRATES_IO_SPARSE,
    "sparse+https://index.crates.io",
    "registry+https://example.com/index",
    "sparse+https://my.registry.test/",
];

/// Git repositories.
const REPOSITORIES: [&str; 3] = [
    "git+https://github.com/srinji-kaggss/a",
    "git+https://github.com/srinji-kaggss/b",
    "git+ssh://git@host.test/x",
];

/// Git revision policies, the empty one included.
const POLICIES: [&str; 5] = ["", "?rev=abc123", "?branch=main", "#0123abcd", "?tag=v1#0123abcd"];

/// Path authorities, one a trailing-slash near-miss of another.
const PATHS: [&str; 3] = [
    "path+file:///w/vendor/x",
    "path+file:///w/vendor/x/",
    "path+file:///w/vendor/y",
];

/// A family's result.
type Outcome = Result<(), String>;

/// An index below `len`, drawn without bias.
fn index(stream: &mut Seeded, len: usize) -> Result<usize, String> {
    stream.index(len).map_err(|error| error.to_string())
}

/// One entry of `table`.
fn pick<'table>(stream: &mut Seeded, table: &[&'table str]) -> Result<&'table str, String> {
    let at = index(stream, table.len())?;
    table.get(at).copied().ok_or_else(|| format!("entry {at} is past the table"))
}

/// One drawn source string of any class, near-misses included.
fn origin(stream: &mut Seeded) -> Result<String, String> {
    let drawn = match index(stream, 3)? {
        0 => pick(stream, &REGISTRIES)?.to_owned(),
        1 => format!("{}{}", pick(stream, &REPOSITORIES)?, pick(stream, &POLICIES)?),
        _ => pick(stream, &PATHS)?.to_owned(),
    };
    Ok(match index(stream, 6)? {
        0 => format!("{drawn}/"),
        1 => drawn.to_uppercase(),
        _ => drawn,
    })
}

/// The reference split: the text before the first `?` or `#`, and the rest.
fn reference_split(detail: &str) -> (&str, &str) {
    let cut = detail
        .char_indices()
        .find(|&(_, glyph)| glyph == '?' || glyph == '#')
        .map(|(at, _)| at);
    match cut {
        Some(at) => detail.split_at(at),
        None => (detail, ""),
    }
}

/// The reference identity, from the documentation: equal strings are one
/// origin; otherwise two crates.io spellings are, two Git sources with one
/// repository and one policy are, and nothing else is.
fn reference_same(approved: &str, observed: &str) -> bool {
    let crates_io = |text: &str| {
        text == CRATES_IO_INDEX || text == CRATES_IO_SPARSE || text == "sparse+https://index.crates.io"
    };
    if approved == observed {
        return true;
    }
    if approved.starts_with("registry+") || approved.starts_with("sparse+") {
        return crates_io(approved) && crates_io(observed);
    }
    approved.starts_with("git+")
        && observed.starts_with("git+")
        && reference_split(approved) == reference_split(observed)
}

/// Every drawn pair of origins under each seed.
fn sweep(mut check: impl FnMut(u64, &mut Seeded, &str, &str) -> Outcome) -> Outcome {
    for seed in SEEDS {
        let mut stream = Seeded::from_seed(seed);
        for _ in 0..DRAWS {
            let (approved, observed) = (origin(&mut stream)?, origin(&mut stream)?);
            check(seed, &mut stream, &approved, &observed)?;
        }
    }
    Ok(())
}

#[test]
/// Only the three crates.io spellings name crates.io.
fn sim_only_the_three_crates_io_spellings_name_crates_io() -> Outcome {
    sweep(|seed, _, approved, _| {
        let spelled = REGISTRIES.iter().take(3).any(|spelling| *spelling == approved);
        assert_eq!(is_crates_io(approved), spelled, "seed {seed:#x}: {approved:?}");
        Ok(())
    })
}

#[test]
/// A Git source splits into repository and policy with nothing lost.
fn sim_a_git_source_splits_with_nothing_lost() -> Outcome {
    sweep(|seed, _, approved, _| {
        let (repository, policy) = (git_repository(approved), git_policy(approved));
        assert_eq!(format!("{repository}{policy}"), approved, "seed {seed:#x}");
        assert!(!repository.contains(['?', '#']), "seed {seed:#x}: {repository:?}");
        assert!(
            policy.is_empty() || policy.starts_with(['?', '#']),
            "seed {seed:#x}: {policy:?}"
        );
        assert_eq!((repository, policy), reference_split(approved), "seed {seed:#x}");
        Ok(())
    })
}

#[test]
/// Origin identity agrees with the reference on every drawn pair.
fn sim_origin_identity_agrees_with_the_reference() -> Outcome {
    sweep(|seed, _, approved, observed| {
        assert_eq!(
            same_origin(approved, observed),
            reference_same(approved, observed),
            "seed {seed:#x}: {approved:?} vs {observed:?}"
        );
        Ok(())
    })
}

#[test]
/// Origin identity is reflexive and symmetric.
fn sim_origin_identity_is_reflexive_and_symmetric() -> Outcome {
    sweep(|seed, _, approved, observed| {
        assert!(same_origin(approved, approved), "seed {seed:#x}: {approved:?}");
        assert_eq!(
            same_origin(approved, observed),
            same_origin(observed, approved),
            "seed {seed:#x}: {approved:?} vs {observed:?}"
        );
        Ok(())
    })
}

#[test]
/// A moved revision inside the approved repository is drift; one repository
/// and one policy is one origin.
fn sim_a_moved_revision_is_drift() -> Outcome {
    sweep(|seed, stream, _, _| {
        let repository = pick(stream, &REPOSITORIES)?;
        let (first, second) = (pick(stream, &POLICIES)?, pick(stream, &POLICIES)?);
        let approved = format!("{repository}{first}");
        let observed = format!("{repository}{second}");
        assert_eq!(same_origin(&approved, &observed), first == second, "seed {seed:#x}");
        let elsewhere = REPOSITORIES
            .iter()
            .find(|other| **other != repository)
            .ok_or("a second repository")?;
        assert!(
            !same_origin(&approved, &format!("{elsewhere}{first}")),
            "seed {seed:#x}: one policy in another repository"
        );
        Ok(())
    })
}

#[test]
/// The two crates.io spellings are one registry, and no other registry is it.
fn sim_crates_io_spellings_are_one_registry_and_no_other() -> Outcome {
    sweep(|seed, stream, _, _| {
        let (first, second) = (pick(stream, &REGISTRIES)?, pick(stream, &REGISTRIES)?);
        let both = is_crates_io(first) && is_crates_io(second);
        assert_eq!(same_origin(first, second), both || first == second, "seed {seed:#x}");
        Ok(())
    })
}

#[test]
/// A path authority admits only itself, byte for byte, and no class admits
/// another class.
fn sim_a_path_admits_only_itself_and_classes_never_cross() -> Outcome {
    sweep(|seed, stream, approved, observed| {
        let (first, second) = (pick(stream, &PATHS)?, pick(stream, &PATHS)?);
        assert_eq!(same_origin(first, second), first == second, "seed {seed:#x}");
        let class = |text: &str| text.split('+').next().map(str::to_owned);
        let crossed = class(approved) != class(observed)
            && !(is_crates_io(approved) && is_crates_io(observed));
        if crossed {
            assert!(
                !same_origin(approved, observed),
                "seed {seed:#x}: {approved:?} admitted {observed:?}"
            );
        }
        Ok(())
    })
}

#[test]
/// A scope with no text prints `<none>` in either vocabulary; any other
/// prints as itself, and the register spelling of no scope is `""`.
fn sim_a_scope_prints_none_whichever_vocabulary_spelled_it() -> Outcome {
    let scopes = ["", "cfg(unix)", "cfg(windows)", "cfg(target_os = \"linux\")", " "];
    for seed in SEEDS {
        let mut stream = Seeded::from_seed(seed);
        for _ in 0..DRAWS {
            let scope = pick(&mut stream, &scopes)?;
            let expected = if scope.is_empty() { "<none>" } else { scope };
            assert_eq!(scope_label(Some(scope)), expected, "seed {seed:#x}");
            assert_eq!(declared_scope(Some(scope)), scope, "seed {seed:#x}");
            assert_eq!(scope_label(Some(declared_scope(None))), scope_label(None));
        }
    }
    assert_eq!(scope_label(None), "<none>", "an unconditional edge");
    assert_eq!(declared_scope(None), "", "INV-DEP-13's spelling of no scope");
    Ok(())
}

#[test]
/// An authored bit prints as itself, and the absent bit says so.
fn sim_an_authored_bit_prints_as_itself() -> Outcome {
    for seed in SEEDS {
        let mut stream = Seeded::from_seed(seed);
        for _ in 0..DRAWS {
            let bit = match index(&mut stream, 3)? {
                0 => None,
                1 => Some(false),
                _ => Some(true),
            };
            let label = bit_label(bit);
            match bit {
                Some(value) => assert_eq!(label, value.to_string(), "seed {seed:#x}"),
                None => assert_eq!(label, "no admitted bit", "seed {seed:#x}"),
            }
        }
    }
    Ok(())
}

/// The replay trace of every verdict drawn under `seed`: FNV-1a over the
/// verdict bits and the drawn strings.
fn verdict_trace(seed: u64) -> Result<u64, String> {
    let mut stream = Seeded::from_seed(seed);
    let mut trace: u64 = 0xCBF2_9CE4_8422_2325;
    for _ in 0..DRAWS {
        let (approved, observed) = (origin(&mut stream)?, origin(&mut stream)?);
        let verdict = if same_origin(&approved, &observed) { "=" } else { "!" };
        for byte in format!("{approved}{verdict}{observed};").bytes() {
            trace = (trace ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3);
        }
    }
    Ok(trace)
}

#[test]
/// The replay oracle: one seed, one verdict trace.
fn sim_the_same_seed_replays_the_same_verdicts() -> Outcome {
    for seed in SEEDS {
        assert_eq!(verdict_trace(seed)?, verdict_trace(seed)?, "seed {seed:#x} replays");
    }
    Ok(())
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn sim_distinct_seeds_draw_different_verdicts() -> Outcome {
    assert_ne!(
        verdict_trace(SEEDS[0])?,
        verdict_trace(SEEDS[1])?,
        "two seeds drew alike"
    );
    Ok(())
}
