//! Checks that `REQUIREMENTS.md` has not drifted without a recorded supersession.
//!
//! The product requirements are the repository's normative spine. A requirement
//! is superseded, never edited, so the gate needs a memory of what each
//! normative sentence said the last time a person decided it. That memory is
//! `scripts/requirements.lock`: one entry per requirement id, holding a short
//! digest of its normative sentence and the sentence itself.
//!
//! The check answers one question: has any normative sentence changed since it
//! was last recorded, and if so, did someone say so out loud? A mismatch is not
//! silently repaired. `--update` rewrites the lock, but only for requirements
//! whose id appears in the Supersession log section of `REQUIREMENTS.md`.
//!
//! Usage: `check-requirements [--update] [ROOT]`. The exit status is non-zero on
//! any mismatch or on a refused update.
//!
//! The one implementation of this check in the estate (#695): this file is the
//! rule, the `check-requirements` binary reads its arguments and runs
//! [`execute`], and `logical_ci`'s `governance` gate calls [`execute`] as a
//! library, so the requirement that a normative sentence moves only under a
//! Supersession log entry exists once with one owner.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The lock's name as it is spelled in messages.
const LOCK_NAME: &str = "scripts/requirements.lock";

/// The lock's header comment, written verbatim ahead of its rows.
const LOCK_HEADER: &str = "\
# The normative sentence of each product requirement, as it read when a person
# last decided it. `REQUIREMENTS.md` is superseded, never edited, so this lock
# is the memory the gate checks an edit against.
#
# One entry per requirement: `R<id>` TAB `<sha256, first 12 hex>` TAB
# `<the normative sentence, whitespace-collapsed>`.
# Generated and checked by the `check-requirements` binary of `lgwks_deps`. A changed digest
# with no Supersession log entry naming that id is refused; record the
# supersession first, then `--update`.
";

/// SHA-256 round constants (FIPS 180-4, section 4.2.2).
const ROUND: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// SHA-256 initial hash value (FIPS 180-4, section 5.3.3).
const INITIAL: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// Why the check could not run.
#[derive(Debug)]
enum CheckError {
    /// A file could not be read or written.
    Io {
        /// What was being touched.
        path: PathBuf,
        /// What the operating system said.
        cause: io::Error,
    },
    /// A requirement heading has no blockquote under it.
    NoBlockquote {
        /// The requirement id.
        id: u64,
    },
    /// A lock row is not `R<id> TAB digest TAB sentence`.
    BadLockRow {
        /// The one-based line number.
        line: usize,
    },
    /// A lock row's id is not `R<digits>`.
    BadLockId {
        /// The one-based line number.
        line: usize,
        /// The id as written.
        key: String,
    },
    /// The text to digest is longer than a SHA-256 length field can state.
    TooLong,
}

impl fmt::Display for CheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io {
                ref path,
                ref cause,
            } => write!(formatter, "{}: {cause}", path.display()),
            Self::NoBlockquote { id } => write!(
                formatter,
                "REQUIREMENTS.md: R{id} has no normative blockquote. A requirement without a \
                 `>` sentence is not written yet."
            ),
            Self::BadLockRow { line } => write!(
                formatter,
                "{LOCK_NAME}:{line}: expected `R<id> TAB digest TAB sentence`"
            ),
            Self::BadLockId { line, ref key } => {
                write!(formatter, "{LOCK_NAME}:{line}: bad requirement id {key:?}")
            }
            Self::TooLong => write!(formatter, "the sentence is too long to digest"),
        }
    }
}

impl std::error::Error for CheckError {}

/// Logs `error` at the site that produced it, then hands it back as the `Err`.
fn fail<T>(error: CheckError, site: &'static str) -> Result<T, CheckError> {
    lgwks_std::trace::debug!(error = ?error, site, "returning an error to the caller");
    Err(error)
}

/// The requirements and the ids the Supersession log names.
struct Parsed {
    /// Each requirement's normative sentence by id.
    sentences: BTreeMap<u64, String>,
    /// Ids the Supersession log names: a receipt that the requirement moved.
    logged: BTreeSet<u64>,
}

/// One lock row: the stored digest and sentence.
struct Locked {
    /// The first twelve hex digits of the sentence's SHA-256.
    digest: String,
    /// The sentence as recorded.
    sentence: String,
}

