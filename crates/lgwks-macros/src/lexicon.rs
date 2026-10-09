//! The lexicon: every word of `script!`, one row each (SL-1, #380).
//!
//! The parser dispatches on this table, and the documentation is rendered from
//! it: the block table in the crate documentation and the word table in the
//! README are both checked against [`doc_table`] and [`readme_table`] by the
//! tests below, so a word cannot be added, renamed or re-described in one
//! place and not the other.
//!
//! A word is reachable only through its row. [`Kind`] is constructed nowhere
//! but in [`LEXICON`], so a kind with no row is a variant the compiler reports
//! as never constructed, which the workspace's `-D warnings` refuses; and
//! `emit` dispatches on the kind of the row a line's first token names, so a
//! line whose word has no row is plain Rust and never reaches an emitter.
//!
//! The particles inside a form (`in`, `up to`, `times`, `waiting`, `at most`,
//! `with`) are part of that word's grammar, read by its emitter; they are not
//! words, and a line cannot start with one.

use lgwks_deps::proc_macro2::TokenTree;

use crate::lines::{Line, ident};

/// What a word does, which is what `emit` dispatches on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// `flow name(inputs) -> Output:`
    Flow,
    /// `each x in xs:`
    Each,
    /// `within 2s:`
    Within,
    /// `retry up to 3 times:`
    Retry,
    /// `together:`
    Together,
    /// `step name:`
    Step,
    /// `for x in xs:`
    For,
    /// `if cond:`
    If,
    /// `else:` / `else if cond:`
    Else,
    /// `let x = <block>:`
    Let,
    /// `run other(args)`
    Run,
    /// `give back value`
    GiveBack,
    /// `fail with reason`
    Fail,
}

impl Kind {
    /// The token a line starts with to say this word.
    pub(crate) const fn spelling(self) -> &'static str {
        match self {
            Self::Flow => "flow",
            Self::Each => "each",
            Self::Within => "within",
            Self::Retry => "retry",
            Self::Together => "together",
            Self::Step => "step",
            Self::For => "for",
            Self::If => "if",
            Self::Else => "else",
            Self::Let => "let",
            Self::Run => "run",
            Self::GiveBack => "give",
            Self::Fail => "fail",
        }
    }
}

/// Where a word may stand.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Position {
    /// Opens a flow at the top level of a script.
    Flow,
    /// Opens an indented block: its line ends in `:`.
    Block,
    /// Starts a line that opens no block.
    Line,
    /// Anywhere inside a line of Rust.
    Inline,
}

impl Position {
    /// The name the README table gives this position.
    const fn name(self) -> &'static str {
        match self {
            Self::Flow => "flow",
            Self::Block => "block",
            Self::Line => "line",
            Self::Inline => "inline",
        }
    }
}

/// The axis a guarantee serves (SL-4).
///
/// The nine axes decide what a word must guarantee before it is admitted; they
/// are a design requirement, not a score. Only the axes some word's guarantee
/// serves today are variants here: a variant no row names would be dead code
/// the workspace's `-D warnings` refuses, and adding it is part of admitting
/// the word that first serves it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Axis {
    /// Holds for any input and any schedule, not only the common one.
    Generalized,
    /// The caller depends on a typed answer, never on the callee's internals.
    Decoupled,
    /// Nothing outlives the run, and a rerun reproduces its keys.
    Ephemeral,
    /// Bounded in work, time and retries, however wide the input.
    Hyperscale,
    /// Idiomatic Rust: no panic, no hidden control flow.
    Idiomatic,
    /// Every key and every call stays inside one tenant.
    MultiTenant,
}

impl Axis {
    /// The axis as the README names it.
    const fn name(self) -> &'static str {
        match self {
            Self::Generalized => "generalized",
            Self::Decoupled => "decoupled",
            Self::Ephemeral => "ephemeral",
            Self::Hyperscale => "hyperscale",
            Self::Idiomatic => "idiomatic",
            Self::MultiTenant => "multi-tenant",
        }
    }
}

