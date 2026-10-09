//! The pages generated from the lexicon, checked against the lexicon.
//!
//! The lexicon itself lives with the parser in `lgwks_ast::script` (#383); the
//! crate documentation and the README that describe the language live here,
//! beside the macro a reader reaches them through. Each table is rendered from
//! [`LEXICON`] and must appear verbatim, and every guarantee a word makes must
//! name a test that exists.

use std::collections::HashSet;
use std::path::PathBuf;

use lgwks_ast::script::{Evidence, Kind, LEXICON, Position, Word};

use crate::tests::{REFUSALS, expand};

/// The workspace root, which every path a test names is relative to.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The crate documentation's block table, as `//!` lines.
fn doc_table() -> String {
    let rows = LEXICON
        .iter()
        .flat_map(Word::forms)
        .map(|form| format!("//! | {} | {} |\n", form.written(), form.means()));
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
fn readme_table() -> Result<String, String> {
    let mut lines = Vec::with_capacity(LEXICON.len());
    for row in &LEXICON {
        let written: Vec<&str> = row.forms().iter().map(|form| form.written()).collect();
        let means: Vec<String> = row.forms().iter().map(|form| plain(form.means())).collect();
        let written = if written.is_empty() {
            format!("`{}` (see `if`)", row.kind().spelling())
        } else {
            written.join("<br>")
        };
        lines.push(format!(
            "| {written} | {} | {} | {} | {} | {} |\n",
            row.position().name(),
            row.primitive(),
            cell(&means.join("<br>")),
            cell(&row.refuses().join("; ")),
            cell(&guarantees(row)?),
        ));
    }
    Ok(format!(
        "| Written | Where | Calls | Means | Refuses | Guarantees |\n|---|---|---|---|---|---|\n{}",
        lines.concat()
    ))
}

/// A row's guarantees as one README cell: each claim with its axis and proof.
fn guarantees(row: &Word) -> Result<String, String> {
    let mut cited = Vec::with_capacity(row.guarantees().len());
    for guarantee in row.guarantees() {
        cited.push(format!(
            "{} ({}; {})",
            guarantee.claim(),
            guarantee.axis().name(),
            cite(guarantee.evidence())?
        ));
    }
    Ok(cited.join("; "))
}

/// The evidence as the README cites it.
fn cite(evidence: &Evidence) -> Result<String, String> {
    match *evidence {
        Evidence::Refusal(case) => Ok(format!("refusal *{case}*")),
        // A path with no directory is already the file's own name.
        Evidence::Sim { file, test } => Ok(match file.rsplit_once('/') {
            Some((_, name)) => format!("`{test}` in `{name}`"),
            None => format!("`{test}` in `{file}`"),
        }),
        // `Evidence` is `#[non_exhaustive]`: a kind of proof a newer
        // `lgwks_ast` adds has no citation form here until one is written.
        _ => Err(format!("no citation form for {evidence:?}")),
    }
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
fn the_readme_table_is_rendered_from_the_lexicon() -> Result<(), String> {
    let table = readme_table()?;
    assert!(
        include_str!("../README.md").contains(&table),
        "README.md must carry the lexicon's word table verbatim:\n{table}"
    );
    Ok(())
}

/// One row per spelling: two rows for one token would make dispatch depend on
/// table order.
#[test]
fn every_spelling_has_exactly_one_row() {
    let mut seen = HashSet::new();
    for row in &LEXICON {
        assert!(
            seen.insert(row.kind().spelling()),
            "`{}` has two rows",
            row.kind().spelling()
        );
    }
}

/// Every row's example is a script the macro expands, and it uses the word it
/// illustrates.
#[test]
fn every_example_expands_and_uses_its_word() -> Result<(), String> {
    for row in &LEXICON {
        let spelling = row.kind().spelling();
        let uses_word = row
            .example()
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .any(|token| token == spelling);
        assert!(uses_word, "the example for `{spelling}` never says it");
        expand(row.example())
            .map_err(|refusal| format!("the example for `{spelling}` is refused: {refusal}"))?;
    }
    Ok(())
}

/// Every word guarantees something, and every guarantee names a test that
/// exists: a compile-fail case in `REFUSALS`, or a `#[test]` or swept family in
/// a `sim_` file. Renaming or deleting the test fails this, so a guarantee
/// cannot outlive its proof. The tests themselves run in the same gate, so
/// existing is what is left to check here.
#[test]
fn every_guarantee_names_a_test_that_exists() -> Result<(), String> {
    for row in &LEXICON {
        let spelling = row.kind().spelling();
        if row.guarantees().is_empty() {
            return Err(format!("`{spelling}` guarantees nothing"));
        }
        for guarantee in row.guarantees() {
            resolve(guarantee.evidence())
                .map_err(|why| format!("`{spelling}` ({}): {why}", guarantee.claim()))?;
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
                let refusal = format!("{file} is not a simulation file");
                lgwks_std::trace::debug!(error = %refusal, "resolve: returning an error to the caller");
                return Err(refusal);
            }
            let source = std::fs::read_to_string(workspace_root().join(file))
                .map_err(|error| format!("{file}: {error}"))?;
            if defines_test(&source, test) {
                Ok(())
            } else {
                Err(format!("{file} has no test or swept family `{test}`"))
            }
        }
        _ => Err(format!("no resolver for {evidence:?}")),
    }
}

/// Whether `source` declares `test` as a `#[test]` function, or defines it as
/// a family a `band_family!` sweeps.
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

/// The resolver is not vacuous: a name with no test behind it, a swept family
/// that is not swept, and a case not in the table are all refused.
#[test]
fn a_guarantee_whose_test_is_gone_is_refused() {
    let sweeps = "fn fan(band: Band) -> TestResult {}\nband_family! { fan_band_00 => fan, 0; }\n";
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

/// Where the one-page lexicon is committed, from the workspace root.
const PAGE: &str = "docs/script-lexicon.md";

/// The page's budget: what an agent can hold whole beside its task.
const PAGE_TOKENS: usize = 4_000;

/// The variable that makes the page test write the page it renders.
const WRITE_PAGE: &str = "LGWKS_WRITE_LEXICON_PAGE";

/// The one-page lexicon (#385), an agent's whole context for writing a script:
/// each word's forms, what it refuses, what it guarantees and an example,
/// rendered from [`LEXICON`] so it cannot say what the parser does not do.
/// Proofs are left to the README's table; the page carries what an author
/// needs, inside its token budget.
fn page() -> Result<String, String> {
    let wrapped: String = LEXICON
        .first()
        .map(Word::example)
        .into_iter()
        .flat_map(str::lines)
        .map(|line| format!("    {line}\n"))
        .collect();
    let mut page = format!(
        "# The `script!` lexicon\n\n\
         <!-- Generated from `LEXICON` in crates/lgwks-ast/src/script/lexicon.rs by the \
         test `the_lexicon_page_is_rendered_from_the_lexicon` in lgwks_macros. Edit the \
         table, not this page. -->\n\n\
         Everything needed to write `lgwks_bot::script!` flows. A script is one or \
         more `flow`s inside the macro:\n\n\
         ```rust\nlgwks_bot::script! {{\n{wrapped}}}\n```\n\n\
         Each line is one step: it starts with one of the words below, or it is a \
         line of Rust. A line ending in `:` opens a block of the lines indented four \
         spaces beneath it. Inside a flow, `scope` is the current step's scope. A \
         line of Rust that could end or leak the program is refused when the macro \
         expands, and the refusal names what to write instead.\n"
    );
    for row in &LEXICON {
        page.push_str(&section(row)?);
    }
    Ok(page)
}

/// One word's section of the page.
fn section(row: &Word) -> Result<String, String> {
    let forms: String = row
        .forms()
        .iter()
        .map(|form| format!("- {} — {}\n", form.written(), plain(form.means())))
        .collect();
    let forms = if forms.is_empty() {
        "Written as part of `if`'s forms.\n".to_owned()
    } else {
        forms
    };
    let refuses = if row.refuses().is_empty() {
        "nothing of its own".to_owned()
    } else {
        row.refuses().join("; ")
    };
    let guarantees: Vec<String> = row
        .guarantees()
        .iter()
        .map(|guarantee| format!("{} ({})", guarantee.claim(), guarantee.axis().name()))
        .collect();
    let example = if row.kind() == Kind::Else {
        String::new()
    } else {
        format!("\n```text\n{}```\n", row.example())
    };
    Ok(format!(
        "\n## `{}`\n\n{}. Calls {}.\n\n{forms}\nRefuses: {refuses}.\n\nGuarantees: {}.\n{example}",
        heading(row.kind()),
        stands(row.position())?,
        row.primitive(),
        guarantees.join("; "),
    ))
}

/// The word as an author says it: `give back` is two tokens.
fn heading(kind: Kind) -> &'static str {
    if kind == Kind::GiveBack {
        "give back"
    } else {
        kind.spelling()
    }
}

/// Where a word stands, as a sentence.
fn stands(position: Position) -> Result<&'static str, String> {
    match position {
        Position::Flow => Ok("Opens a flow at the top level"),
        Position::Block => Ok("Opens a block: its line ends in `:`"),
        Position::Line => Ok("Starts a line of its own"),
        Position::Inline => Ok("Stands inside a line of Rust"),
        // `Position` is `#[non_exhaustive]`: a place a newer `lgwks_ast` adds
        // has no sentence here until one is written.
        _ => Err(format!("no sentence for {position:?}")),
    }
}

/// The committed page is the one the table renders, and it fits the budget.
/// Tokens are counted as bytes divided by four, rounded up: a tokenizer-free
/// bound that overcounts English prose, so a page under it is under it for any
/// common tokenizer. Set `LGWKS_WRITE_LEXICON_PAGE` to write the rendered page
/// instead of failing on a stale one.
#[test]
fn the_lexicon_page_is_rendered_from_the_lexicon() -> Result<(), String> {
    let rendered = page()?;
    let path = workspace_root().join(PAGE);
    if std::env::var_os(WRITE_PAGE).is_some() {
        std::fs::write(&path, &rendered).map_err(|error| format!("{PAGE}: {error}"))?;
    }
    let committed = std::fs::read_to_string(&path).map_err(|error| format!("{PAGE}: {error}"))?;
    if committed != rendered {
        return Err(format!(
            "{PAGE} is generated from LEXICON and differs from it; rerun this test with \
             {WRITE_PAGE}=1 and commit the page"
        ));
    }
    let tokens = rendered.len().div_ceil(4);
    if tokens > PAGE_TOKENS {
        return Err(format!(
            "{PAGE} is {tokens} tokens (bytes / 4, rounded up), past its {PAGE_TOKENS}"
        ));
    }
    Ok(())
}