/// What the comparison of the requirements with the lock found.
struct Findings {
    /// One message per mismatch.
    problems: Vec<String>,
    /// The ids that moved.
    changed: Vec<u64>,
}

/// The exit code a run settles on.
type Code = u8;

/// Reads a file whole, naming it when it cannot be read.
fn read(path: &Path) -> Result<String, CheckError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(cause) => fail(
            CheckError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "read",
        ),
    }
}

/// A run of ASCII digits as a requirement id, or `None` when it does not fit.
fn parse_id(digits: &str) -> Option<u64> {
    match digits.parse::<u64>() {
        Ok(id) => Some(id),
        Err(error) => {
            lgwks_std::trace::debug!(%error, digits, "a requirement id does not parse as a u64");
            None
        }
    }
}

/// Mixes one 64-byte block into the running state.
fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut schedule = [0_u32; 64];
    let (words, _) = block.as_slice().as_chunks::<4>();
    for (slot, word) in schedule.iter_mut().zip(words) {
        *slot = u32::from_be_bytes(*word);
    }
    for round in 16..64_usize {
        let early = schedule[round.saturating_sub(15)];
        let late = schedule[round.saturating_sub(2)];
        let low = early.rotate_right(7) ^ early.rotate_right(18) ^ early.wrapping_shr(3);
        let high = late.rotate_right(17) ^ late.rotate_right(19) ^ late.wrapping_shr(10);
        schedule[round] = schedule[round.saturating_sub(16)]
            .wrapping_add(low)
            .wrapping_add(schedule[round.saturating_sub(7)])
            .wrapping_add(high);
    }
    let [
        mut aa,
        mut bb,
        mut cc,
        mut dd,
        mut ee,
        mut ff,
        mut gg,
        mut hh,
    ] = *state;
    for (constant, word) in ROUND.iter().zip(schedule.iter()) {
        let sum_e = ee.rotate_right(6) ^ ee.rotate_right(11) ^ ee.rotate_right(25);
        let choose = (ee & ff) ^ (!ee & gg);
        let first = hh
            .wrapping_add(sum_e)
            .wrapping_add(choose)
            .wrapping_add(*constant)
            .wrapping_add(*word);
        let sum_a = aa.rotate_right(2) ^ aa.rotate_right(13) ^ aa.rotate_right(22);
        let majority = (aa & bb) ^ (aa & cc) ^ (bb & cc);
        let second = sum_a.wrapping_add(majority);
        hh = gg;
        gg = ff;
        ff = ee;
        ee = dd.wrapping_add(first);
        dd = cc;
        cc = bb;
        bb = aa;
        aa = first.wrapping_add(second);
    }
    for (slot, added) in state.iter_mut().zip([aa, bb, cc, dd, ee, ff, gg, hh]) {
        *slot = slot.wrapping_add(added);
    }
}

/// The SHA-256 of `data`, as eight state words.
fn sha256(data: &[u8]) -> Result<[u32; 8], CheckError> {
    let length = match u64::try_from(data.len()) {
        Ok(length) => length,
        Err(_) => return fail(CheckError::TooLong, "sha256"),
    };
    let Some(bits) = length.checked_mul(8) else {
        return fail(CheckError::TooLong, "sha256");
    };
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() & 63 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());
    let mut state = INITIAL;
    let (blocks, _) = message.as_slice().as_chunks::<64>();
    for block in blocks {
        compress(&mut state, block);
    }
    Ok(state)
}

/// The first twelve hex digits of `sentence`'s SHA-256: the lock's digest.
fn digest_of(sentence: &str) -> Result<String, CheckError> {
    let state = sha256(sentence.as_bytes())?;
    Ok(format!("{:08x}{:04x}", state[0], state[1].wrapping_shr(16)))
}

/// A requirement heading, `### R12. Title`, as its id.
fn heading_id(line: &str) -> Option<u64> {
    let rest = line.strip_prefix("###")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim_start().strip_prefix('R')?;
    let end = match rest.find(|ch: char| !ch.is_ascii_digit()) {
        Some(end) => end,
        None => rest.len(),
    };
    let digits = rest.get(..end)?;
    if digits.is_empty() {
        return None;
    }
    let after = rest.get(end..)?.strip_prefix('.')?;
    if !after.starts_with(char::is_whitespace) || after.trim().is_empty() {
        return None;
    }
    parse_id(digits)
}