/// The test that proves a guarantee: a compile-fail case or a deterministic
/// simulation, the two kinds of evidence SL-4 accepts.
pub(crate) enum Evidence {
    /// A case of the compile-fail table (`REFUSALS` in `src/tests.rs`), named
    /// by its case.
    Refusal(&'static str),
    /// A seeded simulation: a `#[test]`, or a family a `band_family!` sweeps,
    /// in a `sim_` file named from the workspace root.
    Sim {
        /// The file, from the workspace root.
        file: &'static str,
        /// The test or family.
        test: &'static str,
    },
}

impl Evidence {
    /// The evidence as the README cites it.
    fn cite(&self) -> String {
        match *self {
            Self::Refusal(case) => format!("refusal *{case}*"),
            // A path with no directory is already the file's own name.
            Self::Sim { file, test } => match file.rsplit_once('/') {
                Some((_, name)) => format!("`{test}` in `{name}`"),
                None => format!("`{test}` in `{file}`"),
            },
        }
    }
}

/// One thing that holds wherever a word is used, and the test that proves it.
pub(crate) struct Guarantee {
    /// The axis it serves.
    pub(crate) axis: Axis,
    /// What holds, in one clause.
    pub(crate) claim: &'static str,
    /// The test that fails if it stops holding.
    pub(crate) evidence: Evidence,
}

/// A guarantee proved by a simulation in `lgwks_bot`'s `sim_script.rs`, the
/// file whose flows the real macro expanded.
const fn script_sim(axis: Axis, claim: &'static str, test: &'static str) -> Guarantee {
    Guarantee {
        axis,
        claim,
        evidence: Evidence::Sim {
            file: SCRIPT_SIMS,
            test,
        },
    }
}

/// A guarantee proved by a compile-fail case.
const fn refused(axis: Axis, claim: &'static str, case: &'static str) -> Guarantee {
    Guarantee {
        axis,
        claim,
        evidence: Evidence::Refusal(case),
    }
}

/// The simulation file whose flows the real macro expanded.
const SCRIPT_SIMS: &str = "crates/lgwks-bot/tests/it/sim_script.rs";

/// One written form of a word and what it means.
pub(crate) struct Form {
    /// The form as an author writes it.
    pub(crate) written: &'static str,
    /// What it does, in one clause.
    pub(crate) means: &'static str,
}

/// One word of the language: the six things SL-1 names, plus where it stands.
pub(crate) struct Word {
    /// What the word does; its spelling is [`Kind::spelling`].
    pub(crate) kind: Kind,
    /// Where it may stand.
    pub(crate) position: Position,
    /// The `lgwks_bot::script` primitive the word expands to a call of, as
    /// Markdown the README renders as written: code in backticks, and prose,
    /// such as a word that expands to plain Rust, as prose.
    pub(crate) primitive: &'static str,
    /// Its written forms. A word documented inside another word's form (`else`
    /// in `if`'s) has none of its own.
    pub(crate) forms: &'static [Form],
    /// What the word itself refuses, beyond the refusals every passthrough
    /// line carries (`refuse.rs`).
    pub(crate) refuses: &'static [&'static str],
    /// What holds wherever the word is used, each with its proof (SL-4).
    pub(crate) guarantees: &'static [Guarantee],
    /// A whole script using the word, which the tests expand.
    pub(crate) example: &'static str,
}

/// A written form and what it means.
const fn form(written: &'static str, means: &'static str) -> Form {
    Form { written, means }
}

/// `if` and `else` are one construct, so they share one example.
const IF_ELSE_EXAMPLE: &str = "flow route(code: u16) -> u8:\n    if code < 400:\n        give back 0\n    else if code < 500:\n        give back 1\n    else:\n        give back 2\n";

