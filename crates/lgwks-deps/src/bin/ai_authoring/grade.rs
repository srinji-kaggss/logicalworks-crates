//! What the text of a trial says: the oracle's clause names, each clause's
//! verdict in a `cargo test` log, the peak resident set of the oracle's run, and
//! the size of a solution.

use std::collections::BTreeMap;

/// What one oracle clause came to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Verdict {
    /// The clause ran and passed.
    Pass,
    /// The clause ran and failed.
    Fail,
    /// The clause never ran: the crate did not compile, or the run ended first.
    NotRun,
    /// The oracle was still running at its bound, and this clause had not finished.
    Timeout,
}

impl Verdict {
    /// The word a record carries.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NotRun => "not_run",
            Self::Timeout => "timeout",
        }
    }

    /// The verdict a record's word names.
    pub(crate) fn from_label(label: &str) -> Option<Self> {
        match label {
            "pass" => Some(Self::Pass),
            "fail" => Some(Self::Fail),
            "not_run" => Some(Self::NotRun),
            "timeout" => Some(Self::Timeout),
            _ => None,
        }
    }
}

/// Whether `ch` can be part of a Rust identifier.
fn is_word(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// The byte length of the leading run of `text` whose characters satisfy `accept`.
fn run_length(text: &str, accept: impl Fn(char) -> bool) -> usize {
    match text.find(|ch: char| !accept(ch)) {
        Some(at) => at,
        None => text.len(),
    }
}

/// The function name a `#[test]` attribute is followed by, in `rest`.
fn name_after_attribute(rest: &str) -> Option<&str> {
    let trimmed = rest.trim_start();
    let signature = match trimmed.strip_prefix("pub") {
        Some(after) if after.starts_with(char::is_whitespace) => after.trim_start(),
        Some(_) | None => trimmed,
    };
    let after_fn = signature.strip_prefix("fn")?;
    if !after_fn.starts_with(char::is_whitespace) {
        return None;
    }
    let body = after_fn.trim_start();
    body.get(..run_length(body, is_word))
        .filter(|name| !name.is_empty())
}

/// The name of every `#[test] fn` in an oracle's source, in source order.
pub(crate) fn oracle_test_names(source: &str) -> Vec<String> {
    let attribute = "#[test]";
    source
        .match_indices(attribute)
        .filter_map(|(at, _)| {
            source
                .get(at.saturating_add(attribute.len())..)
                .and_then(name_after_attribute)
        })
        .map(str::to_owned)
        .collect()
}

/// The clause and verdict one line of libtest output reports, when it reports one.
///
/// `ignored` reads as a failure: an oracle clause that did not run proved nothing.
fn clause_verdict(line: &str) -> Option<(&str, Verdict)> {
    let rest = line.trim().strip_prefix("test")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let named = rest.trim_start();
    let end = run_length(named, |ch| is_word(ch) || ch == ':');
    let name = named.get(..end).filter(|found| !found.is_empty())?;
    let after = named.get(end..)?;
    if !after.starts_with(char::is_whitespace) {
        return None;
    }
    let dots = after.trim_start().strip_prefix("...")?;
    if !dots.starts_with(char::is_whitespace) {
        return None;
    }
    let status = dots.trim_start();
    if status.starts_with("ok") {
        Some((name, Verdict::Pass))
    } else if status.starts_with("FAILED") || status.starts_with("ignored") {
        Some((name, Verdict::Fail))
    } else {
        None
    }
}

/// Records each named clause's verdict from a `cargo test` log.
pub(crate) fn read_verdicts(log: &str, clauses: &mut BTreeMap<String, Verdict>) {
    for line in log.lines() {
        if let Some((name, verdict)) = clause_verdict(line)
            && let Some(slot) = clauses.get_mut(name)
        {
            *slot = verdict;
        }
    }
}

/// The peak resident set, in bytes, a `/usr/bin/time -l` report states.
///
/// macOS prints `67911680  maximum resident set size` in bytes and GNU prints
/// `Maximum resident set size (kbytes): 66443` in kibibytes. The label is matched
/// without regard to case and the value is taken from the side the label leaves
/// it on, so neither spelling reads as an absent measurement.
pub(crate) fn peak_rss_bytes(report: &str) -> Option<u64> {
    for line in report.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some(label) = words
            .iter()
            .position(|word| word.eq_ignore_ascii_case("maximum"))
        else {
            continue;
        };
        return match label.checked_sub(1) {
            Some(before) => words.get(before).and_then(|word| word.parse::<u64>().ok()),
            None => words
                .last()
                .and_then(|word| word.parse::<u64>().ok())
                .and_then(|kibibytes| kibibytes.checked_mul(1024)),
        };
    }
    None
}

/// The non-blank, non-comment lines of a solution.
pub(crate) fn consumer_lines(code: &str) -> usize {
    code.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .count()
}

