//! Whether a `cfg` predicate can only hold under `test`.
//!
//! The predicate's free atoms are enumerated exhaustively, at most
//! [`MAX_ATOMS`] of them, with `test` held false: if no assignment satisfies
//! it, the item exists only in test builds. Atoms are keyed by a structural
//! rendering, and every comparison renders both sides through the same
//! [`atom_key`], so two spellings of one atom are one variable.

use std::collections::{BTreeMap, BTreeSet};

use super::meta::{self, Meta};

/// The most free atoms one predicate is solved over: 4,096 assignments. A
/// predicate past it is not called test-only, the answer that keeps an item
/// in scope for the other detectors.
const MAX_ATOMS: usize = 12;

/// True when no assignment of the predicate's atoms outside `test` satisfies
/// it, so the item it gates exists only under `cfg(test)`.
pub(super) fn requires_test(predicate: &Meta<'_>) -> bool {
    let mut atoms = BTreeSet::new();
    collect(predicate, &mut atoms);
    if atoms.len() > MAX_ATOMS {
        return false;
    }
    let Ok(width) = u32::try_from(atoms.len()) else {
        return false;
    };
    let Some(assignments) = 1_usize.checked_shl(width) else {
        return false;
    };
    let atoms: Vec<String> = atoms.into_iter().collect();
    !(0..assignments).any(|assignment| {
        let values: BTreeMap<&str, bool> = atoms
            .iter()
            .zip(0_u32..)
            .map(|(atom, bit)| {
                let set = assignment
                    .checked_shr(bit)
                    .is_some_and(|shifted| shifted & 1 == 1);
                (atom.as_str(), set)
            })
            .collect();
        evaluate(predicate, &values).may_hold()
    })
}

/// What a predicate evaluates to under one assignment of its atoms.
///
/// `Unevaluated` is an answer of its own: an atom the assignment never
/// recorded means the walk that collected atoms and the evaluation that reads
/// them saw different shapes in one predicate, and folding that into either
/// truth value would hide the disagreement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// Holds under this assignment.
    Satisfied,
    /// Does not hold under this assignment.
    Unsatisfied,
    /// Could not be evaluated under this assignment.
    Unevaluated,
}

impl Verdict {
    /// Whether the predicate can hold. One that could not be evaluated is
    /// never called test-only: a kept item is a candidate for a finding, a
    /// dropped one is a clean report for source nobody read.
    const fn may_hold(self) -> bool {
        matches!(self, Self::Satisfied | Self::Unevaluated)
    }

    /// `all` of two conjuncts: one unsatisfied conjunct decides it.
    const fn all(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unsatisfied, _) | (_, Self::Unsatisfied) => Self::Unsatisfied,
            (Self::Satisfied, Self::Satisfied) => Self::Satisfied,
            _ => Self::Unevaluated,
        }
    }

    /// `any` of two disjuncts: one satisfied disjunct decides it.
    const fn any(self, other: Self) -> Self {
        match (self, other) {
            (Self::Satisfied, _) | (_, Self::Satisfied) => Self::Satisfied,
            (Self::Unsatisfied, Self::Unsatisfied) => Self::Unsatisfied,
            _ => Self::Unevaluated,
        }
    }

    /// `not` of one argument; the negation of "unknown" stays unknown.
    const fn negate(self) -> Self {
        match self {
            Self::Satisfied => Self::Unsatisfied,
            Self::Unsatisfied => Self::Satisfied,
            Self::Unevaluated => Self::Unevaluated,
        }
    }
}

/// The combinator a list applies, when its path is exactly one of the three.
fn combinator(predicate: &Meta<'_>) -> Option<&'static str> {
    let Meta::List { ref path, .. } = *predicate else {
        return None;
    };
    match *path.as_slice() {
        [ref only] if only == "all" => Some("all"),
        [ref only] if only == "any" => Some("any"),
        [ref only] if only == "not" => Some("not"),
        _ => None,
    }
}

/// True for the bare path `test`, which the solver holds false.
fn is_test(predicate: &Meta<'_>) -> bool {
    matches!(*predicate, Meta::Path(ref path) if path.len() == 1 && path.first().is_some_and(|only| only == "test"))
}

/// The items of a list predicate, or `None` when its group does not parse.
fn children<'tree>(predicate: &Meta<'tree>) -> Option<Vec<Meta<'tree>>> {
    match *predicate {
        Meta::List { ref group, .. } => meta::parse_list(group),
        Meta::Path(_) | Meta::NameValue { .. } => None,
    }
}

/// Every free atom of a predicate, through `all`, `any` and `not`.
///
/// `test` is not collected: the solver supplies it as false, and a free
/// `test` would let it satisfy a predicate no non-test build can. A
/// combinator whose items do not parse is recorded whole, by its own key,
/// which keeps the predicate unsolvable rather than dropping the atom.
fn collect(predicate: &Meta<'_>, atoms: &mut BTreeSet<String>) {
    if is_test(predicate) {
        return;
    }
    if combinator(predicate).is_some() {
        match children(predicate) {
            Some(items) => items.iter().for_each(|item| collect(item, atoms)),
            None => {
                atoms.insert(atom_key(predicate));
            }
        }
        return;
    }
    atoms.insert(atom_key(predicate));
}

/// A predicate's structural key: segments joined by `::`, a list's items in
/// order, a value as [`meta`] rendered it. Equal atoms render equal keys and
/// distinct atoms distinct keys, which is all the solver relies on.
fn atom_key(predicate: &Meta<'_>) -> String {
    match *predicate {
        Meta::Path(ref path) => path.join("::"),
        Meta::List { ref path, .. } => {
            let head = path.join("::");
            match children(predicate) {
                Some(items) => {
                    let inner = items.iter().map(atom_key).collect::<Vec<_>>().join(",");
                    format!("{head}({inner})")
                }
                None => format!("{head}(?)"),
            }
        }
        Meta::NameValue {
            ref path,
            ref value,
        } => format!("{}={value}", path.join("::")),
    }
}

/// A predicate's verdict under one assignment, `test` held false.
fn evaluate(predicate: &Meta<'_>, values: &BTreeMap<&str, bool>) -> Verdict {
    if is_test(predicate) {
        return Verdict::Unsatisfied;
    }
    let Some(operator) = combinator(predicate) else {
        return atom(predicate, values);
    };
    let items = children(predicate);
    match (operator, items) {
        // `all` or `any` of a list nobody could read cannot be shown to hold
        // or to fail.
        ("all" | "any", None) => Verdict::Unevaluated,
        // Each fold seeds with its operator's identity: `all` of nothing
        // holds, `any` of nothing does not.
        ("all", Some(items)) => items.iter().fold(Verdict::Satisfied, |held, item| {
            held.all(evaluate(item, values))
        }),
        ("any", Some(items)) => items.iter().fold(Verdict::Unsatisfied, |held, item| {
            held.any(evaluate(item, values))
        }),
        // A `not` of anything but one argument is malformed Rust; it reads as
        // the atom it was given rather than guessing which argument was meant.
        (_, Some(items)) => match *items.as_slice() {
            [ref only] => evaluate(only, values).negate(),
            _ => atom(predicate, values),
        },
        (_, None) => atom(predicate, values),
    }
}

/// One atom's assigned value; `Unevaluated` when the assignment never
/// recorded it.
fn atom(predicate: &Meta<'_>, values: &BTreeMap<&str, bool>) -> Verdict {
    match values.get(atom_key(predicate).as_str()) {
        Some(&true) => Verdict::Satisfied,
        Some(&false) => Verdict::Unsatisfied,
        None => Verdict::Unevaluated,
    }
}