/// Every word, in the order the documentation lists them.
pub(crate) const LEXICON: [Word; 13] = [
    Word {
        kind: Kind::Flow,
        position: Position::Flow,
        primitive: "`Scope::enter`",
        forms: &[form(
            "`[pub] flow name(inputs) [-> Output]:`",
            "an `async fn` taking the tenant [`Scope`] first and returning `Result<Output, FlowError>`",
        )],
        refuses: &["a promised output the last line never produces"],
        guarantees: &[
            script_sim(
                Axis::MultiTenant,
                "every step keyed by tenant and path",
                "tenants_isolated",
            ),
            refused(
                Axis::Idiomatic,
                "the panicking calls refused by resolved path",
                "unwrap by path",
            ),
        ],
        example: "flow count(items: Vec<u8>) -> usize:\n    give back items.len()\n",
    },
    Word {
        kind: Kind::Each,
        position: Position::Block,
        primitive: "`each`",
        forms: &[
            form(
                "`each x in xs:`",
                "every item, as many at once as the machine sustains, results in input order, first failure stops the rest",
            ),
            form(
                "`each x in xs, at most (limit) at once:`",
                "the same, under a limit that comes from somewhere named (an upstream's quota)",
            ),
        ],
        refuses: &[
            "a concurrency bound typed as a number",
            "a bound outside `1..=65536`",
        ],
        guarantees: &[
            script_sim(Axis::Hyperscale, "bounded fan-out", "fan_bounded"),
            script_sim(
                Axis::Generalized,
                "results in input order",
                "fan_machine_sized",
            ),
            script_sim(
                Axis::Hyperscale,
                "first failure cancels the rest",
                "fan_fail_fast",
            ),
        ],
        example: "flow sizes(pages: Vec<String>) -> Vec<usize>:\n    let sizes = each page in pages:\n        page.len()\n    give back sizes\n",
    },
    Word {
        kind: Kind::Within,
        position: Position::Block,
        primitive: "`within`",
        forms: &[form(
            "`within 2s:`",
            "the block, or `TimedOut` when the deadline passes",
        )],
        refuses: &["a zero duration", "a duration with no unit"],
        guarantees: &[Guarantee {
            axis: Axis::Hyperscale,
            claim: "a deadline, on the scope's clock",
            evidence: Evidence::Sim {
                file: "crates/lgwks-bot/tests/it/sim_clock_wiring.rs",
                test: "a_within_budget_is_refused_by_an_advance_with_no_real_wait",
            },
        }],
        example: "flow ping(host: &Host) -> u16:\n    let code = within 2s:\n        host.ping().await.or_retry()?\n    give back code\n",
    },
    Word {
        kind: Kind::Retry,
        position: Position::Block,
        primitive: "`retry`",
        forms: &[form(
            "`retry up to 3 times[, waiting 100ms]:`",
            "the block again while it fails transiently, same key each attempt, within the run's retry budget",
        )],
        refuses: &["attempts outside `1..=1000`", "a zero wait"],
        guarantees: &[
            script_sim(
                Axis::Ephemeral,
                "one idempotency key across attempts",
                "retry_one_key",
            ),
            script_sim(
                Axis::Hyperscale,
                "attempts within the run's retry budget",
                "retry_budget_holds",
            ),
        ],
        example: "flow fetch(site: &Site) -> Page:\n    let page = retry up to 3 times, waiting 200ms:\n        site.get(scope.key()).await.or_retry()?\n    give back page\n",
    },
    Word {
        kind: Kind::Together,
        position: Position::Block,
        primitive: "`try_join!`",
        forms: &[form(
            "`together:`",
            "each line underneath concurrently; `let x = ..` lines bind their result",
        )],
        refuses: &[
            "anything before its `:`",
            "an indent under a line that opens no block",
        ],
        guarantees: &[
            script_sim(
                Axis::Ephemeral,
                "every branch owned by this task",
                "together_owned",
            ),
            script_sim(
                Axis::Hyperscale,
                "first failure cancels the rest",
                "together_fail_fast",
            ),
        ],
        example: "flow both(left: &Api, right: &Api):\n    together:\n        let up = run probe(left)\n        let down = run probe(right)\n",
    },
    Word {
        kind: Kind::Step,
        position: Position::Block,
        primitive: "`Scope::enter`",
        forms: &[form(
            "`step name:`",
            "a named scope: its own key and error location",
        )],
        refuses: &["a name that is not one word"],
        guarantees: &[
            script_sim(Axis::Ephemeral, "its own step key", "step_own_key"),
            script_sim(
                Axis::Decoupled,
                "failures located at the step",
                "step_failure_located",
            ),
        ],
        example: "flow deploy(target: &Host):\n    step upload:\n        target.upload().await.or_fail()?\n",
    },
    Word {
        kind: Kind::For,
        position: Position::Block,
        primitive: "`Scope::item`",
        forms: &[form(
            "`for x in xs:`",
            "every item in turn, each in its own scope",
        )],
        refuses: &["a missing pattern or missing items"],
        guarantees: &[
            script_sim(
                Axis::Generalized,
                "items in order, one at a time",
                "for_in_order",
            ),
            script_sim(Axis::Ephemeral, "one scope per item", "for_scope_per_item"),
        ],
        example: "flow notify(users: Vec<User>):\n    for user in users:\n        run email(user)\n",
    },
    Word {
        kind: Kind::If,
        position: Position::Block,
        primitive: "none: Rust `if`",
        forms: &[form("`if cond:` / `else if cond:` / `else:`", "as written")],
        refuses: &["a missing condition"],
        guarantees: &[refused(
            Axis::Idiomatic,
            "a value only when an `else:` ends the chain",
            "if with no else as the value",
        )],
        example: IF_ELSE_EXAMPLE,
    },
    Word {
        kind: Kind::Else,
        position: Position::Block,
        primitive: "none: Rust `else`",
        forms: &[],
        refuses: &[
            "an `else` not after an `if`",
            "an `else:` that is not the last branch",
        ],
        guarantees: &[refused(
            Axis::Idiomatic,
            "only ever continues an `if` chain",
            "else without if",
        )],
        example: IF_ELSE_EXAMPLE,
    },
    Word {
        kind: Kind::Let,
        position: Position::Block,
        primitive: "the block's own",
        forms: &[form(
            "`let x = <block>:`",
            "the block's last line becomes `x`",
        )],
        refuses: &["a `let` with no `=` or nothing after it"],
        guarantees: &[script_sim(
            Axis::Idiomatic,
            "the block's failure propagates before `x` is bound",
            "let_fails_before_binding",
        )],
        example: "flow total(xs: Vec<u8>) -> u8:\n    let sum = within 1s:\n        xs.iter().sum()\n    give back sum\n",
    },
    Word {
        kind: Kind::Run,
        position: Position::Inline,
        primitive: "the callee flow",
        forms: &[form(
            "`run other(args)`",
            "call another flow in this scope, await it, propagate its failure",
        )],
        refuses: &[],
        guarantees: &[script_sim(
            Axis::MultiTenant,
            "the callee runs in this tenant's scope",
            "run_in_callers_tenant",
        )],
        example: "flow outer(site: &Site) -> usize:\n    let pages = run crawl(site)\n    give back pages\n",
    },
    Word {
        kind: Kind::GiveBack,
        position: Position::Line,
        primitive: "none: `return Ok`",
        forms: &[form("`give back value`", "return from the flow")],
        refuses: &["`give back` inside a block that has its own value"],
        guarantees: &[refused(
            Axis::Idiomatic,
            "leaves the flow, never a nested block",
            "give back from a nested block",
        )],
        example: "flow one() -> u8:\n    give back 1\n",
    },
    Word {
        kind: Kind::Fail,
        position: Position::Line,
        primitive: "`FlowError::failed` / `FlowError::transient`",
        forms: &[form(
            "`fail with reason` / `fail transiently with reason`",
            "stop with a permanent / retryable failure",
        )],
        refuses: &["a reason missing after `with`"],
        guarantees: &[script_sim(
            Axis::Decoupled,
            "a typed failure the caller can classify",
            "fail_is_classified",
        )],
        example: "flow check(ready: bool):\n    if !ready:\n        fail with \"not ready\"\n",
    },
];