/// A blockquote line's body: the text after `>` and one optional space.
fn quote_body(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('>')?;
    match rest.chars().next() {
        Some(first) if first.is_whitespace() => rest.strip_prefix(first),
        _ => Some(rest),
    }
}

/// Whether `line` is the `## Supersession log` heading.
fn is_log_heading(line: &str) -> bool {
    match line.strip_prefix("##") {
        Some(rest) => rest.starts_with(char::is_whitespace) && rest.trim() == "Supersession log",
        None => false,
    }
}

/// Whether `ch` continues a word, for the `\b` boundaries of an id.
fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Every `R<digits>` id in `line` that stands alone as a word.
fn log_ids(line: &str) -> Vec<u64> {
    let mut found = Vec::new();
    let mut previous: Option<char> = None;
    for (start, ch) in line.char_indices() {
        let standalone_start = match previous {
            Some(before) => !is_word(before),
            None => true,
        };
        previous = Some(ch);
        if ch != 'R' || !standalone_start {
            continue;
        }
        let Some(tail) = line.get(start.saturating_add(1)..) else {
            continue;
        };
        let end = match tail.find(|digit: char| !digit.is_ascii_digit()) {
            Some(end) => end,
            None => tail.len(),
        };
        let Some(digits) = tail.get(..end) else {
            continue;
        };
        if digits.is_empty() {
            continue;
        }
        let bounded = match tail.get(end..).and_then(|rest| rest.chars().next()) {
            Some(after) => !is_word(after),
            None => true,
        };
        if !bounded {
            continue;
        }
        if let Some(id) = parse_id(digits) {
            found.push(id);
        }
    }
    found
}

