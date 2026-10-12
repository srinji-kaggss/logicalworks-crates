//! Seeded simulation of the identifier rule a tenant, a task name and a
//! request key share.
//!
//! The three are part of every step path, key, log line and file name a run
//! writes, so they share one check (`name_fault`): non-empty, at most a ceiling
//! of bytes, ASCII letters, digits and `-_.:` only. One seed draws names on
//! each side of every edge of that rule — the empty name, the ceiling and one
//! past it, a single planted byte outside the set, multi-byte text whose
//! character count fits but whose byte count does not — and every family
//! asserts the verdict against a reference written from the rule's statement:
//!
//! - a drawn legal name of 1 to the ceiling bytes is admitted by all three
//!   types and held byte for byte;
//! - the byte length, not the character count, decides `TooLong`, and it is
//!   checked before the alphabet;
//! - one planted byte outside the set is `BadChar` wherever it lands;
//! - the three types agree with `name_fault` on every draw, each with its own
//!   error variant and reason text;
//! - tenants order by name, so per-tenant state in an ordered map iterates in
//!   one order on every process;
//! - the same seed draws the same verdicts, and distinct seeds diverge.
//!
//! Sources: CWE-22 and CWE-117 (a name copied into a path or a log line
//! unescaped), and the ownership rule in `scope.rs` that one check serves all
//! three so none drifts into accepting what the others refuse.

use std::collections::BTreeMap;

use crate::script::scope::{NameFault, name_fault};
use crate::script::{FlowError, MAX_TENANT_BYTES, Tenant};
use crate::task::name::{MAX_TASK_NAME_BYTES, TaskName};
use crate::task::{MAX_REQUEST_KEY_BYTES, RequestKey};

/// The seeds every family replays.
const SEEDS: [u64; 4] = [
    0x7E4A_0000_0000_0001,
    0x7E4A_0000_0000_0002,
    0x7E4A_FFFF_0000_0003,
    0xC0DE_7E4A_0000_0004,
];

/// Names each family draws per seed.
const DRAWS: usize = 512;

/// The bytes the rule admits.
const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.:";

/// Printable ASCII bytes the rule refuses: the separators and metacharacters
/// a name copied into a path, a shell or a log line would carry.
const REFUSED: &[u8] = b" /\\\t\n\r\"'`$;|&<>*?[]{}()#%!@,=+~^";

/// A family's result.
type Outcome = Result<(), String>;

/// SplitMix64: a seeded stream with no state beyond one word, so a seed is a
/// replayable trace.
struct Stream(u64);

impl Stream {
    /// The next word.
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    /// A drawn index in `0..bound`, from the word's low sixteen bits: every
    /// bound here is far below 65,536, so the bias is under one part in a
    /// hundred. An empty range has no index to draw, and is refused.
    fn below(&mut self, bound: usize) -> Result<usize, String> {
        let [low, high, ..] = self.next().to_le_bytes();
        usize::from(u16::from_le_bytes([low, high]))
            .checked_rem(bound)
            .ok_or_else(|| "an index drawn from an empty range".to_owned())
    }

    /// A drawn byte of `set`.
    fn pick(&mut self, set: &[u8]) -> Result<u8, String> {
        let at = self.below(set.len())?;
        set.get(at).copied().ok_or_else(|| format!("{at} is outside a {}-byte set", set.len()))
    }

    /// A legal name of exactly `length` bytes.
    fn legal(&mut self, length: usize) -> Result<String, String> {
        (0..length).map(|_| self.pick(ALPHABET).map(char::from)).collect()
    }

    /// A legal name of `length` bytes with one refused byte planted in it,
    /// and where it was planted.
    fn planted(&mut self, length: usize) -> Result<(String, usize, u8), String> {
        let mut bytes = self.legal(length)?.into_bytes();
        let at = self.below(bytes.len())?;
        let byte = self.pick(REFUSED)?;
        let slot = bytes.get_mut(at).ok_or("the planted byte is inside the name")?;
        *slot = byte;
        let name = String::from_utf8(bytes).map_err(|error| error.to_string())?;
        Ok((name, at, byte))
    }
}

/// The verdict the rule's statement gives, written without `name_fault`.
fn reference(name: &str, max: usize) -> Option<NameFault> {
    if name.is_empty() {
        Some(NameFault::Empty)
    } else if name.len() > max {
        Some(NameFault::TooLong)
    } else if name.bytes().any(|byte| !ALPHABET.contains(&byte)) {
        Some(NameFault::BadChar)
    } else {
        None
    }
}