/// The row `token` names, when it is a word.
///
/// The identifier is rendered once and compared with each spelling, rather
/// than asked of each row, so a lookup costs one rendering however long the
/// table grows.
pub(crate) fn word(token: &TokenTree) -> Option<&'static Word> {
    let name = ident(token)?.to_string();
    LEXICON.iter().find(|row| row.kind.spelling() == name)
}

/// Whether `token` is the word `kind`.
pub(crate) fn is(token: &TokenTree, kind: Kind) -> bool {
    word(token).is_some_and(|row| row.kind == kind)
}

/// The word a line starts with, if it starts with one.
pub(crate) fn kind_of(line: &Line) -> Option<Kind> {
    line.tokens.first().and_then(word).map(|row| row.kind)
}

/// The block words, as the refusal for an unknown block lists them.
pub(crate) fn block_words() -> String {
    let words: Vec<String> = LEXICON
        .iter()
        .filter(|row| row.position == Position::Block)
        .map(|row| format!("`{}`", row.kind.spelling()))
        .collect();
    words.join(", ")
}

/// The crate documentation's block table, as `//!` lines.
pub(crate) fn doc_table() -> String {
    let rows = LEXICON
        .iter()
        .flat_map(|row| row.forms)
        .map(|form| format!("//! | {} | {} |\n", form.written, form.means));
    [
        "//! | Written | Means |\n".to_owned(),
        "//! |---|---|\n".to_owned(),
    ]
    .into_iter()
    .chain(rows)
    .collect()
}