/// Collapses a blockquote to one comparable sentence.
fn normative_sentence(block: &[&str]) -> String {
    block
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Reads each requirement's sentence and the ids the log names.
fn parse_requirements(text: &str) -> Result<Parsed, CheckError> {
    let lines: Vec<&str> = text.lines().collect();
    let mut sentences = BTreeMap::new();
    let mut logged = BTreeSet::new();
    let mut index = 0_usize;
    while let Some(line) = lines.get(index) {
        if is_log_heading(line) {
            index = index.saturating_add(1);
            while let Some(entry) = lines.get(index) {
                if entry.starts_with("## ") {
                    break;
                }
                logged.extend(log_ids(entry));
                index = index.saturating_add(1);
            }
            continue;
        }
        let Some(id) = heading_id(line) else {
            index = index.saturating_add(1);
            continue;
        };
        index = index.saturating_add(1);
        while lines
            .get(index)
            .is_some_and(|blank| blank.trim().is_empty())
        {
            index = index.saturating_add(1);
        }
        let mut quote: Vec<&str> = Vec::new();
        while let Some(current) = lines.get(index) {
            if let Some(body) = quote_body(current) {
                quote.push(body);
                index = index.saturating_add(1);
                continue;
            }
            // A hard-wrapped quote continues over one blank line only when the
            // line after it is quoted again.
            if current.trim().is_empty()
                && lines
                    .get(index.saturating_add(1))
                    .is_some_and(|next| quote_body(next).is_some())
            {
                quote.push("");
                index = index.saturating_add(1);
                continue;
            }
            break;
        }
        if quote.is_empty() {
            return fail(CheckError::NoBlockquote { id }, "parse_requirements");
        }
        sentences.insert(id, normative_sentence(&quote));
    }
    Ok(Parsed { sentences, logged })
}

/// A lock key, `R<digits>`, as its id.
fn lock_id(key: &str) -> Option<u64> {
    let digits = key.trim().strip_prefix('R')?;
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    parse_id(digits)
}

/// Reads the lock's rows by id.
fn parse_lock(text: &str) -> Result<BTreeMap<u64, Locked>, CheckError> {
    let mut entries = BTreeMap::new();
    for (offset, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let number = offset.saturating_add(1);
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        let (Some(key), Some(digest), Some(sentence)) = (parts.first(), parts.get(1), parts.get(2))
        else {
            return fail(CheckError::BadLockRow { line: number }, "parse_lock");
        };
        let Some(id) = lock_id(key) else {
            return fail(
                CheckError::BadLockId {
                    line: number,
                    key: (*key).to_owned(),
                },
                "parse_lock",
            );
        };
        entries.insert(
            id,
            Locked {
                digest: digest.trim().to_owned(),
                sentence: (*sentence).to_owned(),
            },
        );
    }
    Ok(entries)
}

/// Writes the lock from the current sentences.
fn write_lock(path: &Path, sentences: &BTreeMap<u64, String>) -> Result<(), CheckError> {
    let mut rows = vec![LOCK_HEADER.to_owned()];
    for (id, sentence) in sentences {
        rows.push(format!("R{id}\t{}\t{sentence}\n", digest_of(sentence)?));
    }
    match fs::write(path, rows.concat()) {
        Ok(()) => Ok(()),
        Err(cause) => fail(
            CheckError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "write_lock",
        ),
    }
}

/// Compares the requirements with the lock.
fn compare(
    sentences: &BTreeMap<u64, String>,
    locked: &BTreeMap<u64, Locked>,
) -> Result<Findings, CheckError> {
    let mut problems = Vec::new();
    let mut changed = Vec::new();
    for (id, sentence) in sentences {
        let Some(old) = locked.get(id) else {
            problems.push(format!(
                "R{id}: present in REQUIREMENTS.md, absent from the lock"
            ));
            changed.push(*id);
            continue;
        };
        if digest_of(sentence)? != old.digest || *sentence != old.sentence {
            problems.push(format!(
                "R{id}: normative sentence changed\n    was: {}\n    now: {sentence}",
                old.sentence
            ));
            changed.push(*id);
        }
    }
    for id in locked.keys() {
        if !sentences.contains_key(id) {
            problems.push(format!(
                "R{id}: present in the lock, absent from REQUIREMENTS.md"
            ));
            changed.push(*id);
        }
    }
    Ok(Findings { problems, changed })
}

/// Runs the check over `root`, writing its verdict to the two handles.
#[doc(hidden)]
pub fn execute(
    root: &Path,
    update: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<Code, Box<dyn std::error::Error>> {
    let requirements_path = root.join("REQUIREMENTS.md");
    let lock_path = root.join("scripts").join("requirements.lock");
    if !requirements_path.is_file() {
        writeln!(err, "REQUIREMENTS.md is missing")?;
        return Ok(1);
    }
    let parsed = parse_requirements(&read(&requirements_path)?)?;
    if parsed.sentences.is_empty() {
        writeln!(err, "REQUIREMENTS.md declares no requirements")?;
        return Ok(1);
    }
    if !lock_path.is_file() {
        if update {
            write_lock(&lock_path, &parsed.sentences)?;
            writeln!(
                out,
                "wrote {LOCK_NAME} — {} requirement(s)",
                parsed.sentences.len()
            )?;
            return Ok(0);
        }
        writeln!(
            err,
            "{LOCK_NAME} is missing. Run with --update to record the current text."
        )?;
        return Ok(1);
    }
    let locked = parse_lock(&read(&lock_path)?)?;
    let findings = compare(&parsed.sentences, &locked)?;
    let mut unlogged: Vec<u64> = findings
        .changed
        .iter()
        .copied()
        .filter(|id| !parsed.logged.contains(id))
        .collect();
    unlogged.sort_unstable();
    unlogged.dedup();
    if !findings.problems.is_empty() {
        for problem in &findings.problems {
            writeln!(err, "{problem}")?;
        }
        if !unlogged.is_empty() {
            let names: Vec<String> = unlogged.iter().map(|id| format!("R{id}")).collect();
            writeln!(
                err,
                "\n{} change(s) have no Supersession log entry: {}.\nA requirement is \
                 superseded, never edited. Add an entry to the Supersession log in \
                 REQUIREMENTS.md naming the id and why the previous statement was wrong or \
                 incomplete, then re-run with --update.",
                unlogged.len(),
                names.join(", ")
            )?;
            return Ok(1);
        }
        if update {
            write_lock(&lock_path, &parsed.sentences)?;
            writeln!(
                out,
                "updated {LOCK_NAME} — {} supersession(s) recorded",
                findings.changed.len()
            )?;
            return Ok(0);
        }
        writeln!(
            err,
            "\nEvery change has a Supersession log entry. Re-run with --update to record the \
             new text."
        )?;
        return Ok(1);
    }
    if update {
        writeln!(
            out,
            "{LOCK_NAME} already matches REQUIREMENTS.md; nothing to do"
        )?;
        return Ok(0);
    }
    writeln!(
        out,
        "requirements: {} declared, all digests match the lock",
        parsed.sentences.len()
    )?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::{
        digest_of, execute, heading_id, is_log_heading, log_ids, parse_lock, parse_requirements,
        quote_body,
    };
    use std::error::Error;
    use std::path::Path;

    type TestResult = Result<(), Box<dyn Error>>;

    /// FIPS 180-4 / RFC 6234 test vectors: the empty message, "abc", and the
    /// 56-byte message that forces a second block.
    #[test]
    fn the_digest_matches_the_published_sha256_vectors() -> TestResult {
        assert_eq!(
            digest_of("")?,
            "e3b0c44298fc",
            "SHA-256 of the empty message"
        );
        assert_eq!(digest_of("abc")?, "ba7816bf8f01", "SHA-256 of abc");
        assert_eq!(
            digest_of("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")?,
            "248d6a61d206",
            "SHA-256 of the two-block message"
        );
        Ok(())
    }

    #[test]
    fn a_heading_names_its_id_and_a_plain_line_names_none() {
        assert_eq!(
            heading_id("### R12. Human mess is in scope"),
            Some(12),
            "a heading"
        );
        assert_eq!(heading_id("### R12 no dot"), None, "no dot");
        assert_eq!(heading_id("## R12. wrong level"), None, "wrong level");
        assert_eq!(heading_id("### R. no id"), None, "no digits");
    }

    #[test]
    fn a_quote_loses_its_marker_and_one_space() {
        assert_eq!(quote_body("> text"), Some("text"), "marker and space");
        assert_eq!(quote_body(">text"), Some("text"), "marker only");
        assert_eq!(quote_body("  >  two"), Some(" two"), "one space only");
        assert_eq!(quote_body("text"), None, "no marker");
    }

    #[test]
    fn the_log_heading_and_its_ids_are_found() {
        assert!(is_log_heading("## Supersession log"), "the heading");
        assert!(!is_log_heading("### Supersession log"), "a deeper heading");
        assert_eq!(
            log_ids("R3 and R41, not XR5 or R6x"),
            vec![3, 41],
            "standalone ids"
        );
    }

    #[test]
    fn requirements_collapse_a_wrapped_quote_and_record_the_log() -> TestResult {
        let text = "### R1. One\n\n> first\n> second\n\n### R2. Two\n> only\n\n## Supersession log\nR2 moved\n";
        let parsed = parse_requirements(text)?;
        assert_eq!(
            parsed.sentences.get(&1).map(String::as_str),
            Some("first second"),
            "a wrapped quote collapses"
        );
        assert_eq!(
            parsed.sentences.get(&2).map(String::as_str),
            Some("only"),
            "a one-line quote"
        );
        assert!(parsed.logged.contains(&2), "the log names R2");
        Ok(())
    }

    #[test]
    fn a_heading_without_a_quote_is_refused() {
        assert!(
            parse_requirements("### R1. One\n\ntext\n").is_err(),
            "a requirement with no blockquote is not written yet"
        );
    }

    #[test]
    fn a_lock_row_needs_three_tab_separated_fields() -> TestResult {
        let rows = parse_lock("# header\nR7\tabc\tsentence\n")?;
        assert_eq!(
            rows.get(&7).map(|row| row.digest.as_str()),
            Some("abc"),
            "the digest is read"
        );
        assert!(
            parse_lock("R7 abc sentence\n").is_err(),
            "spaces are not tabs"
        );
        assert!(
            parse_lock("X7\tabc\tsentence\n").is_err(),
            "the id must start with R"
        );
        Ok(())
    }

    /// The committed lock verifies against the committed `REQUIREMENTS.md`: the
    /// digests the Python script recorded are the digests this binary computes.
    #[test]
    fn the_committed_lock_verifies() -> TestResult {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute(&root, false, &mut out, &mut err)?;
        assert_eq!(
            code,
            0,
            "the committed lock must verify: {}",
            String::from_utf8_lossy(&err)
        );
        Ok(())
    }
}