/// A drawn name on one side of one edge of the rule.
fn draw(stream: &mut Stream, max: usize) -> Result<String, String> {
    match stream.below(6)? {
        0 => Ok(String::new()),
        1 => {
            let length = stream.below(max)?.saturating_add(1);
            stream.legal(length)
        }
        2 => stream.legal(max),
        3 => {
            let length = max.saturating_add(stream.below(64)?).saturating_add(1);
            stream.legal(length)
        }
        4 => {
            let length = stream.below(max)?.saturating_add(1);
            stream.planted(length).map(|(name, _, _)| name)
        }
        // Two bytes a character: the character count may fit while the byte
        // count does not.
        _ => Ok("é".repeat(stream.below(max)?.saturating_add(1))),
    }
}

/// Every drawn name under each seed, against ceiling `max`.
fn sweep(max: usize, mut check: impl FnMut(u64, &str) -> Outcome) -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream(seed);
        for _ in 0..DRAWS {
            let name = draw(&mut stream, max)?;
            check(seed, &name)?;
        }
    }
    Ok(())
}

#[test]
/// `name_fault` gives the verdict the rule's statement gives, on every draw.
fn sim_name_fault_matches_the_rule_as_stated() -> Outcome {
    for max in [1, 7, MAX_TENANT_BYTES] {
        sweep(max, |seed, name| {
            assert_eq!(
                name_fault(name, max),
                reference(name, max),
                "seed {seed:#x}: {name:?} under {max} bytes"
            );
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
/// A drawn legal name of 1 to the ceiling bytes is admitted by every type and
/// held byte for byte.
fn sim_a_legal_name_is_admitted_and_held_byte_for_byte() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream(seed);
        for _ in 0..DRAWS {
            let length = stream.below(MAX_TENANT_BYTES)?.saturating_add(1);
            let name = stream.legal(length)?;
            let tenant = Tenant::new(&name).map_err(|error| format!("seed {seed:#x}: {error}"))?;
            let task = TaskName::new(&name).map_err(|error| format!("seed {seed:#x}: {error}"))?;
            assert_eq!(tenant.as_str(), name, "seed {seed:#x}: tenant held as given");
            assert_eq!(tenant.to_string(), name, "seed {seed:#x}: tenant displays as given");
            assert_eq!(task.as_str(), name, "seed {seed:#x}: task name held as given");
            assert_eq!(task.to_string(), name, "seed {seed:#x}: task name displays as given");
            if length <= MAX_REQUEST_KEY_BYTES {
                let key =
                    RequestKey::new(&name).map_err(|error| format!("seed {seed:#x}: {error}"))?;
                assert_eq!(key.as_str(), name, "seed {seed:#x}: request key held as given");
            }
        }
    }
    Ok(())
}

#[test]
/// The ceiling is admitted and one byte past it is refused, for every type.
fn sim_the_ceiling_is_admitted_and_one_past_it_refused() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream(seed);
        for _ in 0..64 {
            let at_tenant = stream.legal(MAX_TENANT_BYTES)?;
            let past_tenant = stream.legal(MAX_TENANT_BYTES.saturating_add(1))?;
            assert!(Tenant::new(&at_tenant).is_ok(), "seed {seed:#x}: tenant at its ceiling");
            assert!(
                matches!(Tenant::new(&past_tenant), Err(FlowError::InvalidTenant { .. })),
                "seed {seed:#x}: tenant one past its ceiling"
            );
            let at_task = stream.legal(MAX_TASK_NAME_BYTES)?;
            let past_task = stream.legal(MAX_TASK_NAME_BYTES.saturating_add(1))?;
            assert!(TaskName::new(&at_task).is_ok(), "seed {seed:#x}: task at its ceiling");
            assert!(
                matches!(TaskName::new(&past_task), Err(FlowError::InvalidName { .. })),
                "seed {seed:#x}: task one past its ceiling"
            );
            let at_key = stream.legal(MAX_REQUEST_KEY_BYTES)?;
            let past_key = stream.legal(MAX_REQUEST_KEY_BYTES.saturating_add(1))?;
            assert!(RequestKey::new(&at_key).is_ok(), "seed {seed:#x}: key at its ceiling");
            assert!(
                matches!(
                    RequestKey::new(&past_key),
                    Err(FlowError::InvalidRequestKey { .. })
                ),
                "seed {seed:#x}: key one past its ceiling"
            );
        }
    }
    Ok(())
}

#[test]
/// Bytes, not characters, decide the ceiling: multi-byte text whose character
/// count fits is `TooLong` once its bytes do not, and `BadChar` otherwise.
fn sim_bytes_not_characters_decide_the_ceiling() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream(seed);
        for _ in 0..DRAWS {
            let characters = stream.below(MAX_TENANT_BYTES)?.saturating_add(1);
            let name = "é".repeat(characters);
            let expected = if name.len() > MAX_TENANT_BYTES {
                NameFault::TooLong
            } else {
                NameFault::BadChar
            };
            assert_eq!(
                name_fault(&name, MAX_TENANT_BYTES),
                Some(expected),
                "seed {seed:#x}: {characters} characters, {} bytes",
                name.len()
            );
        }
    }
    Ok(())
}