/// The README's word table: every row, with the columns the crate
/// documentation leaves to the README.
pub(crate) fn readme_table() -> String {
    let rows = LEXICON.iter().map(|row| {
        let written: Vec<&str> = row.forms.iter().map(|form| form.written).collect();
        let means: Vec<String> = row.forms.iter().map(|form| plain(form.means)).collect();
        let written = if written.is_empty() {
            format!("`{}` (see `if`)", row.kind.spelling())
        } else {
            written.join("<br>")
        };
        format!(
            "| {written} | {} | {} | {} | {} | {} |\n",
            row.position.name(),
            row.primitive,
            cell(&means.join("<br>")),
            cell(&row.refuses.join("; ")),
            cell(&guarantees(row)),
        )
    });
    [
        "| Written | Where | Calls | Means | Refuses | Guarantees |\n".to_owned(),
        "|---|---|---|---|---|---|\n".to_owned(),
    ]
    .into_iter()
    .chain(rows)
    .collect()
}

/// A row's guarantees as one README cell: each claim with its axis and proof.
fn guarantees(row: &Word) -> String {
    let cited: Vec<String> = row
        .guarantees
        .iter()
        .map(|guarantee| {
            format!(
                "{} ({}; {})",
                guarantee.claim,
                guarantee.axis.name(),
                guarantee.evidence.cite()
            )
        })
        .collect();
    cited.join("; ")
}

/// An intra-doc link rendered as the code it names, for a page rustdoc does
/// not resolve.
fn plain(text: &str) -> String {
    text.replace("[`", "`").replace("`]", "`")
}

