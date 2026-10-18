//! Seeded sweeps over `lexical_join` against a component-stack model (INV-DEP-7). Each test draws
//! its own base depth and its own `a`, `b`, `.` and `..` parts from the seed, and checks the join
//! against a stack written here, never against the join's own output.

use std::error::Error;
use std::path::{Path, PathBuf};

use lgwks_std::seeded::Seeded;

use super::edges::lexical_join;

/// What every test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// The seeds each sweep covers.
const SEEDS: u64 = 400;

/// A drawn key and its stack model: the components the join stands for, or `None` once a `..`
/// pops past the root.
type Drawn = (String, Option<Vec<String>>);

/// A name unique to one draw: `index` keeps two draws apart, the word keeps seeds apart.
fn ident(stream: &mut Seeded, index: usize) -> Result<String, Box<dyn Error>> {
    Ok(format!("p{index}_{:x}", stream.below(1 << 32)?))
}

/// The absolute path a stack of normal components stands for.
fn path_of(stack: &[String]) -> PathBuf {
    let mut path = PathBuf::from("/");
    for part in stack {
        path.push(part);
    }
    path
}

/// A base `depth` components deep: `/d0/d1/...`.
fn base_of(depth: usize) -> PathBuf {
    path_of(
        &(0..depth)
            .map(|level| format!("d{level}"))
            .collect::<Vec<_>>(),
    )
}

/// A relative key of drawn parts and the stack model of its join onto a base `depth` deep; the
/// model is `None` once a `..` pops past the root.
fn drawn_relative(stream: &mut Seeded, depth: usize) -> Result<Drawn, Box<dyn Error>> {
    let mut parts = Vec::new();
    let mut stack: Option<Vec<String>> =
        Some((0..depth).map(|level| format!("d{level}")).collect());
    for _ in 0..stream.below(8)? {
        let part = ["a", "b", ".", ".."][stream.index(4)?];
        parts.push(part);
        stack = match (stack, part) {
            (held, ".") => held,
            (Some(mut held), "..") => held.pop().map(|_| held),
            (Some(mut held), name) => {
                held.push(name.to_owned());
                Some(held)
            }
            (None, _) => None,
        };
    }
    Ok((parts.join("/"), stack))
}

#[test]
fn sim_lexical_join_is_the_component_stack_model() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let depth = usize::try_from(stream.below(4)?)?;
        let base = base_of(depth);
        let (relative, model) = drawn_relative(&mut stream, depth)?;
        assert_eq!(
            lexical_join(&base, &relative),
            model.as_deref().map(path_of),
            "seed {seed}: join({base:?}, {relative:?}) left the stack model"
        );
    }
    Ok(())
}

#[test]
fn sim_lexical_join_ignores_current_directory_parts() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let base = PathBuf::from(format!("/w/{}", ident(&mut stream, 0)?));
        let first = ident(&mut stream, 1)?;
        let second = ident(&mut stream, 2)?;
        assert_eq!(
            lexical_join(&base, &format!("./{first}/./{second}")),
            lexical_join(&base, &format!("{first}/{second}")),
            "seed {seed}: a `.` part changed the join"
        );
    }
    Ok(())
}

#[test]
fn sim_lexical_join_parent_removes_exactly_one_component() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let base = PathBuf::from(format!("/w/{}", ident(&mut stream, 0)?));
        let gone = ident(&mut stream, 1)?;
        let kept = ident(&mut stream, 2)?;
        assert_eq!(
            lexical_join(&base, &format!("{gone}/../{kept}")),
            Some(base.join(&kept)),
            "seed {seed}: `x/..` did not cancel"
        );
    }
    Ok(())
}

#[test]
fn sim_lexical_join_absolute_key_ignores_its_base() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let base = PathBuf::from(format!("/w/{}", ident(&mut stream, 0)?));
        let name = ident(&mut stream, 1)?;
        assert_eq!(
            lexical_join(&base, &format!("/abs/{name}")),
            Some(PathBuf::from(format!("/abs/{name}"))),
            "seed {seed}: an absolute key was joined onto a base"
        );
    }
    Ok(())
}

#[test]
fn sim_lexical_join_past_the_root_is_none_and_at_the_root_is_some() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let depth = usize::try_from(stream.below(6)?)?;
        let base = base_of(depth);
        let at_root = vec![".."; depth].join("/");
        assert_eq!(
            lexical_join(&base, &at_root),
            Some(PathBuf::from("/")),
            "seed {seed}: {depth} parents from depth {depth} reach the root"
        );
        let past = vec![".."; depth.saturating_add(1)].join("/");
        assert_eq!(
            lexical_join(&base, &past),
            None,
            "seed {seed}: one parent too many must fail closed"
        );
    }
    Ok(())
}

#[test]
fn sim_lexical_join_of_nothing_is_the_base() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let base = PathBuf::from(format!("/w/{}", ident(&mut stream, 0)?));
        assert_eq!(
            lexical_join(&base, ""),
            Some(base.clone()),
            "seed {seed}: an empty key moved the base"
        );
        assert_eq!(
            lexical_join(&base, "."),
            Some(base),
            "seed {seed}: a lone `.` moved the base"
        );
    }
    Ok(())
}

#[test]
fn sim_lexical_join_appends_a_normal_part_to_any_join() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let depth = usize::try_from(stream.below(4)?)?;
        let base = base_of(depth);
        let (relative, _) = drawn_relative(&mut stream, depth)?;
        let tail = ident(&mut stream, 9)?;
        // An empty key plus `/tail` would be the absolute key `/tail`, not an append.
        let longer = if relative.is_empty() {
            tail.clone()
        } else {
            format!("{relative}/{tail}")
        };
        assert_eq!(
            lexical_join(&base, &longer),
            lexical_join(&base, &relative).map(|joined| joined.join(&tail)),
            "seed {seed}: appending {tail:?} to {relative:?} was not an append"
        );
    }
    Ok(())
}

#[test]
fn sim_lexical_join_output_rejoined_as_a_key_is_itself() -> TestResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let depth = usize::try_from(stream.below(4)?)?;
        let base = base_of(depth);
        let (relative, _) = drawn_relative(&mut stream, depth)?;
        let Some(joined) = lexical_join(&base, &relative) else {
            continue;
        };
        let key = joined.to_str().ok_or("a drawn path is UTF-8")?;
        assert_eq!(
            lexical_join(Path::new("/elsewhere"), key),
            Some(joined),
            "seed {seed}: a normalized absolute key was changed by a second join"
        );
    }
    Ok(())
}