#[test]
/// One planted byte outside the set is `BadChar` wherever it lands, and a name
/// both too long and badly spelled is reported as too long.
fn sim_a_planted_byte_is_refused_wherever_it_lands() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream(seed);
        for _ in 0..DRAWS {
            let length = stream.below(MAX_TENANT_BYTES)?.saturating_add(1);
            let (name, at, planted) = stream.planted(length)?;
            assert_eq!(
                name_fault(&name, MAX_TENANT_BYTES),
                Some(NameFault::BadChar),
                "seed {seed:#x}: {planted:?} at byte {at} of {length}"
            );
            let long = format!("{name}{}", stream.legal(MAX_TENANT_BYTES)?);
            assert_eq!(
                name_fault(&long, MAX_TENANT_BYTES),
                Some(NameFault::TooLong),
                "seed {seed:#x}: length is checked before the alphabet"
            );
        }
    }
    Ok(())
}

/// The fault a type's refusal reason names, read from its text.
fn fault_named(reason: &str) -> Option<NameFault> {
    if reason.ends_with("is empty") {
        Some(NameFault::Empty)
    } else if reason.contains("is longer than") {
        Some(NameFault::TooLong)
    } else if reason.contains("may hold only") {
        Some(NameFault::BadChar)
    } else {
        None
    }
}

#[test]
/// The three types agree with `name_fault` on every draw, each refusing with
/// its own variant and a reason that names the fault.
fn sim_the_three_types_agree_with_one_rule() -> Outcome {
    sweep(MAX_TENANT_BYTES, |seed, name| {
        let tenant = match Tenant::new(name) {
            Ok(_) => None,
            Err(FlowError::InvalidTenant { reason }) => fault_named(reason),
            Err(other) => return Err(format!("seed {seed:#x}: tenant refused as {other}")),
        };
        assert_eq!(
            tenant,
            name_fault(name, MAX_TENANT_BYTES),
            "seed {seed:#x}: tenant {name:?}"
        );
        let task = match TaskName::new(name) {
            Ok(_) => None,
            Err(FlowError::InvalidName { reason }) => fault_named(reason),
            Err(other) => return Err(format!("seed {seed:#x}: task refused as {other}")),
        };
        assert_eq!(
            task,
            name_fault(name, MAX_TASK_NAME_BYTES),
            "seed {seed:#x}: task {name:?}"
        );
        let key = match RequestKey::new(name) {
            Ok(_) => None,
            Err(FlowError::InvalidRequestKey { reason }) => fault_named(reason),
            Err(other) => return Err(format!("seed {seed:#x}: key refused as {other}")),
        };
        assert_eq!(
            key,
            name_fault(name, MAX_REQUEST_KEY_BYTES),
            "seed {seed:#x}: key {name:?}"
        );
        Ok(())
    })
}

#[test]
/// Tenants order by name: an ordered map of per-tenant state iterates in the
/// order of the names themselves, whatever order they were inserted in.
fn sim_tenants_order_by_name() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream(seed);
        let mut by_tenant = BTreeMap::new();
        let mut names = Vec::new();
        for index in 0..256_usize {
            let length = stream.below(16)?.saturating_add(1);
            let name = stream.legal(length)?;
            let tenant = Tenant::new(&name).map_err(|error| error.to_string())?;
            by_tenant.insert(tenant, index);
            names.push(name);
        }
        names.sort();
        names.dedup();
        let iterated: Vec<&str> = by_tenant.keys().map(Tenant::as_str).collect();
        assert_eq!(iterated, names, "seed {seed:#x}: map order is name order");
    }
    Ok(())
}

/// The FNV-1a hash of every verdict drawn under `seed`.
fn verdict_trace(seed: u64) -> Result<u64, String> {
    let mut stream = Stream(seed);
    let mut trace = 0xcbf2_9ce4_8422_2325_u64;
    for _ in 0..DRAWS {
        let name = draw(&mut stream, MAX_TENANT_BYTES)?;
        let verdict = format!("{name}={:?};", name_fault(&name, MAX_TENANT_BYTES));
        for byte in verdict.bytes() {
            trace = (trace ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    Ok(trace)
}

#[test]
/// The replay oracle: one seed, one verdict trace.
fn sim_the_same_seed_draws_the_same_verdicts() -> Outcome {
    for seed in SEEDS {
        assert_eq!(verdict_trace(seed)?, verdict_trace(seed)?, "seed {seed:#x} replays");
    }
    Ok(())
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn sim_distinct_seeds_draw_differently() -> Outcome {
    assert_ne!(
        verdict_trace(SEEDS[0])?,
        verdict_trace(SEEDS[1])?,
        "two seeds drew alike"
    );
    Ok(())
}