/// An empty cell written as a dash, so a reader sees "nothing" rather than a
/// gap.
fn cell(text: &str) -> &str {
    if text.is_empty() { "—" } else { text }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{Evidence, LEXICON, doc_table, readme_table};
    use crate::tests::{REFUSALS, expand};

    /// The crate documentation's block table is the one the lexicon renders.
    #[test]
    fn the_crate_doc_table_is_rendered_from_the_lexicon() {
        let table = doc_table();
        assert!(
            include_str!("lib.rs").contains(&table),
            "src/lib.rs must carry the lexicon's block table verbatim:\n{table}"
        );
    }

    /// The README's word table is the one the lexicon renders.
    #[test]
    fn the_readme_table_is_rendered_from_the_lexicon() {
        let table = readme_table();
        assert!(
            include_str!("../README.md").contains(&table),
            "README.md must carry the lexicon's word table verbatim:\n{table}"
        );
    }

    /// One row per spelling: two rows for one token would make dispatch
    /// depend on table order.
    #[test]
    fn every_spelling_has_exactly_one_row() {
        let mut seen = HashSet::new();
        for row in &LEXICON {
            assert!(
                seen.insert(row.kind.spelling()),
                "`{}` has two rows",
                row.kind.spelling()
            );
        }
    }

    /// Every row's example is a script the macro expands, and it uses the
    /// word it illustrates.
    #[test]
    fn every_example_expands_and_uses_its_word() -> Result<(), String> {
        for row in &LEXICON {
            let spelling = row.kind.spelling();
            let uses_word = row
                .example
                .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .any(|token| token == spelling);
            assert!(uses_word, "the example for `{spelling}` never says it");
            expand(row.example)
                .map_err(|refusal| format!("the example for `{spelling}` is refused: {refusal}"))?;
        }
        Ok(())
    }

    /// Every word guarantees something, and every guarantee names a test that
    /// exists: a compile-fail case in `REFUSALS`, or a `#[test]` or swept
    /// family in a `sim_` file. Renaming or deleting the test fails this, so a
    /// guarantee cannot outlive its proof. The tests themselves run in the same
    /// gate, so existing is what is left to check here.
    #[test]
    fn every_guarantee_names_a_test_that_exists() -> Result<(), String> {
        for row in &LEXICON {
            let spelling = row.kind.spelling();
            if row.guarantees.is_empty() {
                return Err(format!("`{spelling}` guarantees nothing"));
            }
            for guarantee in row.guarantees {
                resolve(&guarantee.evidence)
                    .map_err(|why| format!("`{spelling}` ({}): {why}", guarantee.claim))?;
            }
        }
        Ok(())
    }

    /// Find the test `evidence` names.
    fn resolve(evidence: &Evidence) -> Result<(), String> {
        match *evidence {
            Evidence::Refusal(case) => {
                if REFUSALS.iter().any(|&(name, _, _)| name == case) {
                    Ok(())
                } else {
                    Err(format!("no compile-fail case named {case:?}"))
                }
            }
            Evidence::Sim { file, test } => {
                let is_sim = file
                    .rsplit_once('/')
                    .is_some_and(|(_, name)| name.starts_with("sim_"));
                if !is_sim {
                    return Err(format!("{file} is not a simulation file"));
                }
                let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
                let source = std::fs::read_to_string(root.join(file))
                    .map_err(|error| format!("{file}: {error}"))?;
                if defines_test(&source, test) {
                    Ok(())
                } else {
                    Err(format!("{file} has no test or swept family `{test}`"))
                }
            }
        }
    }

    /// Whether `source` declares `test` as a `#[test]` function, or defines it
    /// as a family a `band_family!` sweeps.
    fn defines_test(source: &str, test: &str) -> bool {
        let header = format!("fn {test}(");
        let lines: Vec<&str> = source.lines().map(str::trim_start).collect();
        let Some(at) = lines.iter().position(|line| line.starts_with(&header)) else {
            return false;
        };
        let attributed = lines
            .iter()
            .take(at)
            .rev()
            .take_while(|line| line.starts_with("#[") || line.starts_with("///"))
            .any(|line| line.starts_with("#[test]"));
        attributed || source.contains(&format!("=> {test}, "))
    }

    /// The resolver is not vacuous: a name with no test behind it, a swept
    /// family that is not swept, and a case not in the table are all refused.
    #[test]
    fn a_guarantee_whose_test_is_gone_is_refused() {
        let sweeps =
            "fn fan(band: Band) -> TestResult {}\nband_family! { fan_band_00 => fan, 0; }\n";
        assert!(defines_test(sweeps, "fan"), "a swept family resolves");
        assert!(
            !defines_test(sweeps, "fan_gone"),
            "a missing family does not"
        );
        let unswept = "fn helper(band: Band) -> TestResult {}\n";
        assert!(
            !defines_test(unswept, "helper"),
            "an unswept helper does not"
        );
        let attributed = "/// Doc.\n#[test]\nfn proves() {}\n";
        assert!(defines_test(attributed, "proves"), "a `#[test]` resolves");
        assert!(
            resolve(&Evidence::Refusal("no such case")).is_err(),
            "an unknown compile-fail case does not"
        );
        assert!(
            resolve(&Evidence::Sim {
                file: "crates/lgwks-bot/tests/it/script_flow.rs",
                test: "a_deadline_that_passes_fails_the_step_as_timed_out",
            })
            .is_err(),
            "a test outside a simulation file is not evidence"
        );
    }
}