/// How often each author-burden token occurs in a solution, and the total.
pub(crate) fn orchestration_sites(
    code: &str,
    tokens: &[String],
) -> (usize, BTreeMap<String, usize>) {
    let mut counts = BTreeMap::new();
    let mut total = 0_usize;
    for token in tokens {
        let found = code.matches(token.as_str()).count();
        total = total.saturating_add(found);
        counts.insert(token.clone(), found);
    }
    (total, counts)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        Verdict, consumer_lines, oracle_test_names, orchestration_sites, peak_rss_bytes,
        read_verdicts,
    };

    /// The clauses of a task's committed oracle: `#[test]` then an optional `pub`
    /// then `fn`, as the regexp the Python runner used read them.
    #[test]
    fn oracle_clause_names_are_the_test_functions_in_source_order() {
        let source = "#[test]\nfn first_clause() {}\n\n#[test]\n pub fn second_clause() {}\n#[test] fn third() {}\n#[test]\nasync fn not_a_clause() {}\n#[cfg(test)]\nfn helper() {}";
        assert_eq!(
            oracle_test_names(source),
            ["first_clause", "second_clause", "third"],
            "three attributes are followed by a plain or pub fn; an async fn and a cfg(test) helper are not clauses"
        );
    }

    /// rust-lang/rust `library/test/src/formatters/pretty.rs`: one line per test,
    /// `test <name> ... <status>`.
    #[test]
    fn libtest_lines_give_each_clause_its_verdict() {
        let mut clauses: BTreeMap<String, Verdict> = ["alpha", "beta", "gamma", "delta"]
            .iter()
            .map(|name| ((*name).to_owned(), Verdict::NotRun))
            .collect();
        let log = "running 4 tests\ntest alpha ... ok\ntest beta ... FAILED\ntest gamma ... ignored\ntest unrelated ... ok\n\ntest result: FAILED. 1 passed; 1 failed; 1 ignored";
        read_verdicts(log, &mut clauses);
        assert_eq!(clauses.get("alpha"), Some(&Verdict::Pass), "ok is a pass");
        assert_eq!(
            clauses.get("beta"),
            Some(&Verdict::Fail),
            "FAILED is a failure"
        );
        assert_eq!(
            clauses.get("gamma"),
            Some(&Verdict::Fail),
            "an ignored clause proved nothing and counts as a failure"
        );
        assert_eq!(
            clauses.get("delta"),
            Some(&Verdict::NotRun),
            "a clause that printed nothing never ran"
        );
        assert!(
            !clauses.contains_key("unrelated"),
            "a test that is not an oracle clause is not recorded"
        );
    }

    /// `time(1)` on macOS prints bytes before the label; GNU prints kibibytes after
    /// it, capitalised.
    #[test]
    fn the_peak_resident_set_reads_in_both_spellings_in_bytes() {
        assert_eq!(
            peak_rss_bytes(
                "        1.85 real         0.31 user         0.12 sys\n  67911680  maximum resident set size\n"
            ),
            Some(67_911_680),
            "the macOS form is already in bytes"
        );
        assert_eq!(
            peak_rss_bytes("\tMaximum resident set size (kbytes): 66443\n"),
            Some(68_037_632),
            "the GNU form is kibibytes and is scaled to bytes"
        );
        assert_eq!(
            peak_rss_bytes("no report here"),
            None,
            "no label, no measurement"
        );
        assert_eq!(
            peak_rss_bytes("maximum resident set size (kbytes): many"),
            None,
            "a value that is not a number is no measurement"
        );
    }

    #[test]
    fn consumer_lines_skip_blank_and_comment_lines() {
        assert_eq!(
            consumer_lines("//! doc\n\nuse std::fmt;\n    // note\npub fn f() {}\n   \n"),
            2,
            "two lines of code among a doc line, a note and blanks"
        );
    }

    #[test]
    fn orchestration_sites_count_each_token_and_total_them() {
        let tokens = vec![
            "JoinSet".to_owned(),
            "spawn(".to_owned(),
            "abort(".to_owned(),
        ];
        let (total, by_token) = orchestration_sites(
            "let mut set = JoinSet::new(); set.spawn(a); set.spawn(b); JoinSet::new();",
            &tokens,
        );
        assert_eq!(total, 4, "two JoinSet and two spawn( occurrences");
        assert_eq!(
            by_token.get("abort("),
            Some(&0),
            "an absent token counts zero, not nothing"
        );
        assert_eq!(by_token.get("spawn("), Some(&2), "spawn( occurs twice");
    }

    #[test]
    fn verdicts_round_trip_through_their_words() {
        for verdict in [
            Verdict::Pass,
            Verdict::Fail,
            Verdict::NotRun,
            Verdict::Timeout,
        ] {
            assert_eq!(
                Verdict::from_label(verdict.label()),
                Some(verdict),
                "a record's word names its verdict"
            );
        }
        assert_eq!(
            Verdict::from_label("passed"),
            None,
            "an unknown word names no verdict"
        );
    }
}
